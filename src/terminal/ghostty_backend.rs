#![allow(dead_code)]

use anyhow::Context as _;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RowIterator};
use libghostty_vt::terminal::ScrollViewport;
use libghostty_vt::{RenderState, Terminal, TerminalOptions};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use crate::terminal::pty::TerminalScrollState;

enum GhosttyRuntimeCommand {
    Output(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    ScrollDelta(i32),
    ScrollToDisplayOffset(usize),
}

#[derive(Clone)]
pub struct GhosttyRuntimeHandle {
    inner: Arc<GhosttyRuntimeInner>,
}

struct GhosttyRuntimeInner {
    command_tx: mpsc::Sender<GhosttyRuntimeCommand>,
    snapshot: Arc<Mutex<Option<Arc<GhosttyTextSnapshot>>>>,
}

pub struct GhosttyProbe {
    terminal: Terminal<'static, 'static>,
    render_state: RenderState<'static>,
    revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhosttyTextSnapshot {
    pub cols: u16,
    pub rows: Vec<String>,
    pub styled_rows: Vec<Vec<GhosttyCellSnapshot>>,
    pub default_colors: GhosttyDefaultColors,
    pub cursor: Option<GhosttyCursorSnapshot>,
    pub scroll_state: TerminalScrollState,
    pub dirty: GhosttyDirtyState,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhosttyCellSnapshot {
    pub text: String,
    pub foreground: Option<GhosttyRgb>,
    pub background: Option<GhosttyRgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub inverse: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GhosttyDefaultColors {
    pub foreground: GhosttyRgb,
    pub background: GhosttyRgb,
    pub cursor: Option<GhosttyRgb>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GhosttyCursorSnapshot {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub blinking: bool,
    pub style: GhosttyCursorShape,
    pub color: Option<GhosttyRgb>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhosttyCursorShape {
    Bar,
    Block,
    Underline,
    BlockHollow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GhosttyRgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhosttyDirtyState {
    Clean,
    Partial,
    Full,
}

impl GhosttyProbe {
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> anyhow::Result<Self> {
        let terminal = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback,
        })
        .context("create ghostty terminal")?;
        let render_state = RenderState::new().context("create ghostty render state")?;

        Ok(Self {
            terminal,
            render_state,
            revision: 0,
        })
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
        self.revision = self.revision.saturating_add(1);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> anyhow::Result<()> {
        self.terminal
            .resize(cols, rows, 0, 0)
            .context("resize ghostty terminal")?;
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn scroll_delta(&mut self, delta: i32) {
        self.terminal
            .scroll_viewport(ScrollViewport::Delta(-(delta as isize)));
        self.revision = self.revision.saturating_add(1);
    }

    pub fn scroll_to_display_offset(&mut self, target: usize) -> anyhow::Result<()> {
        let current = self.scroll_state()?.display_offset;
        let target = target.min(self.scroll_state()?.history_size);
        let delta = target as i32 - current as i32;
        if delta != 0 {
            self.scroll_delta(delta);
        }
        Ok(())
    }

    pub fn snapshot(&mut self) -> anyhow::Result<GhosttyTextSnapshot> {
        let snapshot = self
            .render_state
            .update(&self.terminal)
            .context("update ghostty render state")?;
        let cols = snapshot.cols().context("read ghostty cols")?;
        let colors = snapshot.colors().context("read ghostty colors")?;
        let default_colors = GhosttyDefaultColors {
            foreground: colors.foreground.into(),
            background: colors.background.into(),
            cursor: colors.cursor.map(Into::into),
        };
        let cursor = match snapshot
            .cursor_viewport()
            .context("read ghostty cursor viewport")?
        {
            Some(cursor) => Some(GhosttyCursorSnapshot {
                x: cursor.x,
                y: cursor.y,
                visible: snapshot
                    .cursor_visible()
                    .context("read ghostty cursor visibility")?,
                blinking: snapshot
                    .cursor_blinking()
                    .context("read ghostty cursor blink state")?,
                style: ghostty_cursor_shape(
                    snapshot
                        .cursor_visual_style()
                        .context("read ghostty cursor style")?,
                ),
                color: snapshot
                    .cursor_color()
                    .context("read ghostty cursor color")?
                    .map(Into::into),
            }),
            None => None,
        };
        let dirty = ghostty_dirty_state(snapshot.dirty().context("read ghostty dirty state")?);
        let mut row_iter = RowIterator::new().context("create ghostty row iterator")?;
        let mut cell_iter = CellIterator::new().context("create ghostty cell iterator")?;
        let mut rows = Vec::new();
        let mut styled_rows = Vec::new();
        let mut row_iteration = row_iter
            .update(&snapshot)
            .context("update ghostty row iterator")?;

        while let Some(row) = row_iteration.next() {
            let mut line = String::new();
            let mut styled_row = Vec::new();
            let mut cell_iteration = cell_iter
                .update(row)
                .context("update ghostty cell iterator")?;
            while let Some(cell) = cell_iteration.next() {
                let graphemes = cell.graphemes().context("read ghostty cell graphemes")?;
                let mut cell_text = String::new();
                for grapheme in graphemes {
                    if grapheme != '\0' {
                        cell_text.push(grapheme);
                        line.push(grapheme);
                    }
                }
                let style = cell.style().context("read ghostty cell style")?;
                styled_row.push(GhosttyCellSnapshot {
                    text: cell_text,
                    foreground: cell
                        .fg_color()
                        .context("read ghostty cell foreground")?
                        .map(Into::into),
                    background: cell
                        .bg_color()
                        .context("read ghostty cell background")?
                        .map(Into::into),
                    bold: style.bold,
                    italic: style.italic,
                    underline: !matches!(style.underline, libghostty_vt::style::Underline::None),
                    strikethrough: style.strikethrough,
                    inverse: style.inverse,
                });
            }
            rows.push(line);
            styled_rows.push(styled_row);
        }

        Ok(GhosttyTextSnapshot {
            cols,
            rows,
            styled_rows,
            default_colors,
            cursor,
            scroll_state: self.scroll_state().context("read ghostty scroll state")?,
            dirty,
            revision: self.revision,
        })
    }

    pub fn scroll_state(&self) -> anyhow::Result<TerminalScrollState> {
        let scrollbar = self
            .terminal
            .scrollbar()
            .context("read ghostty scrollbar")?;
        let total = scrollbar.total as usize;
        let visible_rows = scrollbar.len as usize;
        let history_size = total.saturating_sub(visible_rows);
        // libghostty-vt reports the viewport origin from the top of the full
        // scrollback. The app follows Alacritty semantics: 0 is bottom/current
        // output, and larger offsets move back into history.
        let display_offset =
            history_size.saturating_sub((scrollbar.offset as usize).min(history_size));
        Ok(TerminalScrollState {
            display_offset,
            visible_rows,
            history_size,
        })
    }
}

fn ghostty_dirty_state(dirty: Dirty) -> GhosttyDirtyState {
    match dirty {
        Dirty::Clean => GhosttyDirtyState::Clean,
        Dirty::Partial => GhosttyDirtyState::Partial,
        Dirty::Full => GhosttyDirtyState::Full,
    }
}

fn ghostty_cursor_shape(style: CursorVisualStyle) -> GhosttyCursorShape {
    match style {
        CursorVisualStyle::Bar => GhosttyCursorShape::Bar,
        CursorVisualStyle::Block => GhosttyCursorShape::Block,
        CursorVisualStyle::Underline => GhosttyCursorShape::Underline,
        CursorVisualStyle::BlockHollow => GhosttyCursorShape::BlockHollow,
        _ => GhosttyCursorShape::Block,
    }
}

impl From<libghostty_vt::style::RgbColor> for GhosttyRgb {
    fn from(value: libghostty_vt::style::RgbColor) -> Self {
        Self {
            r: value.r,
            g: value.g,
            b: value.b,
        }
    }
}

impl GhosttyRuntimeHandle {
    pub fn spawn(cols: u16, rows: u16, max_scrollback: usize) -> anyhow::Result<Self> {
        let snapshot = Arc::new(Mutex::new(None));
        let snapshot_for_thread = Arc::clone(&snapshot);
        let (command_tx, command_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::sync_channel(1);
        let _thread = thread::spawn(move || {
            let mut probe = match GhosttyProbe::new(cols, rows, max_scrollback) {
                Ok(probe) => probe,
                Err(err) => {
                    let _ = init_tx.send(Err(err.to_string()));
                    return;
                }
            };
            if let Ok(snapshot) = probe.snapshot() {
                if let Ok(mut target) = snapshot_for_thread.lock() {
                    *target = Some(Arc::new(snapshot));
                }
            }
            let _ = init_tx.send(Ok(()));

            while let Ok(command) = command_rx.recv() {
                apply_runtime_command(&mut probe, command);
                while let Ok(command) = command_rx.try_recv() {
                    apply_runtime_command(&mut probe, command);
                }

                if let Ok(snapshot) = probe.snapshot() {
                    if let Ok(mut target) = snapshot_for_thread.lock() {
                        *target = Some(Arc::new(snapshot));
                    }
                }
            }
        });
        match init_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => anyhow::bail!(err),
            Err(_) => anyhow::bail!("ghostty runtime failed to initialize"),
        }

        Ok(Self {
            inner: Arc::new(GhosttyRuntimeInner {
                command_tx,
                snapshot,
            }),
        })
    }

    pub fn feed(&self, bytes: &[u8]) {
        let _ = self
            .inner
            .command_tx
            .send(GhosttyRuntimeCommand::Output(bytes.to_vec()));
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self
            .inner
            .command_tx
            .send(GhosttyRuntimeCommand::Resize { cols, rows });
    }

    pub fn scroll_delta(&self, delta: i32) {
        let _ = self
            .inner
            .command_tx
            .send(GhosttyRuntimeCommand::ScrollDelta(delta));
    }

    pub fn scroll_to_display_offset(&self, target: usize) {
        let _ = self
            .inner
            .command_tx
            .send(GhosttyRuntimeCommand::ScrollToDisplayOffset(target));
    }

    pub fn snapshot(&self) -> Option<Arc<GhosttyTextSnapshot>> {
        self.inner.snapshot.lock().ok()?.clone()
    }
}

fn apply_runtime_command(probe: &mut GhosttyProbe, command: GhosttyRuntimeCommand) {
    match command {
        GhosttyRuntimeCommand::Output(bytes) => {
            probe.feed(&bytes);
        }
        GhosttyRuntimeCommand::Resize { cols, rows } => {
            let _ = probe.resize(cols, rows);
        }
        GhosttyRuntimeCommand::ScrollDelta(delta) => {
            probe.scroll_delta(delta);
        }
        GhosttyRuntimeCommand::ScrollToDisplayOffset(target) => {
            let _ = probe.scroll_to_display_offset(target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GhosttyProbe, GhosttyRuntimeHandle};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn ghostty_probe_renders_plain_text_snapshot() {
        let mut probe = GhosttyProbe::new(16, 4, 100).expect("ghostty probe");

        probe.feed(b"hello ghostty");
        let snapshot = probe.snapshot().expect("snapshot");

        assert_eq!(snapshot.rows[0].trim_end(), "hello ghostty");
        assert_eq!(snapshot.cols, 16);
        assert_eq!(snapshot.rows.len(), 4);
    }

    #[test]
    fn ghostty_probe_reports_scrollback_after_output_overflows_viewport() {
        let mut probe = GhosttyProbe::new(12, 3, 100).expect("ghostty probe");

        probe.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let scroll = probe.scroll_state().expect("scroll state");

        assert_eq!(scroll.visible_rows, 3);
        assert!(scroll.history_size > 0);
    }

    #[test]
    fn ghostty_probe_scrolls_viewport_through_scrollback() {
        let mut probe = GhosttyProbe::new(12, 3, 100).expect("ghostty probe");
        probe.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");

        let bottom = probe.snapshot().expect("bottom snapshot");
        assert!(bottom.rows.iter().any(|row| row.contains("five")));

        probe.scroll_delta(2);
        let scrolled = probe.snapshot().expect("scrolled snapshot");

        assert!(scrolled.scroll_state.display_offset > bottom.scroll_state.display_offset);
        assert!(scrolled.rows.iter().any(|row| row.contains("one")));
    }

    #[test]
    fn ghostty_probe_scrolls_to_app_display_offset() {
        let mut probe = GhosttyProbe::new(12, 3, 100).expect("ghostty probe");
        probe.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");

        let bottom = probe.snapshot().expect("bottom snapshot");
        assert_eq!(bottom.scroll_state.display_offset, 0);

        probe
            .scroll_to_display_offset(bottom.scroll_state.history_size)
            .expect("scroll to top");
        let top = probe.snapshot().expect("top snapshot");
        assert_eq!(
            top.scroll_state.display_offset,
            top.scroll_state.history_size
        );
        assert!(top.rows.iter().any(|row| row.contains("one")));

        probe.scroll_to_display_offset(0).expect("scroll to bottom");
        let restored = probe.snapshot().expect("restored snapshot");
        assert_eq!(restored.scroll_state.display_offset, 0);
        assert!(restored.rows.iter().any(|row| row.contains("five")));
    }

    #[test]
    fn ghostty_runtime_actor_publishes_snapshots_from_output() {
        let runtime = GhosttyRuntimeHandle::spawn(16, 4, 100).expect("ghostty runtime");

        runtime.feed(b"actor output");
        let snapshot = wait_for_snapshot(&runtime, |snapshot| {
            snapshot.rows[0].contains("actor output")
        });

        assert_eq!(snapshot.rows[0].trim_end(), "actor output");
    }

    #[test]
    fn ghostty_runtime_actor_applies_scroll_commands() {
        let runtime = GhosttyRuntimeHandle::spawn(12, 3, 100).expect("ghostty runtime");

        runtime.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let bottom = wait_for_snapshot(&runtime, |snapshot| {
            snapshot.rows.iter().any(|row| row.contains("five"))
        });
        runtime.scroll_delta(2);
        let scrolled = wait_for_snapshot(&runtime, |snapshot| {
            snapshot.scroll_state.display_offset > bottom.scroll_state.display_offset
        });

        assert!(scrolled.rows.iter().any(|row| row.contains("one")));
    }

    fn wait_for_snapshot(
        runtime: &GhosttyRuntimeHandle,
        predicate: impl Fn(&super::GhosttyTextSnapshot) -> bool,
    ) -> Arc<super::GhosttyTextSnapshot> {
        let started = Instant::now();
        loop {
            if let Some(snapshot) = runtime.snapshot() {
                if predicate(&snapshot) {
                    return snapshot;
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "timed out waiting for ghostty snapshot"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
