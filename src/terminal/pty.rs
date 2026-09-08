use std::io::{Read, Write};
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
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use uuid::Uuid;

fn next_log_sequence(counter: &AtomicU64) -> Option<u64> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .ok()
        .and_then(|previous| previous.checked_add(1))
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

fn yield_for_reader_priority(scheduler: &SharedRuntimeScheduler, session_id: Uuid) {
    let delay = scheduler
        .lock()
        .map(|scheduler| scheduler.reader_delay(session_id))
        .unwrap_or_default();
    if !delay.is_zero() {
        thread::sleep(delay);
    }
}

pub struct PtyHandle {
    pub term: Arc<Mutex<Term<EventProxy>>>,
    title: Arc<ArcSwap<String>>,
    pub alive: Arc<AtomicBool>,
    pub bell_fired: Arc<AtomicBool>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    last_output_at: Arc<AtomicI64>,
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
    scrollback_limit: usize,
    #[cfg(feature = "ghostty-vt")]
    backend_kind: TerminalBackendKind,
    #[cfg(feature = "ghostty-vt")]
    ghostty_runtime: Option<GhosttyRuntimeHandle>,
    /// `None` en una sesión hosteada por el daemon: el PTY real vive allá y
    /// el resize se manda por el socket (P3.15, T3).
    master: Option<Box<dyn MasterPty + Send>>,
    killer: Option<Box<dyn ChildKiller + Send + Sync>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    /// Sesión del daemon a la que este handle está atado, si es remota.
    #[cfg(all(unix, feature = "daemon"))]
    remote: Option<RemoteLink>,
    /// El daemon ya replayó el historial de una sesión preexistente. El store
    /// local no debe inyectar además el mismo checkpoint.
    hot_reattached: bool,
    _reader_thread: thread::JoinHandle<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalScrollState {
    pub display_offset: usize,
    pub visible_rows: usize,
    pub history_size: usize,
}

impl PtyHandle {
    pub fn spawn(
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
        session_id: Uuid,
        scheduler: SharedRuntimeScheduler,
        hooks: HookIdentity,
    ) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let cmd = shell_command(cwd, hooks);

        let child = pair.slave.spawn_command(cmd).context("spawn PTY child")?;
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().context("clone PTY reader")?;
        let writer = pair.master.take_writer().context("take PTY writer")?;

        let title = Arc::new(ArcSwap::from_pointee("Terminal".to_owned()));
        let alive = Arc::new(AtomicBool::new(true));
        let bell_fired = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicI64::new(pty_clock_now_ms()));
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
        let writer_for_reader = Arc::new(Mutex::new(writer));
        let writer_for_thread = Arc::clone(&writer_for_reader);
        let output_for_reader = Arc::clone(&last_output_at);
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
                    wait_for_history_restore(&restoring_for_reader);
                    yield_for_reader_priority(&scheduler_for_reader, session_id);
                    let pending_len = pending_log_for_reader
                        .lock()
                        .map(|pending| pending.len())
                        .unwrap_or(0);
                    if matches!(
                        gate.update(pending_len, std::time::Instant::now()),
                        crate::terminal::flow_control::FlowState::Pause
                    ) {
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(read) => {
                            // La lectura puede haberse desbloqueado justo
                            // después de iniciar el restore. Conservamos estos
                            // bytes, pero no los parseamos hasta terminarlo.
                            wait_for_history_restore(&restoring_for_reader);
                            let now_ms = pty_clock_now_ms();
                            let (clean, agent_reports, cwd_reports) =
                                agent_stream.process(&buf[..read], now_ms);
                            if let Some(report) = agent_reports.into_iter().next_back() {
                                agent_status_for_reader.store(Arc::new(Some(report)));
                            }
                            if let Some(new_cwd) = cwd_reports.into_iter().next_back() {
                                cwd_for_reader.store(Arc::new(Some(new_cwd)));
                            }
                            if let Ok(mut term) = term_for_reader.lock() {
                                processor.advance(&mut *term, &clean);
                            }
                            // Log incremental (P1.7): el mismo byte-stream que
                            // alimenta el grid queda pendiente de appendear.
                            if !clean.is_empty() {
                                if let Ok(mut pending) = pending_log_for_reader.lock() {
                                    if let Some(seq) = next_log_sequence(&log_seq_for_reader) {
                                        pending.extend_from_slice(
                                            &crate::state::scrollback_log::encode_frame(
                                                seq,
                                                crate::state::scrollback_log::FrameKind::Output,
                                                &clean,
                                            ),
                                        );
                                    } else {
                                        log::error!("se agotó la secuencia incremental del PTY");
                                    }
                                }
                            }
                            #[cfg(feature = "ghostty-vt")]
                            if let Some(ghostty) = &ghostty_for_reader {
                                ghostty.feed(&clean);
                            }
                            render_revision_for_reader.fetch_add(1, Ordering::Relaxed);
                            output_for_reader.store(now_ms, Ordering::Relaxed);
                            if let Ok(mut scheduler) = scheduler_for_reader.lock() {
                                scheduler.record_output(session_id);
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
                        Err(_) => break,
                    }
                }
            }));
            if loop_result.is_err() {
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
            window_size,
            render_revision,
            agent_status,
            cwd,
            pending_log,
            log_seq,
            restoring_history,
            scrollback_limit,
            #[cfg(feature = "ghostty-vt")]
            backend_kind,
            #[cfg(feature = "ghostty-vt")]
            ghostty_runtime,
            master: Some(pair.master),
            killer: Some(killer),
            child: Some(child),
            #[cfg(all(unix, feature = "daemon"))]
            remote: None,
            hot_reattached: false,
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
        events: std::os::unix::net::UnixStream,
        snapshot: &[u8],
        attached_seq: u64,
        hot_reattached: bool,
        cols: u16,
        rows: u16,
        scheduler: SharedRuntimeScheduler,
    ) -> anyhow::Result<Self> {
        use crate::terminal::remote_session::{RemoteReader, RemoteWriter};

        let link = RemoteLink::new(session_id, control);
        let title = Arc::new(ArcSwap::from_pointee("Terminal".to_owned()));
        let alive = Arc::new(AtomicBool::new(true));
        let bell_fired = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicI64::new(pty_clock_now_ms()));
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
        if !snapshot.is_empty() {
            if let Ok(mut term) = term.lock() {
                let mut processor = Processor::<StdSyncHandler>::new();
                processor.advance(&mut *term, snapshot);
            }
        }

        let writer: Box<dyn Write + Send> = Box::new(RemoteWriter::new(link.clone()));
        let writer_for_reader = Arc::new(Mutex::new(writer));
        let writer_for_thread = Arc::clone(&writer_for_reader);
        let title_for_reader = Arc::clone(&title);
        let alive_for_reader = Arc::clone(&alive);
        let bell_for_reader = Arc::clone(&bell_fired);
        let output_for_reader = Arc::clone(&last_output_at);
        let term_for_reader = Arc::clone(&term);
        let window_size_for_reader = Arc::clone(&window_size);
        let render_revision_for_reader = Arc::clone(&render_revision);
        let agent_status_for_reader = Arc::clone(&agent_status);
        let cwd_for_reader = Arc::clone(&cwd);
        let scheduler_for_reader = Arc::clone(&scheduler);
        let restoring_for_reader = Arc::clone(&restoring_history);

        let reader_thread = thread::spawn(move || {
            let loop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut processor = Processor::<StdSyncHandler>::new();
                let mut agent_stream = AgentStatusStream::new();
                let mut reader = RemoteReader::new(events, session_id, attached_seq);
                loop {
                    wait_for_history_restore(&restoring_for_reader);
                    yield_for_reader_priority(&scheduler_for_reader, session_id);
                    let Some(bytes) = reader.next_output() else {
                        break;
                    };
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
                    render_revision_for_reader.fetch_add(1, Ordering::Relaxed);
                    output_for_reader.store(now_ms, Ordering::Relaxed);
                    if let Ok(mut scheduler) = scheduler_for_reader.lock() {
                        scheduler.record_output(session_id);
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
            window_size,
            render_revision,
            agent_status,
            cwd,
            pending_log,
            log_seq,
            restoring_history,
            scrollback_limit,
            #[cfg(feature = "ghostty-vt")]
            backend_kind: TerminalBackendKind::Alacritty,
            #[cfg(feature = "ghostty-vt")]
            ghostty_runtime: None,
            master: None,
            killer: None,
            child: None,
            remote: Some(link),
            hot_reattached,
            _reader_thread: reader_thread,
        })
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
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
        }
        // Log incremental (P1.7): un resize real se registra para que el
        // replay reaplique el tamaño antes de seguir con el output.
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

    /// `true` sólo cuando este grid ya recibió el snapshot de una sesión que
    /// sobrevivió a una corrida anterior de la app.
    pub fn was_hot_reattached(&self) -> bool {
        self.hot_reattached
    }

    pub fn write_all(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            if writer.write_all(bytes).is_ok() {
                let _ = writer.flush();
            }
        }
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

    #[cfg(feature = "ghostty-vt")]
    pub fn backend_kind(&self) -> TerminalBackendKind {
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
        if self.restoring_history.load(Ordering::Acquire) {
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

    /// Drena los frames de log incremental acumulados desde el último autosave
    /// (P1.7). Devuelve los bytes ya encodeados, listos para appendear.
    pub fn drain_pending_log(&self) -> Vec<u8> {
        self.pending_log
            .lock()
            .map(|mut pending| {
                let drained = std::mem::take(&mut *pending);
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
        self.pending_log
            .lock()
            .map(|mut pending| {
                // Caminar frames completos hasta agotar el presupuesto.
                // Cada frame: 8B seq + 1B kind + 4B len + payload.
                let mut cut = 0;
                while cut + 13 <= pending.len() && cut + 13 <= max_bytes {
                    let payload_len = u32::from_le_bytes([
                        pending[cut + 9],
                        pending[cut + 10],
                        pending[cut + 11],
                        pending[cut + 12],
                    ]) as usize;
                    let frame_end = cut + 13 + payload_len;
                    if frame_end > max_bytes || frame_end > pending.len() {
                        break;
                    }
                    cut = frame_end;
                }
                let drained = pending.drain(..cut).collect();
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
        self.pending_log
            .lock()
            .map(|pending| pending.clone())
            .unwrap_or_default()
    }

    pub fn acknowledge_pending_log(&self, written_bytes: usize) {
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
        if self.restoring_history.load(Ordering::Acquire) {
            return None;
        }
        let term = self.term.lock().ok()?;
        let pending_bytes = self.pending_log.lock().ok()?.len();
        let text = export(&term);
        Some((text, pending_bytes))
    }

    /// Crea la frontera atómica de un hot attach. El snapshot del grid y los
    /// frames que todavía no recibieron `seq` se toman bajo el mismo orden de
    /// locks que usa el reader (`term` → `pending_log`), de modo que ningún
    /// byte pueda aparecer en el snapshot con un `seq` anterior.
    pub fn attach_snapshot_and_drain<R>(
        &self,
        export: impl FnOnce(&Term<EventProxy>) -> R,
    ) -> Option<(R, Vec<u8>)> {
        if self.restoring_history.load(Ordering::Acquire) {
            return None;
        }
        let term = self.term.lock().ok()?;
        let mut pending = self.pending_log.lock().ok()?;
        let snapshot = export(&term);
        let frames = std::mem::take(&mut *pending);
        self.log_seq.store(0, Ordering::Relaxed);
        Some((snapshot, frames))
    }

    /// Restaura checkpoint + frames del log incremental en orden (P1.7).
    /// Los resize se aplican como `term.resize` antes de seguir el replay del
    /// output posterior, para que el grid tenga el tamaño correcto.
    pub fn replay_session(
        &self,
        checkpoint: &[u8],
        frames: &[crate::state::scrollback_log::Frame],
    ) {
        self.replay_session_preserving_live(checkpoint, frames, |_| Vec::new());
    }

    /// Variante usada por la app real: recibe el exportador semántico desde
    /// el módulo host para que los harness que montan `pty.rs` por `#[path]`
    /// no tengan que incluir todo `terminal::export`.
    pub fn replay_session_preserving_live(
        &self,
        checkpoint: &[u8],
        frames: &[crate::state::scrollback_log::Frame],
        export_live: impl FnOnce(&Term<EventProxy>) -> Vec<u8> + Send + 'static,
    ) {
        if checkpoint.is_empty() && frames.is_empty() {
            return;
        }
        if self
            .restoring_history
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let checkpoint = checkpoint.to_vec();
        let frames = frames.to_vec();
        let term = Arc::clone(&self.term);
        let restoring = Arc::clone(&self.restoring_history);
        let restoring_for_thread = Arc::clone(&restoring);
        let render_revision = Arc::clone(&self.render_revision);
        let spawn_result = thread::Builder::new()
            .name("scrollback-replay".to_owned())
            .spawn(move || {
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
                            return;
                        };
                        let live = export_live(&term);
                        processor.advance(&mut *term, b"\x1b[2J\x1b[3J");
                        live
                    };
                    for chunk in checkpoint.chunks(CHUNK_BYTES) {
                        let Ok(mut term) = term.lock() else {
                            return;
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
                                        return;
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
                                        return;
                                    };
                                    term.resize(TermSize::new(cols as usize, rows as usize));
                                }
                            }
                            crate::state::scrollback_log::FrameKind::Clear => {
                                let Ok(mut term) = term.lock() else {
                                    return;
                                };
                                processor.advance(&mut *term, b"\x1b[2J\x1b[3J");
                            }
                        }
                    }
                    if let Ok(mut term) = term.lock() {
                        processor
                            .advance(&mut *term, &crate::state::scrollback_store::replay_marker());
                    }
                    for chunk in live_output.chunks(CHUNK_BYTES) {
                        let Ok(mut term) = term.lock() else {
                            return;
                        };
                        processor.advance(&mut *term, chunk);
                        drop(term);
                        thread::yield_now();
                    }
                }));
                if replay_result.is_err() {
                    log::error!("el replay de scrollback paniqueó");
                }
                render_revision.fetch_add(1, Ordering::Release);
                restoring_for_thread.store(false, Ordering::Release);
            });
        if let Err(err) = spawn_result {
            restoring.store(false, Ordering::Release);
            log::warn!("no se pudo iniciar el replay de scrollback: {err}");
        }
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

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
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
    cmd
}

/// Identidad que se exporta al shell para que los hooks sepan de qué panel
/// vienen (P2.12).
#[derive(Debug, Clone, Copy, Default)]
pub struct HookIdentity {
    pub panel_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub leaf_id: Option<Uuid>,
}

fn drain_terminal_events(
    event_rx: &mpsc::Receiver<Event>,
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
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
                if let Ok(mut writer) = writer.lock() {
                    let _ = writer.write_all(text.as_bytes());
                    let _ = writer.flush();
                }
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
                            if let Ok(mut writer) = writer.lock() {
                                let _ = writer.write_all(formatter(&text).as_bytes());
                                let _ = writer.flush();
                            }
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
                    if let Ok(mut writer) = writer.lock() {
                        let _ = writer.write_all(formatter(rgb).as_bytes());
                        let _ = writer.flush();
                    }
                }
            }
            Event::TextAreaSizeRequest(formatter) => {
                if let Ok(size) = window_size.lock().map(|guard| *guard) {
                    if let Ok(mut writer) = writer.lock() {
                        let _ = writer.write_all(formatter(size).as_bytes());
                        let _ = writer.flush();
                    }
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
        self.alive.store(false, Ordering::Relaxed);
        if let Some(killer) = self.killer.as_mut() {
            let _ = killer.kill();
        }
        // Ojo: una sesión remota **no** se mata al dropear el handle. Cerrar la
        // app dropea todos los handles, y matar ahí sería justo lo contrario de
        // lo que el daemon promete. Matarla es explícito (`kill_remote_session`,
        // que usa el cierre de panel) y los huérfanos los limpia ReconcileLive.
        // Reap the child off-thread: without a wait() every closed terminal
        // leaves a zombie process, and long sessions with many terminals
        // eventually exhaust the process table.
        if let Some(mut child) = self.child.take() {
            let _ = thread::Builder::new()
                .name("pty-reaper".to_owned())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::shell_command;

    #[test]
    fn incremental_sequence_fails_closed_at_u64_max() {
        let sequence = AtomicU64::new(u64::MAX);

        assert_eq!(super::next_log_sequence(&sequence), None);
        assert_eq!(sequence.load(Ordering::Relaxed), u64::MAX);
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
        let pending = std::sync::Arc::new(std::sync::Mutex::new(all.clone()));
        let drained = {
            let mut pending = pending.lock().unwrap();
            let mut cut = 0;
            let max_bytes = 20;
            while cut + 13 <= pending.len() && cut + 13 <= max_bytes {
                let payload_len = u32::from_le_bytes([
                    pending[cut + 9],
                    pending[cut + 10],
                    pending[cut + 11],
                    pending[cut + 12],
                ]) as usize;
                let frame_end = cut + 13 + payload_len;
                if frame_end > max_bytes || frame_end > pending.len() {
                    break;
                }
                cut = frame_end;
            }
            let drained: Vec<u8> = pending.drain(..cut).collect();
            drained
        };
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
}
