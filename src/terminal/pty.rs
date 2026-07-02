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

use crate::runtime::SharedRuntimeScheduler;
#[cfg(feature = "ghostty-vt")]
use crate::terminal::backend::{runtime_backend_from_env, TerminalBackendKind};
use crate::terminal::colors::indexed_to_egui;
#[cfg(feature = "ghostty-vt")]
use crate::terminal::ghostty_backend::{GhosttyRuntimeHandle, GhosttyTextSnapshot};
use crate::terminal::input::InputMode;

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
    std::env::var("MI_TERMINAL_ALLOW_OSC52")
        .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn pty_clock_epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn pty_clock_now_ms() -> i64 {
    Instant::now()
        .saturating_duration_since(pty_clock_epoch())
        .as_millis() as i64
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
    scrollback_limit: usize,
    #[cfg(feature = "ghostty-vt")]
    backend_kind: TerminalBackendKind,
    #[cfg(feature = "ghostty-vt")]
    ghostty_runtime: Option<GhosttyRuntimeHandle>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    child: Option<Box<dyn Child + Send + Sync>>,
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
    ) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let cmd = shell_command(cwd);

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
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let term_config = TermConfig::default();
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
        let scheduler_for_reader = Arc::clone(&scheduler);
        let reader_thread = thread::spawn(move || {
            // The parser processes untrusted terminal output; if it ever
            // panics, contain the damage to this session instead of taking
            // down the whole app, and leave the session marked as exited.
            let loop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut buf = vec![0_u8; 65_536];
                let mut processor = Processor::<StdSyncHandler>::new();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(read) => {
                            if let Ok(mut term) = term_for_reader.lock() {
                                processor.advance(&mut *term, &buf[..read]);
                            }
                            #[cfg(feature = "ghostty-vt")]
                            if let Some(ghostty) = &ghostty_for_reader {
                                ghostty.feed(&buf[..read]);
                            }
                            render_revision_for_reader.fetch_add(1, Ordering::Relaxed);
                            output_for_reader.store(pty_clock_now_ms(), Ordering::Relaxed);
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
            scrollback_limit,
            #[cfg(feature = "ghostty-vt")]
            backend_kind,
            #[cfg(feature = "ghostty-vt")]
            ghostty_runtime,
            master: pair.master,
            killer,
            child: Some(child),
            _reader_thread: reader_thread,
        })
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut window_size) = self.window_size.lock() {
            *window_size = WindowSize {
                num_lines: rows,
                num_cols: cols,
                cell_width: 0,
                cell_height: 0,
            };
        }
        if let Ok(mut term) = self.term.lock() {
            term.resize(TermSize::new(cols as usize, rows as usize));
        }
        #[cfg(feature = "ghostty-vt")]
        if let Some(ghostty) = &self.ghostty_runtime {
            ghostty.resize(cols, rows);
        }
        self.mark_render_dirty();
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
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
        }
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
        let mut term = self.term.try_lock().ok()?;
        Some(f(&mut term))
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

fn shell_command(cwd: Option<&Path>) -> CommandBuilder {
    #[cfg(unix)]
    let mut cmd = CommandBuilder::new_default_prog();
    #[cfg(windows)]
    let mut cmd = CommandBuilder::new(default_shell());

    if let Some(cwd) = cwd {
        cmd.cwd(cwd);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("MI_TERMINAL", "1");
    cmd
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
        let _ = self.killer.kill();
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

    use super::shell_command;

    #[cfg(unix)]
    #[test]
    fn shell_command_uses_login_shell_on_unix() {
        let command = shell_command(None);

        assert!(command.is_default_prog());
    }

    #[cfg(windows)]
    #[test]
    fn shell_command_uses_explicit_shell_on_windows() {
        let command = shell_command(None);

        assert!(!command.is_default_prog());
        assert_eq!(command.get_argv().len(), 1);
    }

    #[test]
    fn shell_command_preserves_cwd_and_terminal_env() {
        let cwd = Path::new("/tmp");
        let command = shell_command(Some(cwd));

        assert_eq!(command.get_cwd(), Some(&OsString::from(cwd)));
        assert_eq!(command.get_env("TERM"), Some("xterm-256color".as_ref()));
        assert_eq!(command.get_env("COLORTERM"), Some("truecolor".as_ref()));
        assert_eq!(command.get_env("MI_TERMINAL"), Some("1".as_ref()));
    }
}
