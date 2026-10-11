#[cfg(test)]
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use anyhow::Context as _;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use uuid::Uuid;

#[path = "input_writer.rs"]
mod input_writer;
pub(crate) use input_writer::{InputWriter, MAX_INPUT_BYTES};

#[derive(Clone)]
enum TerminalWriter {
    Local(InputWriter),
    #[cfg(all(unix, feature = "daemon"))]
    Remote(RemoteLink),
}

impl TerminalWriter {
    fn check_open(&self) -> std::io::Result<()> {
        match self {
            Self::Local(writer) => writer.check_open(),
            #[cfg(all(unix, feature = "daemon"))]
            Self::Remote(link) => link.check_input_open(),
        }
    }

    fn enqueue(&self, bytes: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Local(writer) => writer.enqueue(bytes),
            #[cfg(all(unix, feature = "daemon"))]
            Self::Remote(link) => link.write_input(bytes),
        }
    }
    fn error(&self) -> Option<String> {
        match self {
            Self::Local(writer) => writer.error(),
            #[cfg(all(unix, feature = "daemon"))]
            Self::Remote(link) => link.input_error(),
        }
    }
    fn close(&self) {
        match self {
            Self::Local(writer) => writer.close(),
            #[cfg(all(unix, feature = "daemon"))]
            Self::Remote(link) => link.disconnect(),
        }
    }
    fn record_error(&self, message: String) {
        match self {
            Self::Local(writer) => writer.record_error(message),
            #[cfg(all(unix, feature = "daemon"))]
            Self::Remote(link) => link.record_input_error(message),
        }
    }
}

fn next_log_sequence(counter: &AtomicU64) -> Option<u64> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .ok()
        .and_then(|previous| previous.checked_add(1))
}

/// Drain whole frames, allowing one oversized frame so a byte budget can
/// never prevent forward progress. A zero budget still drains nothing.
fn retain_raw_read_and_parse<R>(
    term: &Mutex<Term<EventProxy>>,
    pending_log: &Mutex<Vec<u8>>,
    log_seq: &AtomicU64,
    raw: &[u8],
    parse: impl FnOnce(&mut Term<EventProxy>) -> R,
) -> std::io::Result<Option<R>> {
    // Keep the same term -> pending lock order as Attach/checkpoint. Physical
    // read bytes are retained before any fallible agent/VT parser sees them.
    let (mut term, healthy) = match term.lock() {
        Ok(term) => (term, true),
        Err(poison) => (poison.into_inner(), false),
    };
    let mut pending = pending_log
        .lock()
        .map_err(|_| std::io::Error::other("El log raw del PTY está envenenado."))?;
    let seq = next_log_sequence(log_seq)
        .ok_or_else(|| std::io::Error::other("Se agotó la secuencia raw del PTY."))?;
    pending.extend_from_slice(&crate::state::scrollback_log::encode_frame(
        seq,
        crate::state::scrollback_log::FrameKind::Output,
        raw,
    ));
    drop(pending);
    if healthy {
        Ok(Some(parse(&mut term)))
    } else {
        Ok(None)
    }
}

fn retain_raw_read_for_reader<R>(
    term: &Mutex<Term<EventProxy>>,
    pending: &Mutex<Vec<u8>>,
    sequence: &AtomicU64,
    raw: &[u8],
    parse: impl FnOnce(&mut Term<EventProxy>) -> R,
) -> std::io::Result<Option<R>> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        retain_raw_read_and_parse(term, pending, sequence, raw, parse)
    })) {
        Ok(result) => result,
        Err(_) => Ok(None), // the poisoned grid switches subsequent reads to raw only
    }
}

fn drain_log_prefix(pending: &mut Vec<u8>, max_bytes: usize) -> Vec<u8> {
    if max_bytes == 0 {
        return Vec::new();
    }
    let mut cut = 0usize;
    while pending.len().saturating_sub(cut) >= 13 {
        let payload_len =
            u32::from_le_bytes(pending[cut + 9..cut + 13].try_into().expect("frame header"))
                as usize;
        let Some(end) = cut
            .checked_add(13)
            .and_then(|offset| offset.checked_add(payload_len))
        else {
            break;
        };
        if end > pending.len() || (cut != 0 && end > max_bytes) {
            break;
        }
        cut = end;
        if cut >= max_bytes {
            break;
        }
    }
    pending.drain(..cut).collect()
}

use crate::runtime::SharedRuntimeScheduler;
use crate::terminal::agent_status::{AgentStatusReport, AgentStatusStream};
#[cfg(feature = "ghostty-vt")]
use crate::terminal::backend::{runtime_backend_from_env, TerminalBackendKind};
use crate::terminal::colors::indexed_to_egui;
#[cfg(feature = "ghostty-vt")]
use crate::terminal::ghostty_backend::{GhosttyRuntimeHandle, GhosttyTextSnapshot};
use crate::terminal::input::InputMode;
#[cfg(all(unix, feature = "daemon"))]
use crate::terminal::remote_session::RemoteLink;

#[derive(Clone)]
pub struct EventProxy {
    event_tx: mpsc::Sender<Event>,
}

impl EventProxy {
    pub fn new(event_tx: mpsc::Sender<Event>) -> Self {
        Self { event_tx }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let _ = self.event_tx.send(event);
    }
}

fn osc52_clipboard_enabled() -> bool {
    // El env tiene prioridad (override rápido); sin env manda la config.
    std::env::var("MI_TERMINAL_ALLOW_OSC52")
        .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or_else(|_| crate::config::runtime_config().allow_osc52)
}

fn pty_clock_epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Reloj monotónico del runtime (ms desde el arranque). Lo comparten el
/// lector PTY (última salida, reports de estado de agente) y la orquestación
/// (chequeo de frescura), así que debe ser el mismo reloj.
pub(crate) fn pty_clock_now_ms() -> i64 {
    Instant::now()
        .saturating_duration_since(pty_clock_epoch())
        .as_millis() as i64
}

fn wait_for_history_restore(restoring: &AtomicBool) {
    while restoring.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(test)]
type HistoryReplaySpawner =
    Box<dyn FnMut(Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()> + Send>;

fn yield_for_reader_priority(scheduler: &SharedRuntimeScheduler, session_id: Uuid) {
    let delay = scheduler
        .lock()
        .map(|scheduler| scheduler.reader_delay(session_id))
        .unwrap_or_default();
    if !delay.is_zero() {
        thread::sleep(delay);
    }
}

#[path = "reader_retirement.rs"]
mod reader_retirement;
use reader_retirement::{ReadCompletion, ReaderRetirement};
#[path = "child_lifetime.rs"]
mod child_lifetime;
use child_lifetime::ChildLifetime;

pub struct PtyHandle {
    pub term: Arc<Mutex<Term<EventProxy>>>,
    title: Arc<ArcSwap<String>>,
    pub alive: Arc<AtomicBool>,
    pub bell_fired: Arc<AtomicBool>,
    writer: TerminalWriter,
    last_output_at: Arc<AtomicI64>,
    /// Live input/output only; redraws and history replay are not activity.
    activity_revision: Arc<AtomicU64>,
    window_size: Arc<Mutex<WindowSize>>,
    render_revision: Arc<AtomicU64>,
    agent_status: Arc<ArcSwap<Option<AgentStatusReport>>>,
    /// Último cwd reportado por el shell vía OSC 7 (si el shell lo emite).
    cwd: Arc<ArcSwap<Option<String>>>,
    /// Frames de log incremental pendientes de appendear al disco (P1.7).
    /// El hilo lector y `resize` empujan frames; el autosave los confirma sólo
    /// después de que el worker durable responde.
    pending_log: Arc<Mutex<Vec<u8>>>,
    log_seq: Arc<AtomicU64>,
    restoring_history: Arc<AtomicBool>,
    /// A failed replay may already have changed a healthy grid. Keep it from
    /// replacing the durable checkpoint until a fresh session is created.
    history_restore_failed: Arc<AtomicBool>,
    #[cfg(test)]
    history_replay_spawner: Mutex<Option<HistoryReplaySpawner>>,
    scrollback_limit: usize,
    #[cfg(feature = "ghostty-vt")]
    backend_kind: TerminalBackendKind,
    #[cfg(feature = "ghostty-vt")]
    ghostty_runtime: Option<GhosttyRuntimeHandle>,
    /// `None` en una sesión hosteada por el daemon: el PTY real vive allá y
    /// el resize se manda por el socket (P3.15, T3).
    master: Option<Box<dyn MasterPty + Send>>,
    child_lifetime: Option<ChildLifetime>,
    process_reaped: Arc<AtomicBool>,
    master_closer: Option<thread::JoinHandle<()>>,
    /// Sesión del daemon a la que este handle está atado, si es remota.
    #[cfg(all(unix, feature = "daemon"))]
    remote: Option<RemoteLink>,
    #[cfg(all(unix, feature = "daemon"))]
    remote_exited: Arc<AtomicBool>,
    /// El daemon ya replayó el historial de una sesión preexistente. El store
    /// local no debe inyectar además el mismo checkpoint.
    hook_evidence: crate::hook_authority::HostEvidence,
    hook_local_owner: Option<Uuid>,
    hot_reattached: bool,
    reader_retirement: Arc<ReaderRetirement>,
    retirement_log_owned: AtomicBool,
    legacy_unowned_transfer: AtomicBool,
    semantic_output_ready: Arc<AtomicBool>,
    /// usize::MAX keeps ordinary local FlowGate policy; daemon ownership opts in.
    durable_pressure: Arc<std::sync::atomic::AtomicUsize>,
    _reader_thread: thread::JoinHandle<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalScrollState {
    pub display_offset: usize,
    pub visible_rows: usize,
    pub history_size: usize,
}

/// A restoring grid is incomplete. Its unacknowledged output can still be
/// made durable on top of the previous checkpoint without exporting that grid.
pub enum RecoverySnapshot<R> {
    Checkpoint { value: R, pending_bytes: usize },
    PendingLog(Vec<u8>),
    Unavailable,
}

/// Raw output plus alternate-screen state and an optional primary snapshot.
pub type PtyOutputUpdate = (Vec<u8>, bool, Option<String>);

impl PtyHandle {
    /// Runs the production read/retention and child-wait boundaries with injected
    /// endpoints. It creates no OS child, socket, PTY or graphical surface.
    #[cfg(test)]
    pub(crate) fn for_retirement_boundary_tests(
        mut reader: Box<dyn Read + Send>,
        child_events: mpsc::Sender<&'static str>,
        child_wait: mpsc::Receiver<std::io::Result<()>>,
    ) -> Self {
        let mut handle = Self::for_persistence_tests(b"");
        let control = Arc::new(ReaderRetirement::default());
        let runner = Arc::clone(&control);
        let term = Arc::clone(&handle.term);
        let pending = Arc::clone(&handle.pending_log);
        let sequence = Arc::clone(&handle.log_seq);
        handle.reader_retirement = control;
        handle._reader_thread = thread::spawn(move || {
            let mut bytes = [0; 64];
            loop {
                match runner.read_and_retain(&mut *reader, &mut bytes, |raw| {
                    retain_raw_read_and_parse(&term, &pending, &sequence, raw, |_| ()).map(|_| ())
                }) {
                    ReadCompletion::Data => {}
                    ReadCompletion::Retry | ReadCompletion::Paused => runner.wait_retry(),
                    ReadCompletion::Eof | ReadCompletion::Failed(_) => break,
                }
            }
        });
        let child = ChildLifetime::for_wait_boundary(child_events, child_wait);
        handle.process_reaped = Arc::clone(&child.reaped);
        handle.child_lifetime = Some(child);
        handle
    }

    /// In-memory terminal for persistence regressions. It starts no shell,
    /// socket, process watcher or graphical surface.
    #[cfg(test)]
    pub(crate) fn for_persistence_tests(output: &[u8]) -> Self {
        let (event_tx, _event_rx) = mpsc::channel();
        let config = TermConfig::default();
        let scrollback_limit = config.scrolling_history;
        let reader_retirement = Arc::new(ReaderRetirement::default());
        assert!(matches!(
            reader_retirement.read_and_retain(&mut std::io::empty(), &mut [0; 1], |_| Ok(())),
            ReadCompletion::Eof
        ));
        let handle = Self {
            term: Arc::new(Mutex::new(Term::new(
                config,
                &TermSize::new(80, 24),
                EventProxy::new(event_tx),
            ))),
            title: Arc::new(ArcSwap::from_pointee("Test terminal".to_owned())),
            alive: Arc::new(AtomicBool::new(true)),
            bell_fired: Arc::new(AtomicBool::new(false)),
            writer: TerminalWriter::Local(InputWriter::new(Box::new(std::io::sink())).unwrap()),
            last_output_at: Arc::new(AtomicI64::new(0)),
            activity_revision: Arc::new(AtomicU64::new(0)),
            window_size: Arc::new(Mutex::new(WindowSize {
                num_lines: 24,
                num_cols: 80,
                cell_width: 0,
                cell_height: 0,
            })),
            render_revision: Arc::new(AtomicU64::new(0)),
            agent_status: Arc::new(ArcSwap::from_pointee(None)),
            cwd: Arc::new(ArcSwap::from_pointee(None)),
            pending_log: Arc::new(Mutex::new(Vec::new())),
            log_seq: Arc::new(AtomicU64::new(0)),
            restoring_history: Arc::new(AtomicBool::new(false)),
            history_restore_failed: Arc::new(AtomicBool::new(false)),
            history_replay_spawner: Mutex::new(None),
            scrollback_limit,
            #[cfg(feature = "ghostty-vt")]
            backend_kind: TerminalBackendKind::Alacritty,
            #[cfg(feature = "ghostty-vt")]
            ghostty_runtime: None,
            master: None,
            child_lifetime: None,
            process_reaped: Arc::new(AtomicBool::new(true)),
            master_closer: None,
            #[cfg(all(unix, feature = "daemon"))]
            remote: None,
            #[cfg(all(unix, feature = "daemon"))]
            remote_exited: Arc::new(AtomicBool::new(false)),
            hook_evidence: crate::hook_authority::HostEvidence {
                runtime: Uuid::nil(),
                generation: None,
                target: None,
            },
            hook_local_owner: None,
            hot_reattached: false,
            reader_retirement,
            retirement_log_owned: AtomicBool::new(false),
            legacy_unowned_transfer: AtomicBool::new(false),
            semantic_output_ready: Arc::new(AtomicBool::new(false)),
            durable_pressure: Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX)),
            _reader_thread: thread::spawn(|| {}),
        };
        handle.feed_output_for_persistence_tests(output);
        handle
    }

    #[cfg(test)]
    pub(crate) fn bind_local_hook_for_retirement_tests(
        &mut self,
        claim: crate::hook_authority::Claim,
    ) {
        self.hook_evidence = crate::hook_authority::HostEvidence {
            runtime: claim.runtime,
            generation: claim.generation,
            target: Some(claim.key.into()),
        };
        self.hook_local_owner = claim.owner;
    }

    #[cfg(test)]
    pub(crate) fn feed_output_for_persistence_tests(&self, output: &[u8]) {
        if output.is_empty() {
            return;
        }
        self.semantic_output_ready.store(true, Ordering::Release);
        let mut term = self.term.lock().unwrap();
        let mut pending = self.pending_log.lock().unwrap();
        let mut parser = Processor::<StdSyncHandler>::new();
        parser.advance(&mut *term, output);
        let sequence = next_log_sequence(&self.log_seq).unwrap();
        pending.extend_from_slice(&crate::state::scrollback_log::encode_frame(
            sequence,
            crate::state::scrollback_log::FrameKind::Output,
            output,
        ));
        self.activity_revision.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn close_input_for_prompt_tests(&self) {
        self.writer.close();
    }

    #[cfg(test)]
    pub(crate) fn poison_pending_log_for_persistence_tests(&self) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _pending = self.pending_log.lock().unwrap();
            panic!("injected pending log poison");
        }));
        assert!(result.is_err());
    }

    /// Per-handle replay scheduling injection; tests can reject or hold a
    /// worker without exhausting threads or changing another test's behavior.
    #[cfg(test)]
    pub(crate) fn set_replay_spawner_for_persistence_tests(
        &mut self,
        spawner: impl FnMut(Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()> + Send + 'static,
    ) {
        *self.history_replay_spawner.get_mut().unwrap() = Some(Box::new(spawner));
    }

    pub fn spawn(
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
        session_id: Uuid,
        scheduler: SharedRuntimeScheduler,
        hooks: HookIdentity,
    ) -> anyhow::Result<Self> {
        Self::spawn_with_history(cwd, cols, rows, session_id, scheduler, hooks, &[], &[])
    }

    /// Replay durable history before the reader can publish any live output.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_history(
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
        session_id: Uuid,
        scheduler: SharedRuntimeScheduler,
        hooks: HookIdentity,
        checkpoint: &[u8],
        frames: &[crate::state::scrollback_log::Frame],
    ) -> anyhow::Result<Self> {
        let mut hooks = hooks;
        let endpoint = crate::hook_runtime::process_runtime();
        if hooks.hook_generation.is_none() {
            if let (Some(runtime), Some(key)) = (endpoint, hooks.key()) {
                let claim = runtime.authorize_birth(key, session_id)?;
                hooks.hook_generation = claim.generation;
                hooks.hook_owner = claim.owner;
            }
        }
        if let Some(generation) = hooks.hook_generation {
            let key = hooks
                .key()
                .ok_or_else(|| anyhow::anyhow!("bound birth target incomplete"))?;
            let owner = hooks
                .hook_owner
                .ok_or_else(|| anyhow::anyhow!("bound birth owner unavailable"))?;
            let store = crate::memory::MemoryStore::open_default()?;
            let claim = store
                .hook_claim(key)?
                .ok_or_else(|| anyhow::anyhow!("birth not preauthorized"))?;
            anyhow::ensure!(
                claim.runtime == session_id
                    && claim.generation == Some(generation)
                    && claim.owner == Some(owner),
                "birth authority mismatch"
            );
            let permit = store
                .hook_consume_birth_once(claim)?
                .ok_or_else(|| anyhow::anyhow!("birth already consumed"))?;
            let mut attempt = SpawnAttempt::managed(claim);
            let result = permit.run_guarded(&store, |_| {
                attempt.entered = true;
                Self::spawn_with_history_unfenced(
                    cwd,
                    cols,
                    rows,
                    session_id,
                    scheduler,
                    hooks,
                    checkpoint,
                    frames,
                    &mut attempt,
                )
            });
            let handle = match result {
                Ok(handle) => handle,
                Err(error) => {
                    if let (Some(runtime), Some(proof)) = (
                        endpoint.filter(|runtime| runtime.owner == owner),
                        attempt.no_child_proof(),
                    ) {
                        // This proof is created only by an entered, consumed
                        // factory that never called the physical spawn method.
                        let _ = runtime.confirm_no_child_birth(proof);
                    }
                    return Err(error);
                }
            };
            if let Some(runtime) = endpoint.filter(|runtime| runtime.owner == owner) {
                if let Err(error) = runtime.attach(key, handle.hook_host_evidence(), false) {
                    // Keep the actual child and its raw output alive until the
                    // coordinator obtains a fresh owner CAS and durable drain.
                    runtime.request_local_close_from_factory(Arc::new(Mutex::new(handle)));
                    return Err(error);
                }
            }
            return Ok(handle);
        }
        Self::spawn_with_history_unfenced(
            cwd,
            cols,
            rows,
            session_id,
            scheduler,
            hooks,
            checkpoint,
            frames,
            &mut SpawnAttempt::unmanaged(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_with_history_unfenced(
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
        session_id: Uuid,
        scheduler: SharedRuntimeScheduler,
        hooks: HookIdentity,
        checkpoint: &[u8],
        frames: &[crate::state::scrollback_log::Frame],
        attempt: &mut SpawnAttempt,
    ) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = shell_command(cwd, hooks);
        if let Some(generation) = hooks.hook_generation {
            cmd.env("TC_RUNTIME_ID", session_id.to_string());
            cmd.env("TC_HOOK_GENERATION", generation.to_string());
        }
        cmd.env(
            "TC_MEMORY_TASK_ID",
            hooks.memory_task_id(session_id).to_string(),
        );

        reader_retirement::prepare_nonblocking(&*pair.master)
            .context("prepare cancellable PTY reader")?;
        let mut reader = pair.master.try_clone_reader().context("clone PTY reader")?;
        let writer = pair.master.take_writer().context("take PTY writer")?;

        let title = Arc::new(ArcSwap::from_pointee("Terminal".to_owned()));
        let alive = Arc::new(AtomicBool::new(true));
        let bell_fired = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicI64::new(pty_clock_now_ms()));
        let activity_revision = Arc::new(AtomicU64::new(0));
        let window_size = Arc::new(Mutex::new(WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: 0,
            cell_height: 0,
        }));
        let render_revision = Arc::new(AtomicU64::new(0));
        let agent_status = Arc::new(ArcSwap::from_pointee(None));
        let cwd = Arc::new(ArcSwap::from_pointee(None));
        let pending_log = Arc::new(Mutex::new(Vec::<u8>::new()));
        let log_seq = Arc::new(AtomicU64::new(0));
        let restoring_history = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let term_config = TermConfig {
            scrolling_history: crate::config::runtime_config().scrollback_lines,
            ..TermConfig::default()
        };
        let scrollback_limit = term_config.scrolling_history;
        let term = Arc::new(Mutex::new(Term::new(
            term_config,
            &TermSize::new(cols as usize, rows as usize),
            EventProxy::new(event_tx),
        )));
        if !checkpoint.is_empty() || !frames.is_empty() {
            restore_cold_history(
                &mut term.lock().expect("new terminal lock"),
                checkpoint,
                frames,
                cols,
                rows,
            );
            // Replayed queries belong to the old process, never the new child.
            while event_rx.try_recv().is_ok() {}
        }
        #[cfg(feature = "ghostty-vt")]
        let requested_backend = runtime_backend_from_env();
        #[cfg(feature = "ghostty-vt")]
        let ghostty_runtime = if requested_backend == TerminalBackendKind::Ghostty {
            match GhosttyRuntimeHandle::spawn(cols, rows, scrollback_limit) {
                Ok(runtime) => Some(runtime),
                Err(err) => {
                    log::warn!("failed to start ghostty backend, falling back to alacritty: {err}");
                    None
                }
            }
        } else {
            None
        };
        #[cfg(feature = "ghostty-vt")]
        let backend_kind =
            if requested_backend == TerminalBackendKind::Ghostty && ghostty_runtime.is_some() {
                TerminalBackendKind::Ghostty
            } else {
                TerminalBackendKind::Alacritty
            };

        let title_for_reader = Arc::clone(&title);
        let alive_for_reader = Arc::clone(&alive);
        let bell_for_reader = Arc::clone(&bell_fired);
        let writer_for_reader =
            TerminalWriter::Local(InputWriter::new(writer).context("start PTY input writer")?);
        let writer_for_thread = writer_for_reader.clone();
        let output_for_reader = Arc::clone(&last_output_at);
        let activity_for_reader = Arc::clone(&activity_revision);
        let term_for_reader = Arc::clone(&term);
        #[cfg(feature = "ghostty-vt")]
        let ghostty_for_reader = ghostty_runtime.clone();
        let window_size_for_reader = Arc::clone(&window_size);
        let render_revision_for_reader = Arc::clone(&render_revision);
        let agent_status_for_reader = Arc::clone(&agent_status);
        let cwd_for_reader = Arc::clone(&cwd);
        let pending_log_for_reader = Arc::clone(&pending_log);
        let log_seq_for_reader = Arc::clone(&log_seq);
        let restoring_for_reader = Arc::clone(&restoring_history);
        let scheduler_for_reader = Arc::clone(&scheduler);
        let durable_pressure = Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        let semantic_output_ready = Arc::new(AtomicBool::new(false));
        let pressure_for_reader = Arc::clone(&durable_pressure);
        let semantic_ready_for_reader = Arc::clone(&semantic_output_ready);
        let reader_retirement = Arc::new(ReaderRetirement::default());
        let retirement_for_reader = Arc::clone(&reader_retirement);
        let alive_for_waiter = Arc::clone(&alive);
        let scheduler_for_waiter = Arc::clone(&scheduler);
        let child_lifetime = ChildLifetime::spawn_with(
            || {
                // A failed spawn_command may still be physically uncertain.
                // Mark before the call; its Result never proves absence.
                attempt.child_attempted = true;
                pair.slave.spawn_command(cmd)
            },
            move || {
                alive_for_waiter.store(false, Ordering::Relaxed);
                if let Ok(mut scheduler) = scheduler_for_waiter.lock() {
                    scheduler.record_exit(session_id);
                }
            },
        )
        .context("start PTY child lifetime owner")?;
        let process_reaped = Arc::clone(&child_lifetime.reaped);
        let reader_thread = thread::spawn(move || {
            // The parser processes untrusted terminal output; if it ever
            // panics, contain the damage to this session instead of taking
            // down the whole app, and leave the session marked as exited.
            let loop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut buf = vec![0_u8; 65_536];
                let mut processor = Processor::<StdSyncHandler>::new();
                let mut agent_stream = AgentStatusStream::new();
                // Flow control (P3.16): si el buffer pendiente de log crece
                // más rápido de lo que la app lo drena, dejamos de leer del fd
                // y el kernel bloquea al hijo.
                let mut gate = crate::terminal::flow_control::FlowGate::new();
                loop {
                    if retirement_for_reader.failed() || retirement_for_reader.eof() {
                        break;
                    }
                    if retirement_for_reader.quiescent() {
                        retirement_for_reader.wait_retry();
                        continue;
                    }
                    wait_for_history_restore(&restoring_for_reader);
                    yield_for_reader_priority(&scheduler_for_reader, session_id);
                    let pending_len = pending_log_for_reader
                        .lock()
                        .map(|pending| pending.len())
                        .unwrap_or(0);
                    let external = pressure_for_reader.load(Ordering::Acquire);
                    // No timer escape from durable rejection on daemon-owned PTYs.
                    // Pending-log transfer charges the external queue under the
                    // same log lock, so draining into RAM cannot reset this gate.
                    if external != usize::MAX
                        && external.saturating_add(pending_len)
                            >= crate::terminal::flow_control::HIGH_WATER
                    {
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    if matches!(
                        gate.update(pending_len, std::time::Instant::now()),
                        crate::terminal::flow_control::FlowState::Pause
                    ) {
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    let mut parsed = None;
                    let completion =
                        retirement_for_reader.read_and_retain(&mut *reader, &mut buf, |raw| {
                            wait_for_history_restore(&restoring_for_reader);
                            let now_ms = pty_clock_now_ms();
                            // A parser panic poisons the grid but not the raw stream.
                            // Continue physical reads and retain their exact bytes.
                            parsed = Some(retain_raw_read_for_reader(
                                &term_for_reader,
                                &pending_log_for_reader,
                                &log_seq_for_reader,
                                raw,
                                |term| {
                                    let parts = agent_stream.process(raw, now_ms);
                                    processor.advance(term, &parts.0);
                                    parts
                                },
                            )?);
                            Ok(())
                        });
                    match completion {
                        ReadCompletion::Eof => break,
                        ReadCompletion::Paused | ReadCompletion::Retry => {
                            retirement_for_reader.wait_retry();
                            continue;
                        }
                        ReadCompletion::Failed(error) => {
                            log::error!("PTY reader for session {session_id} failed: {error}");
                            break;
                        }
                        ReadCompletion::Data => {
                            let now_ms = pty_clock_now_ms();
                            let Some((clean, agent_reports, cwd_reports)) = parsed.flatten() else {
                                log::error!(
                                    "El grid no está disponible; el lector conserva sólo raw."
                                );
                                continue;
                            };
                            if !clean.is_empty() {
                                semantic_ready_for_reader.store(true, Ordering::Release);
                            }
                            if let Some(report) = agent_reports.into_iter().next_back() {
                                agent_status_for_reader.store(Arc::new(Some(report)));
                            }
                            if let Some(new_cwd) = cwd_reports.into_iter().next_back() {
                                cwd_for_reader.store(Arc::new(Some(new_cwd)));
                            }
                            #[cfg(feature = "ghostty-vt")]
                            if let Some(ghostty) = &ghostty_for_reader {
                                ghostty.feed(&clean);
                            }
                            if !clean.is_empty() {
                                render_revision_for_reader.fetch_add(1, Ordering::Relaxed);
                                output_for_reader.store(now_ms, Ordering::Relaxed);
                                activity_for_reader.fetch_add(1, Ordering::Relaxed);
                                if let Ok(mut scheduler) = scheduler_for_reader.lock() {
                                    scheduler.record_output(session_id);
                                }
                            }
                            drain_terminal_events(
                                &event_rx,
                                &writer_for_thread,
                                &title_for_reader,
                                &alive_for_reader,
                                &bell_for_reader,
                                &window_size_for_reader,
                                &scheduler_for_reader,
                                session_id,
                            );
                        }
                    }
                }
            }));
            if loop_result.is_err() {
                retirement_for_reader.fail();
                log::error!("PTY reader thread for session {session_id} panicked");
            }
            alive_for_reader.store(false, Ordering::Relaxed);
            if let Ok(mut scheduler) = scheduler_for_reader.lock() {
                scheduler.record_exit(session_id);
            }
            drain_terminal_events(
                &event_rx,
                &writer_for_thread,
                &title_for_reader,
                &alive_for_reader,
                &bell_for_reader,
                &window_size_for_reader,
                &scheduler_for_reader,
                session_id,
            );
        });

        Ok(Self {
            term,
            title,
            alive,
            bell_fired,
            writer: writer_for_reader,
            last_output_at,
            activity_revision,
            window_size,
            render_revision,
            agent_status,
            cwd,
            pending_log,
            log_seq,
            restoring_history,
            history_restore_failed: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            history_replay_spawner: Mutex::new(None),
            scrollback_limit,
            #[cfg(feature = "ghostty-vt")]
            backend_kind,
            #[cfg(feature = "ghostty-vt")]
            ghostty_runtime,
            master: Some(pair.master),
            child_lifetime: Some(child_lifetime),
            process_reaped,
            master_closer: None,
            #[cfg(all(unix, feature = "daemon"))]
            remote: None,
            #[cfg(all(unix, feature = "daemon"))]
            remote_exited: Arc::new(AtomicBool::new(false)),
            hook_evidence: hooks.evidence(session_id),
            hook_local_owner: hooks.hook_owner,
            hot_reattached: false,
            reader_retirement,
            retirement_log_owned: AtomicBool::new(false),
            legacy_unowned_transfer: AtomicBool::new(false),
            semantic_output_ready,
            durable_pressure,
            _reader_thread: reader_thread,
        })
    }

    /// Handle atado a una sesión **del daemon** (P3.15, T3).
    ///
    /// El grid se parsea acá igual que con un PTY local; lo único distinto es
    /// que los bytes llegan del socket y las escrituras salen como `Write`.
    /// El snapshot del attach se replaya antes de escuchar eventos nuevos, y
    /// los eventos con `seq` ≤ el del attach se descartan (dedup, T4).
    #[cfg(all(unix, feature = "daemon"))]
    pub fn attach_remote(
        session_id: Uuid,
        control: std::os::unix::net::UnixStream,
        events: std::io::BufReader<std::os::unix::net::UnixStream>,
        snapshot: &[u8],
        attached_seq: u64,
        hot_reattached: bool,
        session_alive: bool,
        cols: u16,
        rows: u16,
        scheduler: SharedRuntimeScheduler,
    ) -> anyhow::Result<Self> {
        events
            .get_ref()
            .set_read_timeout(None)
            .context("switch attached socket to event streaming")?;
        use crate::terminal::remote_session::RemoteReader;

        let link = RemoteLink::new(session_id, control);
        let title = Arc::new(ArcSwap::from_pointee("Terminal".to_owned()));
        let alive = Arc::new(AtomicBool::new(session_alive));
        let remote_exited = Arc::new(AtomicBool::new(!session_alive));
        let bell_fired = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicI64::new(pty_clock_now_ms()));
        let activity_revision = Arc::new(AtomicU64::new(0));
        let window_size = Arc::new(Mutex::new(WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: 0,
            cell_height: 0,
        }));
        let render_revision = Arc::new(AtomicU64::new(0));
        let agent_status = Arc::new(ArcSwap::from_pointee(None));
        let cwd = Arc::new(ArcSwap::from_pointee(None));
        let pending_log = Arc::new(Mutex::new(Vec::<u8>::new()));
        let log_seq = Arc::new(AtomicU64::new(0));
        let restoring_history = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let term_config = TermConfig {
            scrolling_history: crate::config::runtime_config().scrollback_lines,
            ..TermConfig::default()
        };
        let scrollback_limit = term_config.scrolling_history;
        let term = Arc::new(Mutex::new(Term::new(
            term_config,
            &TermSize::new(cols.max(1) as usize, rows.max(1) as usize),
            EventProxy::new(event_tx),
        )));

        // El historial que el daemon ya tenía se replaya antes de escuchar:
        // así el panel no aparece vacío al reengancharse.
        let mut agent_stream = AgentStatusStream::new();
        let (snapshot, reports, cwds) = agent_stream.process(snapshot, pty_clock_now_ms());
        if let Some(report) = reports.into_iter().next_back() {
            agent_status.store(Arc::new(Some(report)));
        }
        if let Some(new_cwd) = cwds.into_iter().next_back() {
            cwd.store(Arc::new(Some(new_cwd)));
        }
        if !snapshot.is_empty() {
            if let Ok(mut term) = term.lock() {
                let mut processor = Processor::<StdSyncHandler>::new();
                processor.advance(&mut *term, &snapshot);
            }
        }

        let writer_for_reader = TerminalWriter::Remote(link.clone());
        let writer_for_thread = writer_for_reader.clone();
        let title_for_reader = Arc::clone(&title);
        let alive_for_reader = Arc::clone(&alive);
        let bell_for_reader = Arc::clone(&bell_fired);
        let output_for_reader = Arc::clone(&last_output_at);
        let activity_for_reader = Arc::clone(&activity_revision);
        let term_for_reader = Arc::clone(&term);
        let window_size_for_reader = Arc::clone(&window_size);
        let render_revision_for_reader = Arc::clone(&render_revision);
        let agent_status_for_reader = Arc::clone(&agent_status);
        let cwd_for_reader = Arc::clone(&cwd);
        let scheduler_for_reader = Arc::clone(&scheduler);
        let restoring_for_reader = Arc::clone(&restoring_history);
        let exited_for_reader = Arc::clone(&remote_exited);

        let durable_pressure = Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        let semantic_output_ready = Arc::new(AtomicBool::new(false));
        let reader_retirement = Arc::new(ReaderRetirement::default());
        let retirement_for_reader = Arc::clone(&reader_retirement);
        let reader_thread = thread::spawn(move || {
            let loop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut processor = Processor::<StdSyncHandler>::new();
                let mut reader = RemoteReader::from_buffered(events, session_id, attached_seq);
                drain_terminal_events(
                    &event_rx,
                    &writer_for_thread,
                    &title_for_reader,
                    &alive_for_reader,
                    &bell_for_reader,
                    &window_size_for_reader,
                    &scheduler_for_reader,
                    session_id,
                );
                loop {
                    wait_for_history_restore(&restoring_for_reader);
                    yield_for_reader_priority(&scheduler_for_reader, session_id);
                    let Some(bytes) = reader.next_output() else {
                        if reader.exited() {
                            exited_for_reader.store(true, Ordering::Release);
                        }
                        break;
                    };
                    if let Some(error) = reader.take_input_error() {
                        writer_for_thread.record_error(error);
                    }
                    wait_for_history_restore(&restoring_for_reader);
                    let now_ms = pty_clock_now_ms();
                    let (clean, agent_reports, cwd_reports) = agent_stream.process(&bytes, now_ms);
                    if let Some(report) = agent_reports.into_iter().next_back() {
                        agent_status_for_reader.store(Arc::new(Some(report)));
                    }
                    if let Some(new_cwd) = cwd_reports.into_iter().next_back() {
                        cwd_for_reader.store(Arc::new(Some(new_cwd)));
                    }
                    if let Ok(mut term) = term_for_reader.lock() {
                        processor.advance(&mut *term, &clean);
                    }
                    if !clean.is_empty() {
                        render_revision_for_reader.fetch_add(1, Ordering::Relaxed);
                        output_for_reader.store(now_ms, Ordering::Relaxed);
                        activity_for_reader.fetch_add(1, Ordering::Relaxed);
                        if let Ok(mut scheduler) = scheduler_for_reader.lock() {
                            scheduler.record_output(session_id);
                        }
                    }
                    drain_terminal_events(
                        &event_rx,
                        &writer_for_thread,
                        &title_for_reader,
                        &alive_for_reader,
                        &bell_for_reader,
                        &window_size_for_reader,
                        &scheduler_for_reader,
                        session_id,
                    );
                }
            }));
            retirement_for_reader.request();
            if loop_result.is_err() {
                log::error!("el lector remoto de la sesión {session_id} paniqueó");
            }
            alive_for_reader.store(false, Ordering::Relaxed);
            if let Ok(mut scheduler) = scheduler_for_reader.lock() {
                scheduler.record_exit(session_id);
            }
        });

        Ok(Self {
            term,
            title,
            alive,
            bell_fired,
            writer: writer_for_reader,
            last_output_at,
            activity_revision,
            window_size,
            render_revision,
            agent_status,
            cwd,
            pending_log,
            log_seq,
            restoring_history,
            history_restore_failed: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            history_replay_spawner: Mutex::new(None),
            scrollback_limit,
            #[cfg(feature = "ghostty-vt")]
            backend_kind: TerminalBackendKind::Alacritty,
            #[cfg(feature = "ghostty-vt")]
            ghostty_runtime: None,
            master: None,
            child_lifetime: None,
            process_reaped: Arc::new(AtomicBool::new(false)),
            master_closer: None,
            remote: Some(link),
            remote_exited,
            hook_evidence: crate::hook_authority::HostEvidence {
                runtime: session_id,
                generation: None,
                target: None,
            },
            hook_local_owner: None,
            hot_reattached,
            reader_retirement,
            retirement_log_owned: AtomicBool::new(false),
            legacy_unowned_transfer: AtomicBool::new(false),
            semantic_output_ready,
            durable_pressure,
            _reader_thread: reader_thread,
        })
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if self.retirement_log_owned.load(Ordering::Acquire) {
            return;
        }
        if let Some(master) = self.master.as_ref() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        // Sesión remota: el resize viaja al daemon, que lo aplica al PTY real.
        #[cfg(all(unix, feature = "daemon"))]
        if let Some(remote) = self.remote.as_ref() {
            remote.resize(cols, rows);
        }
        let size_changed = if let Ok(mut window_size) = self.window_size.lock() {
            let changed = window_size.num_lines != rows || window_size.num_cols != cols;
            *window_size = WindowSize {
                num_lines: rows,
                num_cols: cols,
                cell_width: 0,
                cell_height: 0,
            };
            changed
        } else {
            true
        };
        if let Ok(mut term) = self.term.lock() {
            term.resize(TermSize::new(cols as usize, rows as usize));
            if size_changed {
                if let Ok(mut pending) = self.pending_log.lock() {
                    if let Some(seq) = next_log_sequence(&self.log_seq) {
                        pending.extend_from_slice(&crate::state::scrollback_log::encode_frame(
                            seq,
                            crate::state::scrollback_log::FrameKind::Resize,
                            &crate::state::scrollback_log::resize_payload(cols, rows),
                        ));
                    } else {
                        log::error!("se agotó la secuencia incremental del PTY");
                    }
                }
            }
        }
        #[cfg(feature = "ghostty-vt")]
        if let Some(ghostty) = &self.ghostty_runtime {
            ghostty.resize(cols, rows);
        }
        self.mark_render_dirty();
    }

    /// Mata la sesión **en el daemon** (P3.15). Se llama al cerrar un panel,
    /// no al cerrar la app: si no, la sesión no sobreviviría a un cierre limpio.
    #[cfg(all(unix, feature = "daemon"))]
    pub fn kill_remote_session(&self) {
        if let Some(remote) = self.remote.as_ref() {
            remote.kill();
        }
    }

    /// ¿Esta sesión vive en el daemon?
    pub fn is_remote(&self) -> bool {
        #[cfg(all(unix, feature = "daemon"))]
        {
            self.remote.is_some()
        }
        #[cfg(not(all(unix, feature = "daemon")))]
        {
            false
        }
    }

    /// An explicit process exit must not be retried as a transport failure.
    pub fn remote_session_exited(&self) -> bool {
        #[cfg(all(unix, feature = "daemon"))]
        {
            self.remote_exited.load(Ordering::Acquire)
        }
        #[cfg(not(all(unix, feature = "daemon")))]
        {
            false
        }
    }

    /// `true` sólo cuando este grid ya recibió el snapshot de una sesión que
    /// sobrevivió a una corrida anterior de la app.
    pub fn was_hot_reattached(&self) -> bool {
        self.hot_reattached
    }

    pub fn write_all(&self, bytes: &[u8]) {
        if let Err(error) = self.try_write_all(bytes) {
            log::warn!("{error}");
        }
    }

    pub fn try_write_all(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.enqueue(bytes)?;
        if !bytes.is_empty() {
            self.activity_revision.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    pub(crate) fn check_input_open(&self) -> std::io::Result<()> {
        self.writer.check_open()
    }

    pub fn activity_revision(&self) -> u64 {
        self.activity_revision.load(Ordering::Relaxed)
    }

    pub fn record_input_error(&self, message: String) {
        self.writer.record_error(message);
    }

    pub fn input_error(&self) -> Option<String> {
        #[cfg(all(unix, feature = "daemon"))]
        if let Some(error) = self.remote.as_ref().and_then(RemoteLink::input_error) {
            return Some(error);
        }
        self.writer.error()
    }

    pub fn output_elapsed(&self) -> Duration {
        let last = self.last_output_at.load(Ordering::Relaxed);
        let delta = pty_clock_now_ms().saturating_sub(last).max(0) as u64;
        Duration::from_millis(delta)
    }

    pub fn input_mode(&self) -> InputMode {
        let Ok(term) = self.term.lock() else {
            return InputMode::default();
        };
        let mode = term.mode().to_owned();
        InputMode {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            mouse_mode: mode.intersects(TermMode::MOUSE_MODE),
            mouse_drag: mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
            mouse_motion: mode.contains(TermMode::MOUSE_MOTION),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
        }
    }

    /// Último report de estado de agente recibido por el canal OSC 9999,
    /// si alguno. El sello `received_at_ms` usa `pty_clock_now_ms`.
    pub fn agent_status_snapshot(&self) -> Option<AgentStatusReport> {
        (*self.agent_status.load_full()).clone()
    }

    /// Último cwd reportado por el shell vía OSC 7, si alguno.
    pub fn current_cwd(&self) -> Option<String> {
        (*self.cwd.load_full()).clone()
    }

    pub fn metadata_osc(&self, attaching: bool) -> Vec<u8> {
        crate::terminal::agent_status::metadata_osc(
            self.current_cwd().as_deref(),
            self.agent_status_snapshot().as_ref(),
            &self.title.load_full(),
            attaching.then(pty_clock_now_ms),
        )
    }

    #[cfg(feature = "ghostty-vt")]
    pub fn backend_kind(&self) -> TerminalBackendKind {
        if self
            .ghostty_runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.is_healthy())
        {
            return TerminalBackendKind::Alacritty;
        }
        self.backend_kind
    }

    #[cfg(feature = "ghostty-vt")]
    pub fn ghostty_snapshot(&self) -> Option<Arc<GhosttyTextSnapshot>> {
        self.ghostty_runtime.as_ref()?.snapshot()
    }

    pub fn scroll_display(&self, scroll: Scroll) {
        if let Ok(mut term) = self.term.lock() {
            term.scroll_display(scroll);
        }
        #[cfg(feature = "ghostty-vt")]
        if let Some(ghostty) = &self.ghostty_runtime {
            match scroll {
                Scroll::Delta(delta) => ghostty.scroll_delta(delta),
                Scroll::PageUp => ghostty.scroll_delta(10),
                Scroll::PageDown => ghostty.scroll_delta(-10),
                Scroll::Top => ghostty.scroll_to_display_offset(usize::MAX),
                Scroll::Bottom => ghostty.scroll_to_display_offset(0),
            }
        }
        self.mark_render_dirty();
    }

    pub fn selected_text(&self) -> Option<String> {
        self.term.lock().ok()?.selection_to_string()
    }

    pub fn with_term<R>(&self, f: impl FnOnce(&mut Term<EventProxy>) -> R) -> Option<R> {
        if self.retirement_log_owned.load(Ordering::Acquire)
            || self.restoring_history.load(Ordering::Acquire)
        {
            return None;
        }
        let mut term = self.term.try_lock().ok()?;
        Some(f(&mut term))
    }

    /// Reinyecta bytes directamente en el grid, **sin** pasarlos al PTY: se usa
    /// para restaurar el scrollback de una sesión anterior. Usa un parser
    /// propio y efímero porque es una pasada única y no debe compartir estado
    /// con el parser del hilo lector.
    pub fn replay_history(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // `lock` (no `try_lock`): la restauración ocurre una sola vez y perder
        // el historial por contención sería peor que esperar un instante.
        let Ok(mut term) = self.term.lock() else {
            return;
        };
        let mut processor = Processor::<StdSyncHandler>::new();
        processor.advance(&mut *term, bytes);
        drop(term);
        self.mark_render_dirty();
    }

    pub fn set_daemon_durable_pressure(&self, pending_bytes: usize) {
        self.durable_pressure
            .store(pending_bytes, Ordering::Release);
    }

    fn charge_transferred_pending(&self, bytes: usize) {
        if bytes > 0 && self.durable_pressure.load(Ordering::Acquire) == usize::MAX {
            // Legacy destructive local callers have no durable-ACK ownership.
            // They cannot later claim a strong physical retirement certificate.
            self.legacy_unowned_transfer.store(true, Ordering::Release);
        }
        let _ =
            self.durable_pressure
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                    (pending != usize::MAX)
                        .then(|| pending.saturating_add(bytes).min(usize::MAX - 1))
                });
    }

    /// Drena los frames de log incremental acumulados desde el último autosave
    /// (P1.7). Devuelve los bytes ya encodeados, listos para appendear.
    pub fn drain_pending_log(&self) -> Vec<u8> {
        if self.retirement_log_owned.load(Ordering::Acquire) {
            return Vec::new();
        }
        self.pending_log
            .lock()
            .map(|mut pending| {
                let drained = std::mem::take(&mut *pending);
                self.charge_transferred_pending(drained.len());
                self.log_seq.store(0, Ordering::Relaxed);
                drained
            })
            .unwrap_or_default()
    }

    /// Como `drain_pending_log`, pero drena como máximo `max_bytes` del
    /// prefijo, cortando solo en fronteras de frame completas. Los frames
    /// restantes quedan para el próximo drenado.
    /// Se usa para el carril interactivo (P3.16, T2): la sesión enfocada
    /// tiene un presupuesto de 32 KB por pump y el resto espera al siguiente.
    pub fn drain_pending_log_capped(&self, max_bytes: usize) -> Vec<u8> {
        if self.retirement_log_owned.load(Ordering::Acquire) {
            return Vec::new();
        }
        self.pending_log
            .lock()
            .map(|mut pending| {
                let drained = drain_log_prefix(&mut pending, max_bytes);
                self.charge_transferred_pending(drained.len());
                if pending.is_empty() {
                    self.log_seq.store(0, Ordering::Relaxed);
                }
                drained
            })
            .unwrap_or_default()
    }

    /// Copia el lote todavía no confirmado. No se quita de RAM hasta que el
    /// worker durable confirma exactamente este prefijo.
    pub fn pending_log_snapshot(&self) -> Vec<u8> {
        if self.retirement_log_owned.load(Ordering::Acquire) {
            return Vec::new();
        }
        self.retirement_log_snapshot()
    }

    fn retirement_log_snapshot(&self) -> Vec<u8> {
        self.pending_log
            .lock()
            .map(|pending| pending.clone())
            .unwrap_or_default()
    }

    pub fn acknowledge_pending_log(&self, written_bytes: usize) {
        if self.retirement_log_owned.load(Ordering::Acquire) {
            return;
        }
        if let Ok(mut pending) = self.pending_log.lock() {
            let acknowledged = written_bytes.min(pending.len());
            pending.drain(..acknowledged);
            if pending.is_empty() {
                self.log_seq.store(0, Ordering::Relaxed);
            }
        }
    }

    /// Snapshot consistente para checkpoint: el orden de locks coincide con
    /// el lector (`term` y luego `pending_log`), así todo byte confirmado por
    /// el ACK ya está representado en el texto exportado.
    pub fn checkpoint_snapshot<R>(
        &self,
        export: impl FnOnce(&Term<EventProxy>) -> R,
    ) -> Option<(R, usize)> {
        if self.retirement_log_owned.load(Ordering::Acquire)
            || self.restoring_history.load(Ordering::Acquire)
            || self.history_restore_failed.load(Ordering::Acquire)
        {
            return None;
        }
        let term = self.term.lock().ok()?;
        let pending_bytes = self.pending_log.lock().ok()?.len();
        let text = export(&term);
        Some((text, pending_bytes))
    }

    pub fn recovery_snapshot<R>(
        &self,
        restore_pending: bool,
        export: impl FnOnce(&Term<EventProxy>) -> R,
    ) -> RecoverySnapshot<R> {
        if self.retirement_log_owned.load(Ordering::Acquire) {
            return RecoverySnapshot::Unavailable;
        }
        let checkpoint = if restore_pending || self.restoring_history.load(Ordering::Acquire) {
            None
        } else {
            self.checkpoint_snapshot(export)
        };
        if let Some((value, pending_bytes)) = checkpoint {
            return RecoverySnapshot::Checkpoint {
                value,
                pending_bytes,
            };
        }
        // A replay or a poisoned grid cannot yield a complete checkpoint.
        // Its healthy log still has a durable rescue boundary without taking
        // `term`; unavailable output must remain visible to the final save.
        self.pending_log
            .lock()
            .map(|pending| RecoverySnapshot::PendingLog(pending.clone()))
            .unwrap_or(RecoverySnapshot::Unavailable)
    }

    #[cfg(test)]
    pub(crate) fn history_restore_in_progress(&self) -> bool {
        self.restoring_history.load(Ordering::Acquire)
    }

    /// Request input/read quiescence and copy the last complete raw boundary.
    /// Pending bytes remain in the PTY until the daemon's durable ACK permits Drop.
    /// None retains the handle while an already-admitted blocking read completes.
    pub fn semantic_output_ready(&self) -> bool {
        self.semantic_output_ready.load(Ordering::Acquire)
    }

    /// The caller owns any earlier batch already moved outside this handle.
    /// It must retain/ACK that batch as well before the durable prefix permits kill.
    pub fn claim_retirement_log_owner(&self) {
        self.retirement_log_owned.store(true, Ordering::Release);
        self.writer.close();
        self.reader_retirement.request();
        if !self.reader_retirement.quiescent() && !self.reader_retirement.draining() {
            if let Err(error) = reader_retirement::cancel_blocked_read(&self._reader_thread) {
                log::error!("No se pudo cancelar la lectura del PTY: {error}");
            }
        }
    }

    pub fn take_retirement_prefix(
        &self,
        durable: &mut Vec<u8>,
        destination_seq: &mut u64,
        has_destination: bool,
    ) -> std::io::Result<bool> {
        if self.retirement_pending_snapshot()?.is_none() {
            return Ok(false);
        }
        self.transfer_retirement_frames(durable, destination_seq, has_destination)
    }

    pub fn take_retirement_tail(
        &self,
        durable: &mut Vec<u8>,
        destination_seq: &mut u64,
        has_destination: bool,
    ) -> std::io::Result<bool> {
        if !self.reader_retirement.draining() && !self.reader_retirement.eof() {
            return Ok(false);
        }
        if self.reader_retirement.failed() {
            return Err(std::io::Error::other(
                "El lector de salida no confirmó EOF.",
            ));
        }
        self.transfer_retirement_frames(durable, destination_seq, has_destination)
    }

    fn transfer_retirement_frames(
        &self,
        durable: &mut Vec<u8>,
        destination_seq: &mut u64,
        has_destination: bool,
    ) -> std::io::Result<bool> {
        if self.log_seq.load(Ordering::Acquire) == u64::MAX {
            return Err(std::io::Error::other("Secuencia raw agotada."));
        }
        let mut pending = self
            .pending_log
            .lock()
            .map_err(|_| std::io::Error::other("Log raw envenenado."))?;
        if !has_destination && (!pending.is_empty() || !durable.is_empty()) {
            return Ok(false);
        }
        let encoded = [
            crate::state::scrollback_log::encode_header(0),
            pending.clone(),
        ]
        .concat();
        let (_, frames, valid_len) = crate::state::scrollback_log::read_frames_with_valid_len(
            &encoded,
        )
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "Log raw inválido."))?;
        if valid_len != encoded.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Log raw incompleto.",
            ));
        }
        let final_seq = destination_seq
            .checked_add(frames.len() as u64)
            .ok_or_else(|| std::io::Error::other("Secuencia durable agotada."))?;
        for frame in frames {
            *destination_seq += 1;
            durable.extend_from_slice(&crate::state::scrollback_log::encode_frame(
                *destination_seq,
                frame.kind,
                &frame.payload,
            ));
        }
        debug_assert_eq!(*destination_seq, final_seq);
        // Transfer exactly once, while the same lock protects newly read bytes.
        pending.clear();
        self.set_daemon_durable_pressure(durable.len());
        Ok(true)
    }

    /// Called only after the complete pretermination prefix is durably ACKed.
    /// Resume first: ConPTY close may block waiting for its output to be drained.
    pub fn begin_retirement_drain(&mut self) -> std::io::Result<()> {
        self.reader_retirement.resume_drain();
        if !self.reader_retirement.draining() && !self.reader_retirement.eof() {
            return Err(std::io::Error::other("El lector no confirmó la pausa."));
        }
        if let Some(master) = self.master.take() {
            let owner = Arc::new(Mutex::new(Some(master)));
            let worker = Arc::clone(&owner);
            match thread::Builder::new()
                .name("pty-master-close".to_owned())
                .spawn(move || {
                    let master = worker
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    drop(master);
                }) {
                Ok(thread) => self.master_closer = Some(thread),
                Err(error) => {
                    self.master = owner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    return Err(error);
                }
            }
        }
        if let Some(child) = &self.child_lifetime {
            child.terminate();
        }
        Ok(())
    }

    pub(crate) fn hook_host_evidence(&self) -> crate::hook_authority::HostEvidence {
        self.hook_evidence
    }
    pub(crate) fn hook_local_owner(&self) -> Option<Uuid> {
        self.hook_local_owner
    }
    #[cfg(all(unix, feature = "daemon"))]
    pub(crate) fn set_hook_host_evidence(&mut self, evidence: crate::hook_authority::HostEvidence) {
        self.hook_evidence = evidence;
    }

    pub(crate) fn process_reaped(&self) -> bool {
        self.process_reaped.load(Ordering::Acquire)
    }

    /// No read error, parser panic, canceled read or finished thread is EOF.
    pub fn retirement_physical_complete(&self) -> bool {
        self.writer.close(); // Retry Windows cancel if admission raced the first call.
        !self.legacy_unowned_transfer.load(Ordering::Acquire)
            && self.reader_retirement.eof()
            && self.output_finished()
            && self.process_reaped()
            && matches!(&self.writer, TerminalWriter::Local(writer) if writer.finished())
            && self.master.is_none()
            && self
                .master_closer
                .as_ref()
                .is_none_or(thread::JoinHandle::is_finished)
    }

    pub fn retirement_pending_snapshot(&self) -> std::io::Result<Option<Vec<u8>>> {
        if self.is_remote() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "El cierre durable pertenece al host del PTY.",
            ));
        }
        self.claim_retirement_log_owner();
        if !self.reader_retirement.quiescent() {
            return Ok(None);
        }
        if self.restoring_history.load(Ordering::Acquire) {
            return Ok(None);
        }
        if self.reader_retirement.failed() {
            return Err(std::io::Error::other(
                "El lector no confirmó EOF; se conserva el PTY.",
            ));
        }
        if self.log_seq.load(Ordering::Acquire) == u64::MAX {
            return Err(std::io::Error::other(
                "El cierre no puede probar captura completa tras agotar secuencias.",
            ));
        }
        let pending = self.pending_log.lock().map_err(|_| {
            std::io::Error::other(
                "El log pendiente del PTY no está disponible para confirmar su cierre.",
            )
        })?;
        if !pending.is_empty()
            && crate::state::scrollback_log::read_frames(
                &[
                    crate::state::scrollback_log::encode_header(0),
                    pending.clone(),
                ]
                .concat(),
            )
            .is_none()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "El cierre conserva un log incremental inválido.",
            ));
        }
        Ok(Some(pending.clone()))
    }

    /// Crea la frontera atómica de un hot attach. El snapshot del grid y los
    /// frames que todavía no recibieron `seq` se toman bajo el mismo orden de
    /// locks que usa el reader (`term` → `pending_log`), de modo que ningún
    /// byte pueda aparecer en el snapshot con un `seq` anterior.
    pub fn attach_snapshot_and_drain<R>(
        &self,
        export: impl FnOnce(&mut Term<EventProxy>) -> R,
    ) -> Option<(R, Vec<u8>)> {
        if self.retirement_log_owned.load(Ordering::Acquire)
            || self.restoring_history.load(Ordering::Acquire)
        {
            return None;
        }
        let mut term = self.term.lock().ok()?;
        let mut pending = self.pending_log.lock().ok()?;
        let snapshot = export(&mut term);
        let frames = std::mem::take(&mut *pending);
        self.charge_transferred_pending(frames.len());
        self.log_seq.store(0, Ordering::Relaxed);
        Some((snapshot, frames))
    }

    /// Capture may refuse an oversized or unavailable semantic replay. Refusal
    /// occurs before taking pending frames or resetting their sequence.
    /// The exporter must include its metadata/wire validation in this Result.
    pub fn try_attach_snapshot_and_drain<R, E>(
        &self,
        export: impl FnOnce(&mut Term<EventProxy>) -> Result<R, E>,
    ) -> Result<Option<(R, Vec<u8>)>, E> {
        if self.retirement_log_owned.load(Ordering::Acquire)
            || self.restoring_history.load(Ordering::Acquire)
            || self.history_restore_failed.load(Ordering::Acquire)
        {
            return Ok(None);
        }
        let Ok(mut term) = self.term.lock() else {
            return Ok(None);
        };
        let Ok(mut pending) = self.pending_log.lock() else {
            return Ok(None);
        };
        let snapshot = export(&mut term)?;
        let frames = std::mem::take(&mut *pending);
        self.charge_transferred_pending(frames.len());
        self.log_seq.store(0, Ordering::Relaxed);
        Ok(Some((snapshot, frames)))
    }

    /// Returning from a TUI needs the hidden primary grid for clients that
    /// attached inside the alternate screen. Snapshot and drain share a boundary.
    pub fn output_update(
        &self,
        max_bytes: usize,
        was_alternate: bool,
        export: impl FnOnce(&mut Term<EventProxy>) -> String,
    ) -> Option<(Vec<u8>, bool, Option<String>)> {
        if self.retirement_log_owned.load(Ordering::Acquire)
            || self.restoring_history.load(Ordering::Acquire)
        {
            return None;
        }
        let mut term = self.term.lock().ok()?;
        let alternate = term.mode().contains(TermMode::ALT_SCREEN);
        let snapshot = (was_alternate && !alternate).then(|| export(&mut term));
        let mut pending = self.pending_log.lock().ok()?;
        let frames = drain_log_prefix(
            &mut pending,
            if snapshot.is_some() {
                usize::MAX
            } else {
                max_bytes
            },
        );
        self.charge_transferred_pending(frames.len());
        Some((frames, alternate, snapshot))
    }

    /// The caller's old alternate state stays unchanged until this Result
    /// succeeds. A failed primary repair leaves every raw frame pending.
    pub fn try_output_update<E>(
        &self,
        max_bytes: usize,
        was_alternate: bool,
        export: impl FnOnce(&mut Term<EventProxy>) -> Result<String, E>,
    ) -> Result<Option<PtyOutputUpdate>, E> {
        if self.retirement_log_owned.load(Ordering::Acquire)
            || self.restoring_history.load(Ordering::Acquire)
            || self.history_restore_failed.load(Ordering::Acquire)
        {
            return Ok(None);
        }
        let Ok(mut term) = self.term.lock() else {
            return Ok(None);
        };
        let alternate = term.mode().contains(TermMode::ALT_SCREEN);
        let snapshot = if was_alternate && !alternate {
            Some(export(&mut term)?)
        } else {
            None
        };
        let Ok(mut pending) = self.pending_log.lock() else {
            return Ok(None);
        };
        let frames = drain_log_prefix(
            &mut pending,
            if snapshot.is_some() {
                usize::MAX
            } else {
                max_bytes
            },
        );
        self.charge_transferred_pending(frames.len());
        Ok(Some((frames, alternate, snapshot)))
    }

    /// Restaura checkpoint + frames del log incremental en orden (P1.7).
    /// Los resize se aplican como `term.resize` antes de seguir el replay del
    /// output posterior, para que el grid tenga el tamaño correcto.
    pub fn replay_session(
        &self,
        checkpoint: &[u8],
        frames: &[crate::state::scrollback_log::Frame],
    ) -> bool {
        self.replay_session_preserving_live(checkpoint, frames, |_| Vec::new())
    }

    fn spawn_history_replay(&self, replay: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
        #[cfg(test)]
        {
            let mut injected = self
                .history_replay_spawner
                .lock()
                .map_err(|_| std::io::Error::other("replay test spawner lock poisoned"))?;
            if let Some(spawn) = injected.as_mut() {
                return spawn(Box::new(replay));
            }
        }
        thread::Builder::new()
            .name("scrollback-replay".to_owned())
            .spawn(replay)
            .map(|_| ())
    }

    /// Variante usada por la app real: recibe el exportador semántico desde
    /// el módulo host para que los harness que montan `pty.rs` por `#[path]`
    /// no tengan que incluir todo `terminal::export`.
    /// Returns whether the request was accepted, not whether replay finished.
    /// A busy or rejected worker leaves the caller's durable history pending.
    pub fn replay_session_preserving_live(
        &self,
        checkpoint: &[u8],
        frames: &[crate::state::scrollback_log::Frame],
        export_live: impl FnOnce(&Term<EventProxy>) -> Vec<u8> + Send + 'static,
    ) -> bool {
        if checkpoint.is_empty() && frames.is_empty() {
            return true;
        }
        if self
            .restoring_history
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        if self.history_restore_failed.load(Ordering::Acquire) {
            // Check after acquiring the guard: an earlier worker can fail
            // while this request is entering. Its partial grid cannot become
            // the live output of a retry; restoring needs a fresh PTY.
            self.restoring_history.store(false, Ordering::Release);
            return false;
        }
        let checkpoint = checkpoint.to_vec();
        let frames = frames.to_vec();
        let term = Arc::clone(&self.term);
        let window_size = Arc::clone(&self.window_size);
        let restoring = Arc::clone(&self.restoring_history);
        let restoring_for_thread = Arc::clone(&restoring);
        let failed = Arc::clone(&self.history_restore_failed);
        let render_revision = Arc::clone(&self.render_revision);
        let spawn_result = self.spawn_history_replay(move || {
            let replay_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                const CHUNK_BYTES: usize = 16 * 1024;
                let mut processor = Processor::<StdSyncHandler>::new();
                // El shell puede haber emitido prompt/salida mientras el
                // checkpoint se leía en el worker. Capturamos ese grid,
                // limpiamos y lo reinyectamos al final: historial viejo
                // primero, salida viva después, sin perder ninguna de las
                // dos ni congelar el frame durante I/O de disco.
                let live_output = {
                    let Ok(mut term) = term.lock() else {
                        return false;
                    };
                    let exported = export_live(&term);
                    let mut live = Vec::with_capacity(exported.len());
                    for (index, byte) in exported.iter().copied().enumerate() {
                        if byte == b'\n' && (index == 0 || exported[index - 1] != b'\r') {
                            live.push(b'\r');
                        }
                        live.push(byte);
                    }
                    processor.advance(&mut *term, b"\x1b[2J\x1b[3J\x1b[H");
                    live
                };
                for chunk in checkpoint.chunks(CHUNK_BYTES) {
                    let Ok(mut term) = term.lock() else {
                        return false;
                    };
                    processor.advance(&mut *term, chunk);
                    drop(term);
                    thread::yield_now();
                }
                for frame in frames {
                    match frame.kind {
                        crate::state::scrollback_log::FrameKind::Output => {
                            for chunk in frame.payload.chunks(CHUNK_BYTES) {
                                let Ok(mut term) = term.lock() else {
                                    return false;
                                };
                                processor.advance(&mut *term, chunk);
                                drop(term);
                                thread::yield_now();
                            }
                        }
                        crate::state::scrollback_log::FrameKind::Resize => {
                            if let Some((cols, rows)) =
                                crate::state::scrollback_log::parse_resize(&frame.payload)
                            {
                                let Ok(mut term) = term.lock() else {
                                    return false;
                                };
                                term.resize(TermSize::new(cols as usize, rows as usize));
                            }
                        }
                        crate::state::scrollback_log::FrameKind::Clear => {
                            let Ok(mut term) = term.lock() else {
                                return false;
                            };
                            processor.advance(&mut *term, b"\x1b[2J\x1b[3J\x1b[H");
                        }
                    }
                }
                let (cols, rows) = match window_size.lock() {
                    Ok(size) => (size.num_cols, size.num_lines),
                    Err(_) => return false,
                };
                {
                    let Ok(mut term) = term.lock() else {
                        return false;
                    };
                    // A replayed resize belongs to the old process. Restore
                    // current physical dimensions before injecting the live prompt.
                    term.resize(TermSize::new(cols.max(1) as usize, rows.max(1) as usize));
                    processor.advance(&mut *term, b"\x1b[?1049l\x1b[0m");
                    processor.advance(&mut *term, &crate::state::scrollback_store::replay_marker());
                }
                for chunk in live_output.chunks(CHUNK_BYTES) {
                    let Ok(mut term) = term.lock() else {
                        return false;
                    };
                    processor.advance(&mut *term, chunk);
                    drop(term);
                    thread::yield_now();
                }
                true
            }));
            if !matches!(replay_result, Ok(true)) {
                failed.store(true, Ordering::Release);
                if replay_result.is_err() {
                    log::error!("el replay de scrollback paniqueó");
                } else {
                    log::error!("el replay de scrollback no pudo leer el estado del terminal");
                }
            }
            render_revision.fetch_add(1, Ordering::Release);
            restoring_for_thread.store(false, Ordering::Release);
        });
        if let Err(err) = spawn_result {
            restoring.store(false, Ordering::Release);
            log::warn!("no se pudo iniciar el replay de scrollback: {err}");
            return false;
        }
        true
    }

    pub fn title_snapshot(&self) -> Option<String> {
        Some((*self.title.load_full()).clone())
    }

    pub fn clear_selection(&self) {
        if let Ok(mut term) = self.term.try_lock() {
            term.selection = None;
        }
        self.mark_render_dirty();
    }

    pub fn render_revision(&self) -> u64 {
        self.render_revision.load(Ordering::Relaxed)
    }

    pub fn mark_render_dirty(&self) {
        self.render_revision.fetch_add(1, Ordering::Relaxed);
    }

    pub fn scroll_state(&self) -> Option<TerminalScrollState> {
        #[cfg(feature = "ghostty-vt")]
        if let Some(snapshot) = self.ghostty_snapshot() {
            return Some(snapshot.scroll_state);
        }
        let term = self.term.try_lock().ok()?;
        Some(TerminalScrollState {
            display_offset: term.grid().display_offset(),
            visible_rows: term.screen_lines(),
            history_size: term.grid().history_size().min(self.scrollback_limit),
        })
    }

    pub fn scroll_to_display_offset(&self, target: usize) {
        if let Ok(mut term) = self.term.try_lock() {
            let current = term.grid().display_offset() as i32;
            let target = target.min(term.grid().history_size()) as i32;
            let delta = target - current;
            if delta != 0 {
                term.scroll_display(Scroll::Delta(delta));
            }
        }
        #[cfg(feature = "ghostty-vt")]
        if let Some(ghostty) = &self.ghostty_runtime {
            ghostty.scroll_to_display_offset(target);
        }
        self.mark_render_dirty();
    }

    pub fn take_bell(&self) -> bool {
        self.bell_fired.swap(false, Ordering::Relaxed)
    }

    /// The process can exit before its PTY reader finishes publishing bytes.
    /// This non-blocking boundary must be sampled before draining the final log.
    /// Windows UI liveness still uses its independent ConPTY process watcher.
    pub fn output_finished(&self) -> bool {
        self._reader_thread.is_finished()
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

fn restore_cold_history(
    term: &mut Term<EventProxy>,
    checkpoint: &[u8],
    frames: &[crate::state::scrollback_log::Frame],
    cols: u16,
    rows: u16,
) {
    use crate::state::scrollback_log::FrameKind;
    let mut parser = Processor::<StdSyncHandler>::new();
    parser.advance(term, checkpoint);
    for frame in frames {
        match frame.kind {
            FrameKind::Output => parser.advance(term, &frame.payload),
            FrameKind::Resize => {
                if let Some((cols, rows)) =
                    crate::state::scrollback_log::parse_resize(&frame.payload)
                {
                    term.resize(TermSize::new(cols.max(1) as usize, rows.max(1) as usize));
                }
            }
            FrameKind::Clear => parser.advance(term, b"\x1b[2J\x1b[3J\x1b[H"),
        }
    }
    // The old process is gone: retain its primary history and start the new
    // shell with ordinary input modes and the current physical dimensions.
    let mut parser = Processor::<StdSyncHandler>::new();
    parser.advance(term, b"\x1b[?1049l\x1b[0m\x1b[?6l\x1b[r\x1b[?1l\x1b[?7h\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1005l\x1b[?1006l\x1b[?2004l\x1b[4l\x1b[20l\x1b>\x1b[=0u");
    term.resize(TermSize::new(cols.max(1) as usize, rows.max(1) as usize));
    // Resetting the scroll region homes the cursor; place the new session
    // after all visible old content so its prompt cannot overwrite history.
    parser.advance(term, format!("\x1b[{};1H\r\n", rows.max(1)).as_bytes());
    parser.advance(term, &crate::state::scrollback_store::replay_marker());
}

fn shell_command(cwd: Option<&Path>, hooks: HookIdentity) -> CommandBuilder {
    let configured_shell = crate::config::runtime_config()
        .shell
        .filter(|shell| !shell.trim().is_empty());

    #[cfg(unix)]
    let mut cmd = match configured_shell {
        Some(shell) => {
            let mut builder = CommandBuilder::new(shell);
            // Login shell, como el default del sistema.
            builder.arg("-l");
            builder
        }
        None => CommandBuilder::new_default_prog(),
    };
    #[cfg(windows)]
    let mut cmd = match configured_shell {
        Some(shell) => CommandBuilder::new(shell),
        None => CommandBuilder::new(crate::utils::platform::default_shell()),
    };

    if let Some(cwd) = cwd {
        cmd.cwd(cwd);
        // El bridge MCP hereda esta raíz desde el shell y la usa como
        // frontera confiable, en vez de aceptar cualquier cwd del agente.
        cmd.env("TC_MEMORY_ROOT", cwd.as_os_str());
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("MI_TERMINAL", "1");
    // Identidad del pane para los hooks del agente (P2.12): el hook las
    // reenvía como query params y el listener resuelve pane -> panel.
    if let Some(panel_id) = hooks.panel_id {
        cmd.env("TC_PANEL_ID", panel_id.to_string());
    }
    if let Some(workspace_id) = hooks.workspace_id {
        cmd.env("TC_WORKSPACE_ID", workspace_id.to_string());
    }
    if let Some(leaf_id) = hooks.leaf_id {
        cmd.env("TC_LEAF_ID", leaf_id.to_string());
    }
    if let Some(task_id) = hooks.memory_task_id.or(hooks.leaf_id).or(hooks.panel_id) {
        cmd.env("TC_MEMORY_TASK_ID", task_id.to_string());
    }
    cmd
}

/// Identidad que se exporta al shell para que los hooks sepan de qué panel
/// vienen (P2.12).
#[derive(Debug, Clone, Copy, Default)]
pub struct HookIdentity {
    pub memory_task_id: Option<Uuid>,
    pub panel_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub leaf_id: Option<Uuid>,
    pub hook_generation: Option<Uuid>,
    pub(crate) hook_owner: Option<Uuid>,
}

impl HookIdentity {
    pub(crate) fn key(self) -> Option<crate::hook_authority::LeafKey> {
        Some(crate::hook_authority::LeafKey {
            workspace: self.workspace_id?,
            panel: self.panel_id?,
            leaf: self.leaf_id?,
        })
    }
    pub(crate) fn evidence(self, runtime: Uuid) -> crate::hook_authority::HostEvidence {
        crate::hook_authority::HostEvidence {
            runtime,
            generation: self.hook_generation,
            target: self.key().map(Into::into),
        }
    }

    pub fn memory_task_id(self, session_id: Uuid) -> Uuid {
        self.memory_task_id
            .or(self.leaf_id)
            .or(self.panel_id)
            .unwrap_or(session_id)
    }
}

fn drain_terminal_events(
    event_rx: &mpsc::Receiver<Event>,
    writer: &TerminalWriter,
    title: &Arc<ArcSwap<String>>,
    alive: &Arc<AtomicBool>,
    bell_fired: &Arc<AtomicBool>,
    window_size: &Arc<Mutex<WindowSize>>,
    scheduler: &SharedRuntimeScheduler,
    session_id: Uuid,
) {
    let mut sched_flags = SchedulerEventFlags::default();
    while let Ok(event) = event_rx.try_recv() {
        match event {
            Event::PtyWrite(text) => {
                let _ = writer.enqueue(text.as_bytes());
            }
            Event::Title(new_title) => {
                title.store(Arc::new(new_title));
                sched_flags.title_changed = true;
            }
            Event::ResetTitle => {
                title.store(Arc::new("Terminal".to_owned()));
                sched_flags.title_changed = true;
            }
            Event::ClipboardStore(_, text) => {
                if osc52_clipboard_enabled() {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(text);
                    }
                }
            }
            Event::ClipboardLoad(_, formatter) => {
                if osc52_clipboard_enabled() {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        if let Ok(text) = clipboard.get_text() {
                            let _ = writer.enqueue(formatter(&text).as_bytes());
                        }
                    }
                }
            }
            Event::ColorRequest(index, formatter) => {
                if index < 256 {
                    let color = indexed_to_egui(index as u8);
                    let rgb = alacritty_terminal::vte::ansi::Rgb {
                        r: color.r(),
                        g: color.g(),
                        b: color.b(),
                    };
                    let _ = writer.enqueue(formatter(rgb).as_bytes());
                }
            }
            Event::TextAreaSizeRequest(formatter) => {
                if let Ok(size) = window_size.lock().map(|guard| *guard) {
                    let _ = writer.enqueue(formatter(size).as_bytes());
                }
            }
            Event::Bell => {
                bell_fired.store(true, Ordering::Relaxed);
                sched_flags.bell = true;
                if crate::config::runtime_config().audio_bell {
                    crate::utils::platform::play_bell_sound();
                }
            }
            Event::Exit | Event::ChildExit(_) => {
                alive.store(false, Ordering::Relaxed);
                sched_flags.exited = true;
            }
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => {
                sched_flags.render = true;
            }
        }
    }

    if sched_flags.has_any() {
        if let Ok(mut scheduler) = scheduler.lock() {
            sched_flags.apply(&mut scheduler, session_id);
        }
    }
}

#[derive(Default)]
struct SchedulerEventFlags {
    title_changed: bool,
    bell: bool,
    exited: bool,
    render: bool,
}

impl SchedulerEventFlags {
    fn has_any(&self) -> bool {
        self.title_changed || self.bell || self.exited || self.render
    }

    fn apply(&self, scheduler: &mut crate::runtime::RuntimeScheduler, session_id: Uuid) {
        if self.title_changed {
            scheduler.record_title_changed(session_id);
        }
        if self.bell {
            scheduler.record_bell(session_id);
        }
        if self.exited {
            scheduler.record_exit(session_id);
        }
        if self.render {
            scheduler.record_render(session_id);
        }
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        if let Some(runtime) = crate::hook_runtime::process_runtime() {
            if self.is_remote() {
                runtime.release_endpoint(self.hook_evidence.runtime);
            } else if self.hook_local_owner == Some(runtime.owner) {
                runtime.begin_close(self.hook_evidence.runtime, true);
            }
        }
        self.writer.close();
        self.alive.store(false, Ordering::Relaxed);
        #[cfg(all(unix, feature = "daemon"))]
        if let Some(remote) = &self.remote {
            remote.disconnect();
        }
        if let Some(child) = &self.child_lifetime {
            child.terminate();
        }
        // Windows ClosePseudoConsole may block; never run it on the UI/daemon.
        if let Some(master) = self.master.take() {
            let _ = thread::Builder::new()
                .name("pty-master-drop".to_owned())
                .spawn(move || drop(master));
        }
        // Ojo: una sesión remota **no** se mata al dropear el handle. Cerrar la
        // app dropea todos los handles, y matar ahí sería justo lo contrario de
        // lo que el daemon promete. Matarla es explícito (`kill_remote_session`,
        // que usa el cierre de panel) y los huérfanos los limpia ReconcileLive.
        // The retained child worker confirms wait and owns all termination.
    }
}

/// The fields are private to this factory module. A caller cannot manufacture
/// absence from a failed Result, missing handle or a child-wait callback.
pub(crate) struct NoChildAttempt(crate::hook_authority::Claim);
impl NoChildAttempt {
    pub(crate) fn claim(&self) -> crate::hook_authority::Claim {
        self.0
    }
}
struct SpawnAttempt {
    claim: Option<crate::hook_authority::Claim>,
    entered: bool,
    child_attempted: bool,
}
impl SpawnAttempt {
    fn managed(claim: crate::hook_authority::Claim) -> Self {
        Self {
            claim: Some(claim),
            entered: false,
            child_attempted: false,
        }
    }
    fn unmanaged() -> Self {
        Self {
            claim: None,
            entered: false,
            child_attempted: false,
        }
    }
    fn no_child_proof(self) -> Option<NoChildAttempt> {
        (self.entered && !self.child_attempted)
            .then(|| self.claim.map(NoChildAttempt))
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::shell_command;

    fn replay_history_text(term: &super::Term<super::EventProxy>) -> String {
        use alacritty_terminal::grid::Dimensions;
        (-(term.grid().history_size() as i32)..term.screen_lines() as i32)
            .flat_map(|line| {
                term.grid()[alacritty_terminal::index::Line(line)]
                    .into_iter()
                    .map(|cell| cell.c)
            })
            .collect()
    }

    #[test]
    fn rejected_replay_preserves_live_output_and_accepts_one_retry() {
        use super::*;
        let mut handle = PtyHandle::for_persistence_tests(b"LIVE\r\n");
        let pending_before = handle.pending_log_snapshot();
        let attempts = Arc::new(AtomicU64::new(0));
        let attempts_for_spawn = Arc::clone(&attempts);
        handle.set_replay_spawner_for_persistence_tests(move |replay| {
            if attempts_for_spawn.fetch_add(1, Ordering::Relaxed) == 0 {
                return Err(std::io::Error::other("injected replay spawn rejection"));
            }
            replay();
            Ok(())
        });
        let frames = vec![crate::state::scrollback_log::Frame {
            seq: 1,
            kind: crate::state::scrollback_log::FrameKind::Output,
            payload: b"TAIL\r\n".to_vec(),
        }];

        assert!(!handle
            .replay_session_preserving_live(b"OLD\r\n", &frames, |_| { b"LIVE\r\n".to_vec() }));
        assert!(!handle.history_restore_in_progress());
        let before = handle.with_term(|term| replay_history_text(term)).unwrap();
        assert!(before.contains("LIVE"));
        assert!(!before.contains("OLD"));
        assert_eq!(handle.pending_log_snapshot(), pending_before);
        assert!(matches!(
            handle.recovery_snapshot::<String>(true, |_| panic!("OLD is still pending")),
            RecoverySnapshot::PendingLog(bytes) if bytes == pending_before
        ));

        assert!(handle
            .replay_session_preserving_live(b"OLD\r\n", &frames, |_| { b"LIVE\r\n".to_vec() }));
        assert!(!handle.history_restore_in_progress());
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
        let (restored, _) = handle.checkpoint_snapshot(replay_history_text).unwrap();
        assert_eq!(restored.matches("OLD").count(), 1);
        assert_eq!(restored.matches("TAIL").count(), 1);
        assert_eq!(restored.matches("LIVE").count(), 1);
        assert!(restored.find("OLD") < restored.find("TAIL"));
        assert!(restored.find("TAIL") < restored.find("LIVE"));
        assert_eq!(handle.pending_log_snapshot(), pending_before);
    }

    #[test]
    fn activity_counts_live_io_but_not_redraw_resize_empty_or_rejected_input() {
        use super::*;
        let mut handle = PtyHandle::for_persistence_tests(b"");
        assert_eq!(handle.activity_revision(), 0);
        handle.mark_render_dirty();
        handle.resize(60, 20);
        handle.try_write_all(b"").unwrap();
        handle.feed_output_for_persistence_tests(b"");
        assert_eq!(handle.activity_revision(), 0);
        handle.try_write_all(b"accepted input").unwrap();
        assert_eq!(handle.activity_revision(), 1);
        handle.feed_output_for_persistence_tests(b"\r");
        handle.feed_output_for_persistence_tests(b"\r");
        assert_eq!(
            handle.activity_revision(),
            3,
            "identical output still is activity"
        );
        assert!(handle
            .try_write_all(&vec![b'x'; MAX_INPUT_BYTES + 1])
            .is_err());
        assert_eq!(handle.activity_revision(), 3);
        handle.writer.close();
        assert!(handle.try_write_all(b"closed input").is_err());
        assert_eq!(handle.activity_revision(), 3);
    }

    #[test]
    fn replay_and_log_acknowledgements_never_change_live_activity() {
        use super::*;
        let mut handle = PtyHandle::for_persistence_tests(b"LIVE\r\n");
        let live_revision = handle.activity_revision();
        let pending = handle.pending_log_snapshot().len();
        handle.set_replay_spawner_for_persistence_tests(|replay| {
            replay();
            Ok(())
        });
        assert!(handle.replay_session_preserving_live(b"OLD\r\n", &[], |_| b"LIVE\r\n".to_vec()));
        assert!(handle
            .with_term(|term| replay_history_text(term))
            .unwrap()
            .contains("OLD"));
        assert_eq!(handle.activity_revision(), live_revision);
        handle.acknowledge_pending_log(pending);
        assert_eq!(handle.activity_revision(), live_revision);
    }

    #[test]
    fn busy_replay_rejects_a_second_worker_without_releasing_the_first_guard() {
        use super::*;
        let mut handle = PtyHandle::for_persistence_tests(b"LIVE\r\n");
        let (task_tx, task_rx) = mpsc::sync_channel(1);
        handle.set_replay_spawner_for_persistence_tests(move |replay| {
            task_tx.send(replay).unwrap();
            Ok(())
        });
        assert!(
            handle.replay_session_preserving_live(b"OLD\r\n", &[], |_| { b"LIVE\r\n".to_vec() })
        );
        let replay = task_rx.try_recv().unwrap();
        assert!(handle.history_restore_in_progress());

        assert!(!handle.replay_session_preserving_live(b"OTHER\r\n", &[], |_| Vec::new()));
        assert!(handle.history_restore_in_progress());
        assert!(task_rx.try_recv().is_err());
        assert!(handle.checkpoint_snapshot(replay_history_text).is_none());
        replay();
        assert!(!handle.history_restore_in_progress());
        let (restored, _) = handle.checkpoint_snapshot(replay_history_text).unwrap();
        assert_eq!(restored.matches("OLD").count(), 1);
        assert_eq!(restored.matches("LIVE").count(), 1);
        assert!(!restored.contains("OTHER"));
    }

    #[test]
    fn empty_replay_needs_no_worker_and_does_not_clear_live_output() {
        use super::*;
        let mut handle = PtyHandle::for_persistence_tests(b"LIVE\r\n");
        handle.set_replay_spawner_for_persistence_tests(|_| {
            panic!("empty replay must not schedule a worker")
        });
        assert!(handle.replay_session(b"", &[]));
        assert!(!handle.history_restore_in_progress());
        let (live, _) = handle.checkpoint_snapshot(replay_history_text).unwrap();
        assert_eq!(live.matches("LIVE").count(), 1);
    }

    #[test]
    fn failed_replay_after_grid_clear_cannot_checkpoint_a_healthy_partial_grid() {
        use super::*;
        let mut handle = PtyHandle::for_persistence_tests(b"LIVE\r\n");
        let pending_before = handle.pending_log_snapshot();
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _size = handle.window_size.lock().unwrap();
            panic!("injected replay window-size poison");
        }));
        assert!(poisoned.is_err());
        handle.set_replay_spawner_for_persistence_tests(|replay| {
            replay();
            Ok(())
        });

        // The worker was accepted but failed after clearing the live grid.
        assert!(
            handle.replay_session_preserving_live(b"OLD\r\n", &[], |_| { b"LIVE\r\n".to_vec() })
        );
        assert!(!handle.history_restore_in_progress());
        assert!(
            handle.term.lock().is_ok(),
            "the grid itself is not poisoned"
        );
        assert!(handle.checkpoint_snapshot(replay_history_text).is_none());
        assert!(matches!(
            handle.recovery_snapshot::<String>(false, |_| panic!("partial grid is not complete")),
            RecoverySnapshot::PendingLog(bytes) if bytes == pending_before
        ));
        assert!(!handle.replay_session(b"OLD\r\n", &[]));
    }

    #[test]
    fn replay_exporter_panic_releases_busy_state_without_authorizing_a_checkpoint() {
        use super::*;
        let mut handle = PtyHandle::for_persistence_tests(b"LIVE\r\n");
        let pending_before = handle.pending_log_snapshot();
        handle.set_replay_spawner_for_persistence_tests(|replay| {
            replay();
            Ok(())
        });
        assert!(handle.replay_session_preserving_live(b"OLD\r\n", &[], |_| {
            panic!("injected replay exporter panic")
        }));
        assert!(!handle.history_restore_in_progress());
        assert!(handle.term.lock().is_err());
        assert!(handle.checkpoint_snapshot(replay_history_text).is_none());
        assert!(matches!(
            handle.recovery_snapshot::<String>(false, |_| panic!("poisoned grid is not complete")),
            RecoverySnapshot::PendingLog(bytes) if bytes == pending_before
        ));
    }

    #[test]
    fn cold_history_restores_checkpoint_tail_and_current_dimensions() {
        use super::*;
        use crate::state::scrollback_log::{Frame, FrameKind};
        let (tx, _rx) = mpsc::channel();
        let mut term = Term::new(
            TermConfig::default(),
            &TermSize::new(20, 4),
            EventProxy::new(tx),
        );
        let frames = vec![
            Frame {
                seq: 1,
                kind: FrameKind::Resize,
                payload: crate::state::scrollback_log::resize_payload(12, 3).to_vec(),
            },
            Frame {
                seq: 2,
                kind: FrameKind::Output,
                payload: b"tail\r\n\x1b[?1049hTUI".to_vec(),
            },
        ];
        restore_cold_history(&mut term, b"old\r\n", &frames, 20, 4);
        assert_eq!(term.columns(), 20);
        assert_eq!(term.screen_lines(), 4);
        assert!(!term.mode().contains(TermMode::ALT_SCREEN));
        let text: String = term.grid().display_iter().map(|cell| cell.c).collect();
        let history: String = (-(term.grid().history_size() as i32)..term.screen_lines() as i32)
            .flat_map(|line| {
                term.grid()[alacritty_terminal::index::Line(line)]
                    .into_iter()
                    .map(|cell| cell.c)
            })
            .collect();
        assert!(history.contains("old"), "{history:?} {text:?}");
        assert!(history.contains("tail"), "{history:?}");
        assert!(!history.contains("TUI"), "{history:?}");
    }

    #[test]
    fn incremental_sequence_fails_closed_at_u64_max() {
        let sequence = AtomicU64::new(u64::MAX);

        assert_eq!(super::next_log_sequence(&sequence), None);
        assert_eq!(sequence.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn shell_memory_identity_is_stable_and_distinct_between_leaves() {
        let panel = uuid::Uuid::new_v4();
        let leaf = uuid::Uuid::new_v4();
        let hooks = super::HookIdentity {
            panel_id: Some(panel),
            leaf_id: Some(leaf),
            ..Default::default()
        };
        let command = shell_command(None, hooks);
        assert_eq!(
            command.get_env("TC_MEMORY_TASK_ID"),
            Some(std::ffi::OsStr::new(&leaf.to_string()))
        );
        assert_eq!(hooks.memory_task_id(uuid::Uuid::new_v4()), leaf);
        assert_ne!(
            super::HookIdentity {
                leaf_id: Some(uuid::Uuid::new_v4()),
                ..hooks
            }
            .memory_task_id(leaf),
            leaf
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_command_uses_login_shell_on_unix() {
        let command = shell_command(None, super::HookIdentity::default());

        assert!(command.is_default_prog());
    }

    #[cfg(windows)]
    #[test]
    fn shell_command_uses_explicit_shell_on_windows() {
        let command = shell_command(None, super::HookIdentity::default());

        assert!(!command.is_default_prog());
        assert_eq!(command.get_argv().len(), 1);
    }

    #[test]
    fn shell_command_preserves_cwd_and_terminal_env() {
        let cwd = Path::new("/tmp");
        let command = shell_command(Some(cwd), super::HookIdentity::default());

        assert_eq!(command.get_cwd(), Some(&OsString::from(cwd)));
        assert_eq!(command.get_env("TERM"), Some("xterm-256color".as_ref()));
        assert_eq!(command.get_env("COLORTERM"), Some("truecolor".as_ref()));
        assert_eq!(command.get_env("MI_TERMINAL"), Some("1".as_ref()));
        assert_eq!(command.get_env("TC_MEMORY_ROOT"), Some(cwd.as_os_str()));
    }

    #[test]
    fn drain_pending_log_capped_respects_frame_boundaries() {
        use crate::state::scrollback_log::{encode_frame, read_frames, FrameKind};

        let frame_a = encode_frame(1, FrameKind::Output, b"aaaa"); // 13 + 4 = 17 bytes
        let frame_b = encode_frame(2, FrameKind::Output, b"bbbb"); // 17 bytes
        let frame_c = encode_frame(3, FrameKind::Output, b"cccc"); // 17 bytes
        let mut all = Vec::new();
        all.extend_from_slice(&frame_a);
        all.extend_from_slice(&frame_b);
        all.extend_from_slice(&frame_c);
        assert_eq!(all.len(), 51);

        // Drenar solo 20 bytes: cabe el frame_a (17B) pero no el frame_b.
        let drained = super::drain_log_prefix(&mut all, 20);
        assert_eq!(drained.len(), 17, "solo cabe el primer frame en 20 bytes");
        let (_, frames) = read_frames(
            &[
                crate::state::scrollback_log::encode_header(0),
                drained.clone(),
            ]
            .concat(),
        )
        .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload, b"aaaa");
    }

    #[test]
    fn capped_log_drain_makes_progress_on_an_oversized_frame() {
        use crate::state::scrollback_log::{encode_frame, FrameKind};
        let first = encode_frame(1, FrameKind::Output, &vec![b'x'; 65_536]);
        let second = encode_frame(2, FrameKind::Output, b"after");
        let mut pending = [first.clone(), second.clone()].concat();
        assert!(super::drain_log_prefix(&mut pending, 0).is_empty());
        assert_eq!(super::drain_log_prefix(&mut pending, 32 * 1024), first);
        assert_eq!(super::drain_log_prefix(&mut pending, 32 * 1024), second);
        assert!(pending.is_empty());
    }
    #[test]
    fn shell_command_exports_panel_workspace_and_leaf_identity() {
        let panel = uuid::Uuid::new_v4();
        let workspace = uuid::Uuid::new_v4();
        let leaf = uuid::Uuid::new_v4();
        let command = shell_command(
            None,
            super::HookIdentity {
                panel_id: Some(panel),
                workspace_id: Some(workspace),
                leaf_id: Some(leaf),
                memory_task_id: None,
                hook_generation: None,
                hook_owner: None,
            },
        );

        assert_eq!(
            command.get_env("TC_PANEL_ID"),
            Some(std::ffi::OsStr::new(&panel.to_string()))
        );
        assert_eq!(
            command.get_env("TC_WORKSPACE_ID"),
            Some(std::ffi::OsStr::new(&workspace.to_string()))
        );
        assert_eq!(
            command.get_env("TC_LEAF_ID"),
            Some(std::ffi::OsStr::new(&leaf.to_string()))
        );
    }

    #[test]
    fn fallible_attach_refusal_retains_pending_frames_and_log_sequence() {
        use super::*;
        let handle = PtyHandle::for_persistence_tests(b"LIVE\r\n");
        let pending = handle.pending_log_snapshot();
        let sequence = handle.log_seq.load(Ordering::Relaxed);
        let cursor = handle.term.lock().unwrap().grid().cursor.point;
        let refused = handle.try_attach_snapshot_and_drain::<(), &str>(|_| Err("wire budget"));
        assert_eq!(refused, Err("wire budget"));
        assert_eq!(handle.pending_log_snapshot(), pending);
        assert_eq!(handle.log_seq.load(Ordering::Relaxed), sequence);
        assert_eq!(handle.term.lock().unwrap().grid().cursor.point, cursor);
    }

    #[test]
    fn fallible_attach_success_drains_its_matching_frames_exactly_once() {
        use super::*;
        let handle = PtyHandle::for_persistence_tests(b"LIVE\r\n");
        let pending = handle.pending_log_snapshot();
        let (snapshot, frames) = handle
            .try_attach_snapshot_and_drain::<_, &str>(|term| {
                Ok(crate::terminal::export::live_snapshot_with_terminal_state(
                    term,
                ))
            })
            .unwrap()
            .unwrap();
        assert!(snapshot.contains("LIVE"));
        assert_eq!(frames, pending);
        assert!(handle.pending_log_snapshot().is_empty());
        assert_eq!(handle.log_seq.load(Ordering::Relaxed), 0);
        let (_, second) = handle
            .try_attach_snapshot_and_drain::<_, &str>(|_| Ok(()))
            .unwrap()
            .unwrap();
        assert!(second.is_empty());
    }

    #[test]
    fn fallible_primary_repair_refusal_retains_the_complete_alternate_exit_frames() {
        use super::*;
        let handle = PtyHandle::for_persistence_tests(b"PRIMARY\r\n\x1b[?1049hTUI");
        handle.feed_output_for_persistence_tests(b"\x1b[?1049l\r\nRETURNED");
        assert!(!handle
            .term
            .lock()
            .unwrap()
            .mode()
            .contains(TermMode::ALT_SCREEN));
        let pending = handle.pending_log_snapshot();
        let sequence = handle.log_seq.load(Ordering::Relaxed);
        let refused = handle.try_output_update(1, true, |_| Err::<String, _>("repair budget"));
        assert_eq!(refused, Err("repair budget"));
        assert_eq!(handle.pending_log_snapshot(), pending);
        assert_eq!(handle.log_seq.load(Ordering::Relaxed), sequence);
        let (frames, alternate, repair) = handle
            .try_output_update(1, true, |term| {
                Ok::<_, &str>(crate::terminal::export::live_snapshot_with_terminal_state(
                    term,
                ))
            })
            .unwrap()
            .unwrap();
        assert_eq!(frames, pending);
        assert!(!alternate);
        let repair = repair.unwrap();
        assert!(repair.contains("PRIMARY"));
        assert!(repair.contains("RETURNED"));
        assert!(handle.pending_log_snapshot().is_empty());
    }

    #[test]
    fn fallible_normal_output_capture_uses_frame_budget_without_requesting_repair() {
        use super::*;
        let handle = PtyHandle::for_persistence_tests(b"FIRST");
        let prefix = handle.pending_log_snapshot();
        handle.feed_output_for_persistence_tests(b"SECOND");
        let (frames, alternate, repair) = handle
            .try_output_update(prefix.len(), false, |_| -> Result<String, &str> {
                panic!("no primary repair")
            })
            .unwrap()
            .unwrap();
        assert_eq!(frames, prefix);
        assert!(!alternate);
        assert!(repair.is_none());
        assert!(!handle.pending_log_snapshot().is_empty());
        let (remaining, _, _) = handle
            .try_output_update(usize::MAX, false, |_| -> Result<String, &str> {
                panic!("no primary repair")
            })
            .unwrap()
            .unwrap();
        assert!(!remaining.is_empty());
        assert!(handle.pending_log_snapshot().is_empty());
    }
}

#[cfg(test)]
mod full_attach_retirement_fixtures {
    use super::*;

    #[test]
    fn actual_reads_survive_parser_panic_and_poison_until_real_eof() {
        let handle = PtyHandle::for_persistence_tests(b"");
        let reader_state = ReaderRetirement::default();
        let mut reader = &b"HEADTAIL"[..];
        let mut buffer = [0; 4];
        assert!(matches!(
            reader_state.read_and_retain(&mut reader, &mut buffer, |raw| {
                retain_raw_read_for_reader(
                    &handle.term,
                    &handle.pending_log,
                    &handle.log_seq,
                    raw,
                    |_| panic!("parser after raw retention"),
                )
                .map(|_| ())
            }),
            ReadCompletion::Data
        ));
        assert!(handle.term.is_poisoned());
        assert!(matches!(
            reader_state.read_and_retain(&mut reader, &mut buffer, |raw| {
                retain_raw_read_for_reader(
                    &handle.term,
                    &handle.pending_log,
                    &handle.log_seq,
                    raw,
                    |_| panic!("poisoned parser must not run"),
                )
                .map(|_| ())
            }),
            ReadCompletion::Data
        ));
        assert!(!reader_state.eof());
        assert!(matches!(
            reader_state.read_and_retain(&mut reader, &mut buffer, |_| panic!("EOF has no raw")),
            ReadCompletion::Eof
        ));
        let (_, frames) = crate::state::scrollback_log::read_frames(
            &[
                crate::state::scrollback_log::encode_header(0),
                handle.retirement_log_snapshot(),
            ]
            .concat(),
        )
        .unwrap();
        assert_eq!(
            frames
                .iter()
                .map(|frame| frame.payload.clone())
                .collect::<Vec<_>>(),
            [b"HEAD".to_vec(), b"TAIL".to_vec()]
        );
    }

    #[test]
    fn legacy_destructive_drain_has_no_strong_retirement_certificate() {
        let mut handle = PtyHandle::for_persistence_tests(b"UNACKNOWLEDGED");
        assert!(!handle.drain_pending_log().is_empty());
        let mut durable = Vec::new();
        let mut sequence = 0;
        assert!(handle
            .take_retirement_prefix(&mut durable, &mut sequence, true)
            .unwrap());
        handle.begin_retirement_drain().unwrap();
        assert!(!handle.retirement_physical_complete());
        assert!(handle.drain_pending_log().is_empty());
    }

    #[test]
    fn retirement_rejects_truncated_raw_without_transferring_a_valid_prefix() {
        let handle = PtyHandle::for_persistence_tests(b"KEEP");
        handle
            .pending_log
            .lock()
            .unwrap()
            .extend_from_slice(&[1, 2, 3]);
        let before = handle.retirement_log_snapshot();
        let mut durable = Vec::new();
        let mut sequence = 0;
        assert!(handle
            .take_retirement_prefix(&mut durable, &mut sequence, true)
            .is_err());
        assert_eq!(handle.retirement_log_snapshot(), before);
        assert!(durable.is_empty());
        assert_eq!(sequence, 0);
    }
    #[test]
    fn retirement_waits_for_an_admitted_read_and_fences_the_next() {
        let state = ReaderRetirement::default();
        assert!(state.begin_read());
        state.request();
        assert!(!state.quiescent());
        assert!(!state.begin_read());
        state.finish_read();
        assert!(state.quiescent());
        assert!(!state.begin_read());
    }
    #[test]
    fn retirement_snapshot_copies_without_ack_or_sequence_reset() {
        let handle = PtyHandle::for_persistence_tests(b"FINAL RAW");
        let before = handle.retirement_log_snapshot();
        let seq = handle.log_seq.load(Ordering::Relaxed);
        assert_eq!(
            handle.retirement_pending_snapshot().unwrap(),
            Some(before.clone())
        );
        assert_eq!(handle.retirement_log_snapshot(), before);
        assert_eq!(handle.log_seq.load(Ordering::Relaxed), seq);
    }
    #[test]
    fn poisoned_pending_log_never_acknowledges_retirement() {
        let handle = PtyHandle::for_persistence_tests(b"KEEP");
        handle.poison_pending_log_for_persistence_tests();
        assert!(handle.retirement_pending_snapshot().is_err());
    }
    #[test]
    fn durable_pressure_is_charged_until_a_host_ack() {
        let handle = PtyHandle::for_persistence_tests(b"RAW");
        handle.set_daemon_durable_pressure(0);
        let pending = handle.retirement_log_snapshot().len();
        assert_eq!(handle.drain_pending_log_capped(usize::MAX).len(), pending);
        assert_eq!(handle.durable_pressure.load(Ordering::Acquire), pending);
        handle.set_daemon_durable_pressure(0);
        assert_eq!(handle.durable_pressure.load(Ordering::Acquire), 0);
    }
}

#[cfg(test)]
mod raw_before_parser_fixtures {
    use super::*;
    fn payloads(frames: &[u8]) -> Vec<Vec<u8>> {
        let encoded = [
            crate::state::scrollback_log::encode_header(0),
            frames.to_vec(),
        ]
        .concat();
        crate::state::scrollback_log::read_frames(&encoded)
            .unwrap()
            .1
            .into_iter()
            .map(|frame| frame.payload)
            .collect()
    }
    #[test]
    fn parser_panic_keeps_the_actual_admitted_read_for_durable_retirement() {
        let handle = PtyHandle::for_persistence_tests(b"");
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = retain_raw_read_and_parse(
                &handle.term,
                &handle.pending_log,
                &handle.log_seq,
                b"PHYSICAL RAW",
                |_| panic!("parser panic after read"),
            );
        }));
        assert!(panic.is_err());
        assert_eq!(
            payloads(&handle.retirement_pending_snapshot().unwrap().unwrap()),
            vec![b"PHYSICAL RAW".to_vec()]
        );
        assert!(!handle.semantic_output_ready());
    }
    #[test]
    fn poisoned_grid_keeps_new_raw_without_reusing_the_failed_parser() {
        let handle = PtyHandle::for_persistence_tests(b"");
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = handle.term.lock().unwrap();
            panic!("poison grid");
        }));
        let result = retain_raw_read_and_parse(
            &handle.term,
            &handle.pending_log,
            &handle.log_seq,
            b"AFTER POISON",
            |_| panic!("poisoned parser must not run"),
        )
        .unwrap();
        assert!(result.is_none());
        assert_eq!(
            payloads(&handle.retirement_pending_snapshot().unwrap().unwrap()),
            vec![b"AFTER POISON".to_vec()]
        );
    }
    #[test]
    fn raw_sequence_exhaustion_never_acknowledges_complete_retirement() {
        let handle = PtyHandle::for_persistence_tests(b"");
        handle.log_seq.store(u64::MAX, Ordering::Release);
        assert!(retain_raw_read_and_parse(
            &handle.term,
            &handle.pending_log,
            &handle.log_seq,
            b"CANNOT NUMBER",
            |_| ()
        )
        .is_err());
        assert!(handle.retirement_pending_snapshot().is_err());
    }
    #[test]
    fn physical_metadata_is_retained_without_visible_startup_readiness() {
        let handle = PtyHandle::for_persistence_tests(b"");
        let raw = b"\x1b]7;file:///tmp/raw-metadata\x07";
        let mut agent = AgentStatusStream::new();
        let (clean, _, _) = retain_raw_read_and_parse(
            &handle.term,
            &handle.pending_log,
            &handle.log_seq,
            raw,
            |_| agent.process(raw, 1),
        )
        .unwrap()
        .unwrap();
        assert!(clean.is_empty());
        assert!(!handle.semantic_output_ready());
        assert_eq!(
            payloads(&handle.retirement_pending_snapshot().unwrap().unwrap()),
            vec![raw.to_vec()]
        );
    }
}
