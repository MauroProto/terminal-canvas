use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use anyhow::Context as _;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use uuid::Uuid;

use crate::runtime::SharedRuntimeScheduler;
use crate::terminal::colors::indexed_to_egui;
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

pub struct PtyHandle {
    pub term: Arc<Mutex<Term<EventProxy>>>,
    pub title: Arc<Mutex<String>>,
    pub alive: Arc<AtomicBool>,
    pub bell_fired: Arc<AtomicBool>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub last_input_at: Arc<Mutex<Instant>>,
    pub last_output_at: Arc<Mutex<Instant>>,
    window_size: Arc<Mutex<WindowSize>>,
    render_revision: Arc<AtomicU64>,
    scrollback_limit: usize,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    _reader_thread: thread::JoinHandle<()>,
}

#[derive(Debug, Clone, Copy)]
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

        let title = Arc::new(Mutex::new("Terminal".to_owned()));
        let alive = Arc::new(AtomicBool::new(true));
        let bell_fired = Arc::new(AtomicBool::new(false));
        let last_input_at = Arc::new(Mutex::new(Instant::now()));
        let last_output_at = Arc::new(Mutex::new(Instant::now()));
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

        let title_for_reader = Arc::clone(&title);
        let alive_for_reader = Arc::clone(&alive);
        let bell_for_reader = Arc::clone(&bell_fired);
        let writer_for_reader = Arc::new(Mutex::new(writer));
        let writer_for_thread = Arc::clone(&writer_for_reader);
        let output_for_reader = Arc::clone(&last_output_at);
        let term_for_reader = Arc::clone(&term);
        let window_size_for_reader = Arc::clone(&window_size);
        let render_revision_for_reader = Arc::clone(&render_revision);
        let scheduler_for_reader = Arc::clone(&scheduler);
        let reader_thread = thread::spawn(move || {
            let mut buf = [0_u8; 4096];
            let mut processor = Processor::<StdSyncHandler>::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        alive_for_reader.store(false, Ordering::Relaxed);
                        if let Ok(mut scheduler) = scheduler_for_reader.lock() {
                            scheduler.record_exit(session_id);
                        }
                        break;
                    }
                    Ok(read) => {
                        if let Ok(mut term) = term_for_reader.lock() {
                            processor.advance(&mut *term, &buf[..read]);
                        }
                        render_revision_for_reader.fetch_add(1, Ordering::Relaxed);
                        *output_for_reader.lock().unwrap() = Instant::now();
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
                    Err(_) => {
                        alive_for_reader.store(false, Ordering::Relaxed);
                        if let Ok(mut scheduler) = scheduler_for_reader.lock() {
                            scheduler.record_exit(session_id);
                        }
                        break;
                    }
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
        });

        Ok(Self {
            term,
            title,
            alive,
            bell_fired,
            writer: writer_for_reader,
            last_input_at,
            last_output_at,
            window_size,
            render_revision,
            scrollback_limit,
            master: pair.master,
            killer,
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
        *self.window_size.lock().unwrap() = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: 0,
            cell_height: 0,
        };
        if let Ok(mut term) = self.term.lock() {
            term.resize(TermSize::new(cols as usize, rows as usize));
        }
        self.mark_render_dirty();
    }

    pub fn write_all(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            if writer.write_all(bytes).is_ok() {
                let _ = writer.flush();
                *self.last_input_at.lock().unwrap() = Instant::now();
            }
        }
    }

    pub fn input_mode(&self) -> InputMode {
        let mode = self.term.lock().unwrap().mode().to_owned();
        InputMode {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            mouse_mode: mode.intersects(TermMode::MOUSE_MODE),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
        }
    }

    pub fn scroll_display(&self, scroll: Scroll) {
        if let Ok(mut term) = self.term.lock() {
            term.scroll_display(scroll);
        }
        self.mark_render_dirty();
    }

    pub fn selected_text(&self) -> Option<String> {
        self.term.lock().unwrap().selection_to_string()
    }

    pub fn with_term<R>(&self, f: impl FnOnce(&mut Term<EventProxy>) -> R) -> Option<R> {
        let mut term = self.term.try_lock().ok()?;
        Some(f(&mut term))
    }

    pub fn title_snapshot(&self) -> Option<String> {
        self.title.try_lock().ok().map(|title| title.clone())
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
    title: &Arc<Mutex<String>>,
    alive: &Arc<AtomicBool>,
    bell_fired: &Arc<AtomicBool>,
    window_size: &Arc<Mutex<WindowSize>>,
    scheduler: &SharedRuntimeScheduler,
    session_id: Uuid,
) {
    while let Ok(event) = event_rx.try_recv() {
        match event {
            Event::PtyWrite(text) => {
                if let Ok(mut writer) = writer.lock() {
                    let _ = writer.write_all(text.as_bytes());
                    let _ = writer.flush();
                }
            }
            Event::Title(new_title) => {
                *title.lock().unwrap() = new_title;
                if let Ok(mut scheduler) = scheduler.lock() {
                    scheduler.record_title_changed(session_id);
                }
            }
            Event::ResetTitle => {
                *title.lock().unwrap() = "Terminal".to_owned();
                if let Ok(mut scheduler) = scheduler.lock() {
                    scheduler.record_title_changed(session_id);
                }
            }
            Event::ClipboardStore(_, text) => {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(text);
                }
            }
            Event::ClipboardLoad(_, formatter) => {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    if let Ok(text) = clipboard.get_text() {
                        if let Ok(mut writer) = writer.lock() {
                            let _ = writer.write_all(formatter(&text).as_bytes());
                            let _ = writer.flush();
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
                let size = *window_size.lock().unwrap();
                if let Ok(mut writer) = writer.lock() {
                    let _ = writer.write_all(formatter(size).as_bytes());
                    let _ = writer.flush();
                }
            }
            Event::Bell => {
                bell_fired.store(true, Ordering::Relaxed);
                if let Ok(mut scheduler) = scheduler.lock() {
                    scheduler.record_bell(session_id);
                }
            }
            Event::Exit | Event::ChildExit(_) => {
                alive.store(false, Ordering::Relaxed);
                if let Ok(mut scheduler) = scheduler.lock() {
                    scheduler.record_exit(session_id);
                }
            }
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => {
                if let Ok(mut scheduler) = scheduler.lock() {
                    scheduler.record_render(session_id);
                }
            }
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

impl Drop for PtyHandle {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        let _ = self.killer.kill();
    }
}
