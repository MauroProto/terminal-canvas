use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{
    pos2, vec2, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Rounding, Sense, Stroke, Vec2,
};
use uuid::Uuid;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{point_to_viewport, viewport_to_point, Term};

use crate::canvas::config::normalize_panel_size;
use crate::canvas::config::SNAP_THRESHOLD;
use crate::canvas::snap::{snap_drag, snap_resize, SnapGuide};
use crate::canvas::viewport::Viewport;
use crate::collab::{SerializableModifiers, SharedPanelSnapshot, TerminalInputEvent};
use crate::orchestration::{PanelOverlay, PanelRuntimeObservation};
use crate::runtime::{
    PtyManager, RenderInputs, RenderQos, RenderTier, SessionSpec, SharedPtyHandle,
};
use crate::state::PanelState;
use crate::terminal::input::{is_paste_shortcut, key_to_bytes, paste_bytes, should_copy_selection};
use crate::terminal::pty::PtyHandle;
use crate::terminal::renderer::{
    compute_grid_size, render_terminal, render_terminal_preview, render_terminal_reduced,
    CELL_HEIGHT_FACTOR, CELL_WIDTH_FACTOR, FONT_SIZE, MIN_TEXT_RENDER_FONT_SIZE, PAD_X, PAD_Y,
};
use crate::utils::platform::default_shell;

pub const TITLE_BAR_HEIGHT: f32 = 42.0;
pub const BORDER_RADIUS: f32 = 16.0;
pub const MIN_WIDTH: f32 = 260.0;
pub const MIN_HEIGHT: f32 = 180.0;
pub const RESIZE_GRIP_SIZE: f32 = 28.0;
pub const RESIZE_HIT_THICKNESS: f32 = 8.0;
pub const RESIZE_CORNER_SIZE: f32 = 22.0;
pub const PANEL_BG: Color32 = Color32::from_rgb(30, 30, 30);
pub const TITLE_BG: Color32 = Color32::from_rgb(38, 38, 58);
pub const BORDER_DEFAULT: Color32 = Color32::from_rgb(72, 72, 84);
pub const BORDER_FOCUS: Color32 = Color32::from_rgb(110, 110, 124);
pub const FG: Color32 = Color32::from_rgb(232, 232, 234);
pub const DIM_FG: Color32 = Color32::from_rgb(146, 146, 152);
pub const SELECTION_BG: Color32 = Color32::from_rgba_premultiplied(80, 130, 200, 80);
pub const MAC_RED: Color32 = Color32::from_rgb(255, 95, 87);
pub const MAC_YELLOW: Color32 = Color32::from_rgb(254, 188, 46);
pub const MAC_GREEN: Color32 = Color32::from_rgb(40, 200, 64);
pub const CHROME_ZOOM_MAX: f32 = 1.0;
pub const MIN_CONTROL_STRIP_WIDTH: f32 = 72.0;
pub const MIN_TITLE_TEXT_WIDTH: f32 = 132.0;
pub const MIN_RESIZE_GRIP_WIDTH: f32 = 150.0;
pub const MIN_RESIZE_GRIP_HEIGHT: f32 = 110.0;
pub const MIN_TERMINAL_RENDER_ZOOM: f32 = MIN_TEXT_RENDER_FONT_SIZE / FONT_SIZE;
pub const MIN_TERMINAL_RENDER_WIDTH: f32 = 40.0;
pub const MIN_TERMINAL_RENDER_HEIGHT: f32 = 28.0;
const STREAMING_OUTPUT_WINDOW: Duration = Duration::from_millis(350);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelAction {
    Close,
    Rename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeHandle {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelHitArea {
    CloseButton,
    TitleBar,
    Body,
    Resize(ResizeHandle),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelLod {
    Full,
    Compact,
    Minimal,
}

#[derive(Debug, Clone, Copy)]
struct PanelRoundings {
    panel: Rounding,
    title: Rounding,
    body: Rounding,
}

#[derive(Default)]
pub struct PanelInteraction {
    pub clicked: bool,
    pub hovered_terminal: bool,
    pub guides: Vec<SnapGuide>,
    pub action: Option<PanelAction>,
}

pub struct TerminalPanel {
    pub id: Uuid,
    pub title: String,
    shell_title: String,
    custom_title: Option<String>,
    cwd_label: String,
    shell_label: String,
    pub position: Pos2,
    pub size: Vec2,
    pub color: Color32,
    pub z_index: u32,
    pub focused: bool,
    pty_manager: Option<Arc<Mutex<PtyManager>>>,
    session_id: Option<Uuid>,
    last_cols: u16,
    last_rows: u16,
    spawn_error: Option<String>,
    pub drag_virtual_pos: Option<Pos2>,
    pub resize_virtual_rect: Option<Rect>,
    bell_flash_until: f64,
    activity_label: Option<String>,
    command_buffer: String,
    last_activity_scan_at: f64,
}

impl TerminalPanel {
    pub fn new(position: Pos2, size: Vec2, color: Color32, z_index: u32) -> Self {
        Self {
            id: Uuid::new_v4(),
            title: "Terminal".to_owned(),
            shell_title: "Terminal".to_owned(),
            custom_title: None,
            cwd_label: "Terminal".to_owned(),
            shell_label: shell_label(),
            position,
            size: normalize_panel_size(size),
            color,
            z_index,
            focused: false,
            pty_manager: None,
            session_id: None,
            last_cols: 0,
            last_rows: 0,
            spawn_error: None,
            drag_virtual_pos: None,
            resize_virtual_rect: None,
            bell_flash_until: 0.0,
            activity_label: None,
            command_buffer: String::new(),
            last_activity_scan_at: 0.0,
        }
    }

    pub fn from_saved(
        saved: PanelState,
        ctx: &egui::Context,
        cwd: Option<&Path>,
        pty_manager: Arc<Mutex<PtyManager>>,
    ) -> Self {
        let mut panel = Self::new(
            pos2(saved.position[0], saved.position[1]),
            normalize_panel_size(vec2(saved.size[0], saved.size[1])),
            Color32::from_rgb(saved.color[0], saved.color[1], saved.color[2]),
            saved.z_index,
        );
        panel.id = Uuid::parse_str(&saved.id).unwrap_or_else(|_| Uuid::new_v4());
        panel.custom_title = saved.custom_title;
        panel.title = panel
            .custom_title
            .clone()
            .unwrap_or_else(|| saved.title.clone());
        panel.focused = saved.focused;
        panel.attach_session(pty_manager, ctx, cwd);
        panel
    }

    pub fn attach_session(
        &mut self,
        pty_manager: Arc<Mutex<PtyManager>>,
        ctx: &egui::Context,
        cwd: Option<&Path>,
    ) {
        let spec = SessionSpec {
            title: self.title.clone(),
            cwd: cwd.map(Path::to_path_buf),
            startup_command: None,
            startup_input: None,
        };
        self.attach_session_with_spec(pty_manager, ctx, cwd, spec);
    }

    pub fn attach_session_with_spec(
        &mut self,
        pty_manager: Arc<Mutex<PtyManager>>,
        ctx: &egui::Context,
        cwd: Option<&Path>,
        spec: SessionSpec,
    ) {
        self.close_runtime_session();
        self.cwd_label = cwd_label(cwd);
        self.shell_label = shell_label();
        let (cols, rows) = compute_grid_size(self.size.x, self.size.y - TITLE_BAR_HEIGHT);
        self.last_cols = cols;
        self.last_rows = rows;
        self.pty_manager = Some(Arc::clone(&pty_manager));
        let spawn_result = match pty_manager.lock() {
            Ok(mut manager) => manager.spawn(ctx, spec, cwd, cols, rows),
            Err(_) => Err(anyhow::anyhow!("PTY manager lock poisoned")),
        };
        match spawn_result {
            Ok(session_id) => {
                self.session_id = Some(session_id);
                self.spawn_error = None;
            }
            Err(err) => {
                self.session_id = None;
                self.spawn_error = Some(err.to_string());
            }
        }
    }

    pub fn runtime_session_id(&self) -> Option<Uuid> {
        self.session_id
    }

    fn session_handle(&self) -> Option<SharedPtyHandle> {
        let manager = self.pty_manager.as_ref()?;
        let session_id = self.session_id?;
        manager.lock().ok()?.handle(session_id)
    }

    fn close_runtime_session(&mut self) {
        let Some(session_id) = self.session_id.take() else {
            return;
        };
        if let Some(manager) = &self.pty_manager {
            if let Ok(mut manager) = manager.lock() {
                manager.close(session_id);
            }
        }
    }

    fn with_pty<R>(&self, f: impl FnOnce(&PtyHandle) -> R) -> Option<R> {
        let handle = self.session_handle()?;
        let pty = handle.lock().ok()?;
        Some(f(&pty))
    }

    pub fn apply_resize(&mut self, rect: Rect) {
        self.position = rect.min;
        self.size = rect.size();
    }

    pub fn is_alive(&self) -> bool {
        let Some(manager) = &self.pty_manager else {
            return false;
        };
        let Some(session_id) = self.session_id else {
            return false;
        };
        manager
            .lock()
            .ok()
            .map(|manager| manager.is_alive(session_id))
            .unwrap_or(false)
    }

    pub fn to_saved(&self) -> PanelState {
        PanelState {
            id: self.id.to_string(),
            title: self.title.clone(),
            custom_title: self.custom_title.clone(),
            position: [self.position.x, self.position.y],
            size: [self.size.x, self.size.y],
            color: [self.color.r(), self.color.g(), self.color.b()],
            z_index: self.z_index,
            focused: self.focused,
        }
    }

    pub fn sync_title(&mut self) {
        let shell_title =
            self.pty_manager
                .as_ref()
                .zip(self.session_id)
                .and_then(|(manager, session_id)| {
                    manager
                        .lock()
                        .ok()
                        .and_then(|manager| manager.session_title(session_id))
                });
        if let Some(shell_title) = shell_title {
            self.apply_shell_title(shell_title);
            if let Some(activity_label) = infer_activity_label(&self.title, &self.shell_title, "") {
                self.activity_label = Some(activity_label);
            }
        }
    }

    pub fn orchestration_observation(&self, workspace_id: Uuid) -> PanelRuntimeObservation {
        let mut visible_text = String::new();
        let recent_output = self
            .with_pty(|pty| {
                if let Ok(term) = pty.term.try_lock() {
                    visible_text = visible_text_snapshot(&term, 16, 180);
                }
                pty.last_output_at
                    .try_lock()
                    .ok()
                    .map(|last_output_at| last_output_at.elapsed() <= Duration::from_secs(4))
                    .unwrap_or(false)
            })
            .unwrap_or(false);

        PanelRuntimeObservation {
            panel_id: self.id,
            runtime_session_id: self.runtime_session_id(),
            workspace_id,
            title: self.title.clone(),
            visible_text,
            alive: self.is_alive(),
            recent_output,
        }
    }

    pub fn handle_input(&mut self, ctx: &egui::Context) {
        if !self.focused {
            return;
        }

        let mode = self.with_pty(PtyHandle::input_mode).unwrap_or_default();
        let has_selection = self
            .with_pty(|pty| pty.with_term(|term| term.selection.is_some()))
            .flatten()
            .unwrap_or(false);
        ctx.input(|input| {
            for event in &input.events {
                match event {
                    egui::Event::Text(text)
                        if !input.modifiers.ctrl && !input.modifiers.command =>
                    {
                        let _ = self.with_pty(|pty| pty.write_all(text.as_bytes()));
                        self.record_input_text(text);
                    }
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if should_copy_selection(modifiers, key, has_selection) {
                            if let Some(text) = self.selected_text() {
                                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                    let _ = clipboard.set_text(text);
                                }
                            }
                            continue;
                        }
                        if is_paste_shortcut(modifiers, key) {
                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                if let Ok(text) = clipboard.get_text() {
                                    let bytes = paste_bytes(&text, &mode);
                                    let _ = self.with_pty(|pty| pty.write_all(&bytes));
                                    self.record_input_text(&text);
                                }
                            }
                            continue;
                        }
                        if let Some(bytes) = key_to_bytes(key, modifiers, &mode) {
                            let _ = self.with_pty(|pty| pty.write_all(&bytes));
                        }
                        self.record_key_input(*key, modifiers.ctrl || modifiers.command);
                    }
                    egui::Event::Paste(text) => {
                        let bytes = paste_bytes(text, &mode);
                        let _ = self.with_pty(|pty| pty.write_all(&bytes));
                        self.record_input_text(text);
                    }
                    _ => {}
                }
            }
        });
    }

    pub fn apply_remote_input_events(&mut self, events: &[TerminalInputEvent]) {
        if events.is_empty() {
            return;
        }

        let mode = self.with_pty(PtyHandle::input_mode).unwrap_or_default();
        for event in events {
            match event {
                TerminalInputEvent::Text(text) => {
                    let _ = self.with_pty(|pty| pty.write_all(text.as_bytes()));
                    self.record_input_text(text);
                }
                TerminalInputEvent::Paste(text) => {
                    let bytes = paste_bytes(text, &mode);
                    let _ = self.with_pty(|pty| pty.write_all(&bytes));
                    self.record_input_text(text);
                }
                TerminalInputEvent::Key { key, modifiers } => {
                    let modifiers = egui_modifiers(*modifiers);
                    if let Some(bytes) = key_to_bytes(&key.to_egui(), &modifiers, &mode) {
                        let _ = self.with_pty(|pty| pty.write_all(&bytes));
                    }
                    self.record_key_input(key.to_egui(), modifiers.ctrl || modifiers.command);
                }
                TerminalInputEvent::Scroll { delta } => {
                    self.apply_scroll_delta(*delta);
                }
            }
        }
    }

    pub fn handle_scroll(&mut self, delta: f32, _ctx: &egui::Context) {
        self.apply_scroll_delta(delta);
    }

    fn apply_scroll_delta(&mut self, delta: f32) {
        let Some(mode) = self.with_pty(PtyHandle::input_mode) else {
            return;
        };
        let lines = (delta.abs() / 24.0).max(1.0) as i32;

        if mode.alt_screen {
            for _ in 0..lines {
                let seq = if delta > 0.0 { b"\x1b[A" } else { b"\x1b[B" };
                let _ = self.with_pty(|pty| pty.write_all(seq));
            }
        } else if mode.mouse_mode {
            for _ in 0..lines {
                let button = if delta > 0.0 { 64 } else { 65 };
                let seq = format!("\x1b[<{};1;1M", button);
                let _ = self.with_pty(|pty| pty.write_all(seq.as_bytes()));
            }
        } else {
            let scroll = if delta > 0.0 {
                Scroll::Delta(lines)
            } else {
                Scroll::Delta(-lines)
            };
            let _ = self.with_pty(|pty| pty.scroll_display(scroll));
        }
    }

    pub fn shared_snapshot(&self) -> SharedPanelSnapshot {
        let mut visible_text = String::new();
        let mut history_text = String::new();
        if let Some(handle) = self.session_handle() {
            if let Ok(pty) = handle.lock() {
                if let Ok(term) = pty.term.try_lock() {
                    visible_text = visible_text_snapshot(&term, 18, 180);
                    history_text = visible_text_snapshot(&term, 80, 220);
                }
            }
        }

        SharedPanelSnapshot {
            panel_id: self.id,
            title: self.title.clone(),
            position: [self.position.x, self.position.y],
            size: [self.size.x, self.size.y],
            color: [self.color.r(), self.color.g(), self.color.b()],
            z_index: self.z_index,
            focused: self.focused,
            alive: self.is_alive(),
            preview_label: self.preview_label(),
            visible_text,
            history_text,
            controller: None,
            controller_name: None,
            queue_len: 0,
        }
    }

    pub fn scroll_hit_test(&self, pos: Pos2, viewport: &Viewport, canvas_rect: Rect) -> bool {
        self.body_screen_rect(viewport, canvas_rect)
            .intersect(canvas_rect)
            .contains(pos)
    }

    pub fn contains_screen_pos(&self, pos: Pos2, viewport: &Viewport, canvas_rect: Rect) -> bool {
        let (screen_rect, _, _) = self.screen_geometry(viewport, canvas_rect);
        screen_rect.intersect(canvas_rect).contains(pos)
    }

    pub fn hit_test(
        &self,
        pos: Pos2,
        viewport: &Viewport,
        canvas_rect: Rect,
    ) -> Option<PanelHitArea> {
        let (screen_rect, title_rect, body_rect) = self.screen_geometry(viewport, canvas_rect);
        let lod = panel_lod(screen_rect, title_rect);
        if !screen_rect.intersect(canvas_rect).contains(pos) {
            return None;
        }

        if should_draw_window_controls(screen_rect, title_rect)
            && close_rect(title_rect).intersect(canvas_rect).contains(pos)
        {
            return Some(PanelHitArea::CloseButton);
        }

        for handle in ResizeHandle::ALL {
            if handle
                .hit_rect(screen_rect)
                .intersect(canvas_rect)
                .contains(pos)
            {
                return Some(PanelHitArea::Resize(handle));
            }
        }

        if title_drag_hit_rect(screen_rect, title_rect)
            .intersect(canvas_rect)
            .contains(pos)
        {
            return Some(PanelHitArea::TitleBar);
        }

        if body_behaves_like_title_bar(lod)
            && body_input_rect(body_rect)
                .intersect(canvas_rect)
                .contains(pos)
        {
            return Some(PanelHitArea::TitleBar);
        }

        if body_input_rect(body_rect)
            .intersect(canvas_rect)
            .contains(pos)
        {
            return Some(PanelHitArea::Body);
        }

        Some(PanelHitArea::Body)
    }

    pub fn drag_to(
        &mut self,
        origin: Pos2,
        pointer_delta: Vec2,
        zoom: f32,
        other_panels: &[Rect],
    ) -> Vec<SnapGuide> {
        let new_virtual = drag_target_from_origin(origin, pointer_delta, zoom);
        let snapped = snap_drag(
            Rect::from_min_size(new_virtual, self.size),
            other_panels,
            SNAP_THRESHOLD,
        );
        self.position = pos2(
            new_virtual.x + snapped.delta.x,
            new_virtual.y + snapped.delta.y,
        );
        snapped.guides
    }

    pub fn resize_to(
        &mut self,
        handle: ResizeHandle,
        origin: Rect,
        pointer_delta: Vec2,
        zoom: f32,
        other_panels: &[Rect],
    ) -> Vec<SnapGuide> {
        let mut new_rect = resize_target_from_origin(handle, origin, pointer_delta, zoom);
        let snapped = snap_resize(
            new_rect,
            other_panels,
            SNAP_THRESHOLD,
            handle.resizes_left(),
            handle.resizes_bottom(),
        );
        new_rect = handle.apply_snap_delta(new_rect, snapped.delta);
        self.apply_resize(new_rect);
        snapped.guides
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        viewport: &Viewport,
        canvas_rect: Rect,
        fast_path_render: bool,
        overlay: Option<&PanelOverlay>,
    ) -> PanelInteraction {
        let mut interaction = PanelInteraction::default();
        let zoom = viewport.zoom;
        let (screen_rect, title_rect, body_rect) = self.screen_geometry(viewport, canvas_rect);
        let lod = panel_lod(screen_rect, title_rect);
        let painter = ui.painter().with_clip_rect(canvas_rect);
        let body_hit_rect = body_input_rect(body_rect).intersect(canvas_rect);

        if !body_behaves_like_title_bar(lod)
            && body_hit_rect.width() > 0.0
            && body_hit_rect.height() > 0.0
        {
            let body_response = ui.interact(
                body_hit_rect,
                ui.id().with(("body", self.id)),
                Sense::click_and_drag(),
            );
            if body_response.clicked() {
                interaction.clicked = true;
                let _ = self.with_pty(|pty| pty.clear_selection());
            }
            interaction.hovered_terminal = body_response.hovered();

            if body_response.drag_started() {
                interaction.clicked = true;
                if let Some(pointer) = body_response.interact_pointer_pos() {
                    self.begin_selection(
                        pointer,
                        body_rect,
                        canvas_rect,
                        zoom,
                        SelectionType::Simple,
                    );
                }
            }
            if body_response.dragged() {
                if let Some(pointer) = body_response.interact_pointer_pos() {
                    self.update_selection(pointer, body_rect, canvas_rect, zoom);
                }
            }
        } else {
            interaction.hovered_terminal = false;
        }

        if !ui.ctx().input(|i| i.pointer.primary_down()) {
            self.drag_virtual_pos = None;
            self.resize_virtual_rect = None;
        }

        if self.with_pty(PtyHandle::take_bell).unwrap_or(false) {
            self.bell_flash_until = ui.ctx().input(|i| i.time) + 0.15;
        }

        let border_color = if ui.ctx().input(|i| i.time) < self.bell_flash_until {
            Color32::from_rgb(255, 200, 80)
        } else if self.focused {
            BORDER_FOCUS
        } else {
            BORDER_DEFAULT
        };
        let chrome_zoom = chrome_zoom(zoom);
        let roundings = panel_roundings(screen_rect, title_rect, body_rect);
        let panel_rounding = roundings.panel;
        let title_rounding = roundings.title;
        let show_controls =
            matches!(lod, PanelLod::Full) && should_draw_window_controls(screen_rect, title_rect);
        let show_title =
            !matches!(lod, PanelLod::Minimal) && should_draw_title_text(screen_rect, title_rect);
        let stroke_rect = screen_rect.shrink(0.5);
        let separator_inset =
            (max_panel_corner_radius(roundings) * 0.8).min(screen_rect.width() * 0.25);
        let separator_y = title_rect.bottom() - 0.5;
        let chrome_painter = painter.with_clip_rect(screen_rect.expand(1.0).intersect(canvas_rect));

        chrome_painter.rect_filled(screen_rect, panel_rounding, PANEL_BG);
        if !matches!(lod, PanelLod::Minimal) {
            chrome_painter.rect_filled(title_rect, title_rounding, TITLE_BG);
        }
        if show_controls {
            let controls_y = title_rect.center().y;
            let button_radius = (6.5 * chrome_zoom).clamp(2.0, 6.5);
            let button_spacing = (20.0 * chrome_zoom).clamp(7.0, 20.0);
            let button_offset = (26.0 * chrome_zoom).clamp(12.0, 26.0);
            let red_center = pos2(title_rect.left() + button_offset, controls_y);
            let yellow_center = pos2(
                title_rect.left() + button_offset + button_spacing,
                controls_y,
            );
            let green_center = pos2(
                title_rect.left() + button_offset + button_spacing * 2.0,
                controls_y,
            );
            chrome_painter.circle_filled(red_center, button_radius, MAC_RED);
            chrome_painter.circle_filled(yellow_center, button_radius, MAC_YELLOW);
            chrome_painter.circle_filled(green_center, button_radius, MAC_GREEN);
        }
        if show_title {
            let title_text = self.window_title(screen_rect.width());
            let title_offset = if show_controls {
                (96.0 * chrome_zoom).clamp(42.0, 96.0)
            } else {
                match lod {
                    PanelLod::Compact => 10.0,
                    PanelLod::Minimal => 0.0,
                    PanelLod::Full => 12.0,
                }
            };
            chrome_painter.text(
                title_rect.left_center() + vec2(title_offset, 0.0),
                Align2::LEFT_CENTER,
                title_text,
                FontId::proportional((15.5 * chrome_zoom).clamp(7.0, 15.5)),
                if self.is_alive() { FG } else { DIM_FG },
            );
        }
        let content_rect = body_rect.shrink2(vec2(0.0, 0.0));
        let content_clip_rect = content_rect.intersect(canvas_rect);
        let content_painter = painter.with_clip_rect(content_clip_rect);
        let content_rounding = roundings.body;
        let now = ui.ctx().input(|i| i.time);
        let title_snapshot = self.title.clone();
        let shell_title_snapshot = self.shell_title.clone();
        let fallback_preview_title = if let Some(overlay) = overlay {
            overlay.preview_label.clone()
        } else if is_generic_terminal_name(&self.title) {
            self.cwd_label.clone()
        } else {
            self.title.clone()
        };
        let mut activity_label = self.activity_label.clone();
        let mut activity_label_scan_at = None;
        if let Some(handle) = self.session_handle() {
            if let Ok(mut pty) = handle.lock() {
                let (cols, rows) = compute_grid_size(self.size.x, self.size.y - TITLE_BAR_HEIGHT);
                let defer_resize =
                    should_defer_terminal_resize(fast_path_render, self.resize_virtual_rect);
                if !defer_resize && (cols != self.last_cols || rows != self.last_rows) {
                    self.last_cols = cols;
                    self.last_rows = rows;
                    pty.resize(cols, rows);
                }
                let render_tier = render_tier_for_panel(
                    content_rect,
                    zoom,
                    lod,
                    fast_path_render,
                    self.focused,
                    is_streaming_output(&pty),
                );
                let scan_activity = should_refresh_activity_label(self.last_activity_scan_at, now);
                let mut scanned_activity_label = None;
                if matches!(render_tier, RenderTier::Full | RenderTier::ReducedLive) {
                    if let Ok(mut term) = pty.term.try_lock() {
                        term.is_focused = self.focused;
                        if scan_activity {
                            scanned_activity_label = Some(infer_activity_label_from_term(
                                &title_snapshot,
                                &shell_title_snapshot,
                                &term,
                            ));
                        }
                        match render_tier {
                            RenderTier::Full => render_terminal(
                                &content_painter,
                                content_rect,
                                &term,
                                self.focused,
                                now,
                                zoom,
                                content_rounding,
                            ),
                            RenderTier::ReducedLive => render_terminal_reduced(
                                &content_painter,
                                content_rect,
                                &term,
                                self.focused,
                                now,
                                zoom,
                                content_rounding,
                            ),
                            RenderTier::Preview | RenderTier::Hidden => {}
                        }
                    } else {
                        let preview_label = overlay
                            .map(|overlay| overlay.preview_label.clone())
                            .unwrap_or_else(|| {
                                preview_label_text(
                                    activity_label.as_deref(),
                                    &fallback_preview_title,
                                )
                            });
                        render_terminal_preview(
                            &content_painter,
                            content_rect,
                            self.focused,
                            zoom,
                            Some(preview_label.as_str()),
                        );
                    }
                } else {
                    if let Ok(mut term) = pty.term.try_lock() {
                        term.is_focused = self.focused;
                        if scan_activity {
                            scanned_activity_label = Some(infer_activity_label_from_term(
                                &title_snapshot,
                                &shell_title_snapshot,
                                &term,
                            ));
                        }
                    }
                    if let Some(detected_label) = scanned_activity_label.take() {
                        activity_label = detected_label.or_else(|| {
                            infer_activity_label(&title_snapshot, &shell_title_snapshot, "")
                        });
                        activity_label_scan_at = Some(now);
                    }
                    if !matches!(render_tier, RenderTier::Hidden) {
                        let preview_label = overlay
                            .map(|overlay| overlay.preview_label.clone())
                            .unwrap_or_else(|| {
                                preview_label_text(
                                    activity_label.as_deref(),
                                    &fallback_preview_title,
                                )
                            });
                        render_terminal_preview(
                            &content_painter,
                            content_rect,
                            self.focused,
                            zoom,
                            Some(preview_label.as_str()),
                        );
                    }
                }
                if let Some(detected_label) = scanned_activity_label.take() {
                    activity_label = detected_label.or_else(|| {
                        infer_activity_label(&title_snapshot, &shell_title_snapshot, "")
                    });
                    activity_label_scan_at = Some(now);
                }
            } else {
                let preview_label = overlay
                    .map(|overlay| overlay.preview_label.clone())
                    .unwrap_or_else(|| {
                        preview_label_text(activity_label.as_deref(), &fallback_preview_title)
                    });
                render_terminal_preview(
                    &content_painter,
                    content_rect,
                    self.focused,
                    zoom,
                    Some(preview_label.as_str()),
                );
            }
        } else if let Some(error) = &self.spawn_error {
            painter.text(
                content_rect.left_top() + vec2(12.0, 12.0),
                Align2::LEFT_TOP,
                error,
                FontId::monospace(FONT_SIZE),
                Color32::from_rgb(239, 68, 68),
            );
        }
        self.activity_label = activity_label;
        if let Some(scanned_at) = activity_label_scan_at {
            self.last_activity_scan_at = scanned_at;
        }

        chrome_painter.rect_stroke(stroke_rect, panel_rounding, Stroke::new(1.0, border_color));
        if matches!(lod, PanelLod::Full) {
            chrome_painter.line_segment(
                [
                    pos2(screen_rect.left() + separator_inset, separator_y),
                    pos2(screen_rect.right() - separator_inset, separator_y),
                ],
                Stroke::new(1.0, border_color),
            );
        }

        let grip_color = Color32::from_rgba_premultiplied(180, 180, 188, 110);
        let resize_rect = resize_handle_rect(screen_rect);
        if matches!(lod, PanelLod::Full) && should_draw_resize_grip(screen_rect) {
            let grip_painter =
                painter.with_clip_rect(screen_rect.shrink(1.0).intersect(canvas_rect));
            for offset in [0.0, 5.0, 10.0] {
                grip_painter.line_segment(
                    [
                        resize_rect.right_bottom() - vec2(18.0 - offset, 6.0),
                        resize_rect.right_bottom() - vec2(6.0, 18.0 - offset),
                    ],
                    Stroke::new(1.0, grip_color),
                );
            }
        }

        interaction
    }

    pub fn rename_title(&mut self, title: String) {
        let title = title.trim().to_owned();
        self.custom_title = if title.is_empty() { None } else { Some(title) };
        self.refresh_display_title();
    }

    fn selected_text(&self) -> Option<String> {
        self.with_pty(PtyHandle::selected_text).flatten()
    }

    fn record_input_text(&mut self, text: &str) {
        for ch in text.chars() {
            match ch {
                '\r' | '\n' => self.commit_command_buffer(),
                ch if !ch.is_control() => self.command_buffer.push(ch),
                _ => {}
            }
        }
    }

    fn record_key_input(&mut self, key: egui::Key, command_modified: bool) {
        if command_modified {
            return;
        }
        match key {
            egui::Key::Backspace => {
                self.command_buffer.pop();
            }
            egui::Key::Enter => self.commit_command_buffer(),
            _ => {}
        }
    }

    fn commit_command_buffer(&mut self) {
        let command = self.command_buffer.trim();
        if let Some(activity_label) = infer_activity_label("", "", command) {
            self.activity_label = Some(activity_label);
        }
        self.command_buffer.clear();
    }

    fn apply_activity_label_update(&mut self, activity_label: Option<String>, time: f64) {
        self.activity_label =
            activity_label.or_else(|| infer_activity_label(&self.title, &self.shell_title, ""));
        self.last_activity_scan_at = time;
    }

    fn preview_label(&self) -> String {
        let fallback = if is_generic_terminal_name(&self.title) {
            &self.cwd_label
        } else {
            &self.title
        };
        preview_label_text(self.activity_label.as_deref(), fallback)
    }

    fn body_screen_rect(&self, viewport: &Viewport, canvas_rect: Rect) -> Rect {
        let screen_pos = viewport.canvas_to_screen(self.position, canvas_rect);
        let screen_rect = Rect::from_min_size(screen_pos, self.size * viewport.zoom);
        Rect::from_min_max(
            pos2(
                screen_rect.left(),
                screen_rect.top() + title_bar_height(viewport.zoom),
            ),
            screen_rect.right_bottom(),
        )
    }

    fn apply_shell_title(&mut self, title: String) {
        self.shell_title = if title.trim().is_empty() {
            "Terminal".to_owned()
        } else {
            title
        };
        self.refresh_display_title();
    }

    fn refresh_display_title(&mut self) {
        self.title = self
            .custom_title
            .clone()
            .unwrap_or_else(|| self.shell_title.clone());
    }

    fn window_title(&self, screen_width: f32) -> String {
        let cols = self.last_cols.max(1);
        let rows = self.last_rows.max(1);
        if screen_width < 340.0 {
            return format!("{}×{}", cols, rows);
        }
        if screen_width < 520.0 {
            return format!("{} — {}×{}", self.title, cols, rows);
        }
        if let Some(custom_title) = &self.custom_title {
            format!("{} — {}×{}", custom_title, cols, rows)
        } else {
            format!(
                "{} — {} — {}×{}",
                self.cwd_label, self.shell_label, cols, rows
            )
        }
    }

    fn begin_selection(
        &mut self,
        pointer: Pos2,
        content_rect: Rect,
        canvas_rect: Rect,
        zoom: f32,
        selection_type: SelectionType,
    ) {
        let Some(handle) = self.session_handle() else {
            return;
        };
        let Ok(pty) = handle.lock() else {
            return;
        };
        let Some((point, side)) =
            self.point_from_pointer(&pty, content_rect, canvas_rect, pointer, zoom)
        else {
            return;
        };
        let Ok(mut term) = pty.term.try_lock() else {
            return;
        };
        term.selection = Some(Selection::new(selection_type, point, side));
    }

    fn update_selection(
        &mut self,
        pointer: Pos2,
        content_rect: Rect,
        canvas_rect: Rect,
        zoom: f32,
    ) {
        let Some(handle) = self.session_handle() else {
            return;
        };
        let Ok(pty) = handle.lock() else {
            return;
        };
        let Some((point, side)) =
            self.point_from_pointer(&pty, content_rect, canvas_rect, pointer, zoom)
        else {
            return;
        };
        let Ok(mut term) = pty.term.try_lock() else {
            return;
        };
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        } else {
            term.selection = Some(Selection::new(SelectionType::Simple, point, side));
        }
    }

    fn point_from_pointer(
        &self,
        pty: &PtyHandle,
        content_rect: Rect,
        canvas_rect: Rect,
        pointer: Pos2,
        zoom: f32,
    ) -> Option<(Point, Side)> {
        if !content_rect.intersect(canvas_rect).contains(pointer) {
            return None;
        }

        let zoom = zoom.max(0.01);
        let cell_width = FONT_SIZE * CELL_WIDTH_FACTOR * zoom;
        let cell_height = FONT_SIZE * CELL_HEIGHT_FACTOR * zoom;
        let pad_x = PAD_X * zoom;
        let pad_y = PAD_Y * zoom;
        let local_x = (pointer.x - content_rect.left() - pad_x).max(0.0);
        let local_y = (pointer.y - content_rect.top() - pad_y).max(0.0);
        let term = pty.term.try_lock().ok()?;

        let row =
            ((local_y / cell_height).floor() as usize).min(term.screen_lines().saturating_sub(1));
        let column =
            ((local_x / cell_width).floor() as usize).min(term.columns().saturating_sub(1));
        let cell_left = column as f32 * cell_width;
        let side = if local_x - cell_left >= cell_width * 0.5 {
            Side::Right
        } else {
            Side::Left
        };

        Some((
            viewport_to_point(
                term.grid().display_offset(),
                Point::new(row, Column(column)),
            ),
            side,
        ))
    }

    fn screen_geometry(&self, viewport: &Viewport, canvas_rect: Rect) -> (Rect, Rect, Rect) {
        let screen_pos = viewport.canvas_to_screen(self.position, canvas_rect);
        let screen_rect = Rect::from_min_size(screen_pos, self.size * viewport.zoom);
        let title_rect = Rect::from_min_size(
            screen_rect.min,
            vec2(screen_rect.width(), title_bar_height(viewport.zoom)),
        );
        let body_rect = Rect::from_min_max(
            pos2(screen_rect.left(), title_rect.bottom()),
            screen_rect.right_bottom(),
        );
        (screen_rect, title_rect, body_rect)
    }
}

impl Drop for TerminalPanel {
    fn drop(&mut self) {
        self.close_runtime_session();
    }
}

impl ResizeHandle {
    const ALL: [Self; 8] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
        Self::Left,
        Self::Right,
        Self::Top,
        Self::Bottom,
    ];

    fn cursor_icon(self) -> CursorIcon {
        match self {
            Self::Left | Self::Right => CursorIcon::ResizeHorizontal,
            Self::Top | Self::Bottom => CursorIcon::ResizeVertical,
            Self::TopLeft | Self::BottomRight => CursorIcon::ResizeNwSe,
            Self::TopRight | Self::BottomLeft => CursorIcon::ResizeNeSw,
        }
    }

    fn resizes_left(self) -> bool {
        matches!(self, Self::Left | Self::TopLeft | Self::BottomLeft)
    }

    fn resizes_right(self) -> bool {
        matches!(self, Self::Right | Self::TopRight | Self::BottomRight)
    }

    fn resizes_top(self) -> bool {
        matches!(self, Self::Top | Self::TopLeft | Self::TopRight)
    }

    fn resizes_bottom(self) -> bool {
        matches!(self, Self::Bottom | Self::BottomLeft | Self::BottomRight)
    }

    fn hit_rect(self, screen_rect: Rect) -> Rect {
        match self {
            Self::TopLeft => Rect::from_min_max(
                screen_rect.min,
                screen_rect.min + vec2(RESIZE_CORNER_SIZE, RESIZE_CORNER_SIZE),
            ),
            Self::TopRight => Rect::from_min_max(
                pos2(screen_rect.right() - RESIZE_CORNER_SIZE, screen_rect.top()),
                pos2(screen_rect.right(), screen_rect.top() + RESIZE_CORNER_SIZE),
            ),
            Self::BottomLeft => Rect::from_min_max(
                pos2(
                    screen_rect.left(),
                    screen_rect.bottom() - RESIZE_CORNER_SIZE,
                ),
                pos2(
                    screen_rect.left() + RESIZE_CORNER_SIZE,
                    screen_rect.bottom(),
                ),
            ),
            Self::BottomRight => Rect::from_min_max(
                screen_rect.right_bottom() - vec2(RESIZE_CORNER_SIZE, RESIZE_CORNER_SIZE),
                screen_rect.right_bottom(),
            ),
            Self::Left => Rect::from_min_max(
                pos2(screen_rect.left(), screen_rect.top() + RESIZE_CORNER_SIZE),
                pos2(
                    screen_rect.left() + RESIZE_HIT_THICKNESS,
                    screen_rect.bottom() - RESIZE_CORNER_SIZE,
                ),
            ),
            Self::Right => Rect::from_min_max(
                pos2(
                    screen_rect.right() - RESIZE_HIT_THICKNESS,
                    screen_rect.top() + RESIZE_CORNER_SIZE,
                ),
                pos2(
                    screen_rect.right(),
                    screen_rect.bottom() - RESIZE_CORNER_SIZE,
                ),
            ),
            Self::Top => Rect::from_min_max(
                pos2(screen_rect.left() + RESIZE_CORNER_SIZE, screen_rect.top()),
                pos2(
                    screen_rect.right() - RESIZE_CORNER_SIZE,
                    screen_rect.top() + RESIZE_HIT_THICKNESS,
                ),
            ),
            Self::Bottom => Rect::from_min_max(
                pos2(
                    screen_rect.left() + RESIZE_CORNER_SIZE,
                    screen_rect.bottom() - RESIZE_HIT_THICKNESS,
                ),
                pos2(
                    screen_rect.right() - RESIZE_CORNER_SIZE,
                    screen_rect.bottom(),
                ),
            ),
        }
    }

    fn apply_delta(self, rect: Rect, delta: Vec2) -> Rect {
        let mut min = rect.min;
        let mut max = rect.max;

        if self.resizes_left() {
            min.x = (min.x + delta.x).min(max.x - MIN_WIDTH);
        }
        if self.resizes_right() {
            max.x = (max.x + delta.x).max(min.x + MIN_WIDTH);
        }
        if self.resizes_top() {
            min.y = (min.y + delta.y).min(max.y - MIN_HEIGHT);
        }
        if self.resizes_bottom() {
            max.y = (max.y + delta.y).max(min.y + MIN_HEIGHT);
        }

        Rect::from_min_max(min, max)
    }

    fn apply_snap_delta(self, rect: Rect, delta: Vec2) -> Rect {
        let mut min = rect.min;
        let mut max = rect.max;

        if self.resizes_left() {
            min.x += delta.x;
        } else if self.resizes_right() {
            max.x += delta.x;
        }

        if self.resizes_top() {
            min.y += delta.y;
        } else if self.resizes_bottom() {
            max.y += delta.y;
        }

        Rect::from_min_max(min, max)
    }
}

fn close_rect(title_rect: Rect) -> Rect {
    let chrome_zoom = chrome_zoom_from_title_rect(title_rect);
    Rect::from_center_size(
        pos2(
            title_rect.left() + 26.0 * chrome_zoom,
            title_rect.center().y,
        ),
        vec2(18.0, 18.0) * chrome_zoom,
    )
}

fn resize_handle_rect(screen_rect: Rect) -> Rect {
    Rect::from_min_size(
        screen_rect.right_bottom() - vec2(RESIZE_GRIP_SIZE, RESIZE_GRIP_SIZE),
        vec2(RESIZE_GRIP_SIZE, RESIZE_GRIP_SIZE),
    )
}

fn title_drag_hit_rect(screen_rect: Rect, title_rect: Rect) -> Rect {
    const MIN_TITLE_DRAG_HIT_HEIGHT: f32 = 18.0;
    const MIN_TITLE_DRAG_HIT_WIDTH: f32 = 28.0;

    let controls_inset = if should_draw_window_controls(screen_rect, title_rect) {
        (90.0 * chrome_zoom_from_title_rect(title_rect)).clamp(42.0, 90.0)
    } else {
        10.0
    };
    let right = screen_rect.right() - RESIZE_HIT_THICKNESS;
    let left = (screen_rect.left() + controls_inset)
        .min(right - MIN_TITLE_DRAG_HIT_WIDTH)
        .max(screen_rect.left() + RESIZE_HIT_THICKNESS);
    let bottom = (title_rect.top() + title_rect.height().max(MIN_TITLE_DRAG_HIT_HEIGHT))
        .min(screen_rect.bottom() - RESIZE_HIT_THICKNESS)
        .max(title_rect.top() + 1.0);

    Rect::from_min_max(pos2(left, title_rect.top()), pos2(right, bottom))
}

fn drag_target_from_origin(origin: Pos2, drag_delta: Vec2, zoom: f32) -> Pos2 {
    origin + drag_delta / zoom.max(0.01)
}

fn resize_target_from_origin(
    handle: ResizeHandle,
    origin: Rect,
    drag_delta: Vec2,
    zoom: f32,
) -> Rect {
    handle.apply_delta(origin, drag_delta / zoom.max(0.01))
}

fn chrome_zoom(zoom: f32) -> f32 {
    zoom.clamp(0.0, CHROME_ZOOM_MAX)
}

fn title_bar_height(zoom: f32) -> f32 {
    TITLE_BAR_HEIGHT * chrome_zoom(zoom)
}

fn chrome_zoom_from_title_rect(title_rect: Rect) -> f32 {
    (title_rect.height() / TITLE_BAR_HEIGHT).clamp(0.0, CHROME_ZOOM_MAX)
}

fn panel_corner_radius(screen_rect: Rect) -> f32 {
    BORDER_RADIUS
        .min(screen_rect.width() * 0.18)
        .min(screen_rect.height() * 0.18)
        .max(2.0)
}

fn panel_roundings(screen_rect: Rect, title_rect: Rect, body_rect: Rect) -> PanelRoundings {
    let base_radius = panel_corner_radius(screen_rect);
    let top_radius = base_radius
        .min(title_rect.width() * 0.5)
        .min(title_rect.height() * 0.5)
        .max(0.0);
    let bottom_radius = base_radius
        .min(body_rect.width() * 0.5)
        .min(body_rect.height() * 0.5)
        .max(0.0);

    PanelRoundings {
        panel: Rounding {
            nw: top_radius,
            ne: top_radius,
            sw: bottom_radius,
            se: bottom_radius,
        },
        title: Rounding {
            nw: top_radius,
            ne: top_radius,
            sw: 0.0,
            se: 0.0,
        },
        body: Rounding {
            nw: 0.0,
            ne: 0.0,
            sw: bottom_radius,
            se: bottom_radius,
        },
    }
}

fn max_panel_corner_radius(roundings: PanelRoundings) -> f32 {
    roundings
        .panel
        .nw
        .max(roundings.panel.ne)
        .max(roundings.panel.sw)
        .max(roundings.panel.se)
}

fn panel_lod(screen_rect: Rect, title_rect: Rect) -> PanelLod {
    if screen_rect.width() < 96.0 || screen_rect.height() < 64.0 || title_rect.height() < 8.0 {
        PanelLod::Minimal
    } else if screen_rect.width() < 220.0
        || screen_rect.height() < 120.0
        || title_rect.height() < 14.0
    {
        PanelLod::Compact
    } else {
        PanelLod::Full
    }
}

fn should_draw_window_controls(screen_rect: Rect, title_rect: Rect) -> bool {
    screen_rect.width() >= MIN_CONTROL_STRIP_WIDTH && title_rect.height() >= 8.0
}

fn should_draw_title_text(screen_rect: Rect, title_rect: Rect) -> bool {
    screen_rect.width() >= MIN_TITLE_TEXT_WIDTH && title_rect.height() >= 10.0
}

fn should_draw_resize_grip(screen_rect: Rect) -> bool {
    screen_rect.width() >= MIN_RESIZE_GRIP_WIDTH && screen_rect.height() >= MIN_RESIZE_GRIP_HEIGHT
}

fn should_render_terminal_contents(content_rect: Rect, zoom: f32) -> bool {
    zoom >= MIN_TERMINAL_RENDER_ZOOM
        && content_rect.width() >= MIN_TERMINAL_RENDER_WIDTH
        && content_rect.height() >= MIN_TERMINAL_RENDER_HEIGHT
}

fn should_render_live_terminal(
    content_rect: Rect,
    zoom: f32,
    lod: PanelLod,
    fast_path_render: bool,
) -> bool {
    matches!(
        render_tier_for_panel(content_rect, zoom, lod, fast_path_render, false, false),
        RenderTier::Full | RenderTier::ReducedLive
    )
}

fn should_defer_terminal_resize(fast_path_render: bool, resize_virtual_rect: Option<Rect>) -> bool {
    fast_path_render && resize_virtual_rect.is_some()
}

fn body_behaves_like_title_bar(lod: PanelLod) -> bool {
    !matches!(lod, PanelLod::Full)
}

fn should_refresh_activity_label(last_scan_at: f64, time: f64) -> bool {
    (time - last_scan_at) >= 0.45
}

fn egui_modifiers(modifiers: SerializableModifiers) -> egui::Modifiers {
    egui::Modifiers {
        alt: modifiers.alt,
        ctrl: modifiers.ctrl,
        shift: modifiers.shift,
        mac_cmd: modifiers.command,
        command: modifiers.command,
    }
}

fn render_tier_for_panel(
    content_rect: Rect,
    zoom: f32,
    lod: PanelLod,
    _fast_path_render: bool,
    focused: bool,
    streaming: bool,
) -> RenderTier {
    let previewable = content_rect.width() >= 24.0 && content_rect.height() >= 18.0;
    if !previewable {
        return RenderTier::Hidden;
    }
    if matches!(lod, PanelLod::Minimal) || !should_render_terminal_contents(content_rect, zoom) {
        return RenderTier::Preview;
    }

    let screen_area = content_rect.width().max(0.0) * content_rect.height().max(0.0);
    RenderQos::decide(RenderInputs {
        visible: true,
        focused,
        screen_area,
        streaming,
        fast_path: false,
        renderable: true,
    })
}

fn is_streaming_output(pty: &PtyHandle) -> bool {
    pty.last_output_at
        .try_lock()
        .ok()
        .map(|last_output_at| last_output_at.elapsed() <= STREAMING_OUTPUT_WINDOW)
        .unwrap_or(false)
}

fn infer_activity_label_from_term(
    display_title: &str,
    shell_title: &str,
    term: &Term<crate::terminal::pty::EventProxy>,
) -> Option<String> {
    let visible_text = visible_text_snapshot(term, 10, 120);
    infer_activity_label(display_title, shell_title, &visible_text)
}

fn visible_text_snapshot(
    term: &Term<crate::terminal::pty::EventProxy>,
    max_lines: usize,
    max_cols: usize,
) -> String {
    let content = term.renderable_content();
    let display_offset = content.display_offset;
    let mut last_row = None;
    let mut current_line = String::new();
    let mut lines = Vec::new();

    for indexed in content.display_iter {
        let Some(point) = point_to_viewport(display_offset, indexed.point) else {
            continue;
        };
        if last_row != Some(point.line) {
            if !current_line.trim().is_empty() {
                lines.push(current_line.trim_end().to_owned());
            }
            current_line.clear();
            last_row = Some(point.line);
        }

        if current_line.chars().count() >= max_cols {
            continue;
        }

        let ch = indexed.cell.c;
        if ch == '\0' || indexed.cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }

        current_line.push(if ch.is_control() { ' ' } else { ch });
    }

    if !current_line.trim().is_empty() {
        lines.push(current_line.trim_end().to_owned());
    }

    let mut tail = lines
        .into_iter()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(max_lines)
        .collect::<Vec<_>>();
    tail.reverse();
    tail.join("\n")
}

fn infer_activity_label(
    display_title: &str,
    shell_title: &str,
    visible_text: &str,
) -> Option<String> {
    if let Some(command) = extract_prompt_command(visible_text) {
        if let Some(label) = map_command_to_activity(&command) {
            return Some(label.to_owned());
        }
    }

    for source in [visible_text, shell_title, display_title] {
        if let Some(label) = detect_activity_keyword(source) {
            return Some(label.to_owned());
        }
    }

    None
}

fn preview_label_text(activity_label: Option<&str>, fallback_title: &str) -> String {
    if let Some(activity_label) = activity_label
        .map(str::trim)
        .filter(|label| !label.is_empty())
    {
        activity_label.to_owned()
    } else {
        sanitize_preview_title(fallback_title).unwrap_or_else(|| "Terminal".to_owned())
    }
}

fn sanitize_preview_title(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn detect_activity_keyword(source: &str) -> Option<&'static str> {
    let source = source.to_ascii_lowercase();
    let source = source.trim();

    [
        ("openclaude", "OpenClaude"),
        ("claude code", "Claude Code"),
        ("claude-code", "Claude Code"),
        (" codex", "Codex"),
        ("codex ", "Codex"),
        ("aider", "Aider"),
        ("cursor", "Cursor"),
        ("gemini", "Gemini"),
        ("chatgpt", "ChatGPT"),
        ("claude", "Claude Code"),
    ]
    .into_iter()
    .find_map(|(needle, label)| source.contains(needle).then_some(label))
}

fn extract_prompt_command(visible_text: &str) -> Option<String> {
    for line in visible_text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        for marker in [" % ", " $ ", "> "] {
            if let Some(index) = line.rfind(marker) {
                let tail = line[index + marker.len()..].trim();
                if tail.is_empty() {
                    continue;
                }
                let command = tail
                    .split_whitespace()
                    .next()
                    .map(|part| part.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`')))
                    .filter(|part| !part.is_empty())?;
                return Some(command.to_owned());
            }
        }
    }

    None
}

fn map_command_to_activity(command: &str) -> Option<&'static str> {
    let command = command.to_ascii_lowercase();
    let command = command.trim();

    match command {
        "openclaude" => Some("OpenClaude"),
        "claude" | "claude-code" => Some("Claude Code"),
        "codex" => Some("Codex"),
        "aider" => Some("Aider"),
        "cursor" | "cursor-agent" => Some("Cursor"),
        "gemini" => Some("Gemini"),
        "chatgpt" => Some("ChatGPT"),
        _ => None,
    }
}

fn is_generic_terminal_name(title: &str) -> bool {
    matches!(
        title.trim().to_ascii_lowercase().as_str(),
        "" | "terminal" | "shell"
    )
}

fn body_input_rect(body_rect: Rect) -> Rect {
    body_rect.shrink2(vec2(RESIZE_HIT_THICKNESS, RESIZE_HIT_THICKNESS))
}

fn shell_label() -> String {
    let shell = default_shell();
    let shell_name = Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("shell");
    format!("-{}", shell_name)
}

fn cwd_label(cwd: Option<&Path>) -> String {
    cwd.and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Terminal")
        .to_owned()
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        text.to_owned()
    } else {
        format!(
            "{}…",
            text.chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::app::gesture_pointer_pos;
    use crate::canvas::viewport::Viewport;
    use crate::runtime::RenderTier;
    use egui::{pos2, vec2, Color32};

    use super::{
        chrome_zoom, close_rect, drag_target_from_origin, infer_activity_label,
        panel_corner_radius, panel_lod, panel_roundings, preview_label_text, render_tier_for_panel,
        resize_target_from_origin, shell_label, should_defer_terminal_resize,
        should_draw_resize_grip, should_draw_title_text, should_draw_window_controls,
        should_render_live_terminal, should_render_terminal_contents, title_bar_height,
        title_drag_hit_rect, PanelHitArea, PanelLod, ResizeHandle, TerminalPanel, BORDER_RADIUS,
        MIN_HEIGHT, MIN_WIDTH, TITLE_BAR_HEIGHT,
    };

    #[test]
    fn custom_title_survives_shell_title_updates() {
        let mut panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
        panel.rename_title("Deploy".to_owned());
        panel.apply_shell_title("bash".to_owned());

        assert_eq!(panel.title, "Deploy");
    }

    #[test]
    fn empty_custom_title_falls_back_to_shell_title() {
        let mut panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
        panel.apply_shell_title("zsh".to_owned());
        panel.rename_title("   ".to_owned());

        assert_eq!(panel.title, "zsh");
    }

    #[test]
    fn default_window_title_uses_shell_and_grid_size() {
        let mut panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
        panel.cwd_label = "mauro".to_owned();
        panel.shell_label = shell_label();
        panel.last_cols = 80;
        panel.last_rows = 24;

        assert_eq!(panel.window_title(720.0), "mauro — -zsh — 80×24");
    }

    #[test]
    fn close_button_is_on_left_like_macos() {
        let title_rect = egui::Rect::from_min_size(pos2(100.0, 50.0), vec2(500.0, 42.0));
        let close = close_rect(title_rect);

        assert!(close.center().x < title_rect.center().x);
        assert!((close.center().x - 126.0).abs() < 0.001);
    }

    #[test]
    fn drag_target_uses_original_position_instead_of_accumulating() {
        let origin = pos2(50.0, 60.0);
        let after_small_drag = drag_target_from_origin(origin, vec2(10.0, 0.0), 1.0);
        let after_larger_drag = drag_target_from_origin(origin, vec2(15.0, 0.0), 1.0);

        assert_eq!(after_small_drag, pos2(60.0, 60.0));
        assert_eq!(after_larger_drag, pos2(65.0, 60.0));
    }

    #[test]
    fn resize_target_uses_original_rect_instead_of_accumulating() {
        let origin = egui::Rect::from_min_size(pos2(50.0, 60.0), vec2(400.0, 300.0));
        let after_small_drag =
            resize_target_from_origin(ResizeHandle::BottomRight, origin, vec2(10.0, 0.0), 1.0);
        let after_larger_drag =
            resize_target_from_origin(ResizeHandle::BottomRight, origin, vec2(15.0, 0.0), 1.0);

        assert_eq!(after_small_drag.size(), vec2(410.0, 300.0));
        assert_eq!(after_larger_drag.size(), vec2(415.0, 300.0));
    }

    #[test]
    fn resize_respects_new_smaller_minimum_size() {
        let origin = egui::Rect::from_min_size(pos2(50.0, 60.0), vec2(320.0, 220.0));
        let resized =
            resize_target_from_origin(ResizeHandle::BottomRight, origin, vec2(-500.0, -500.0), 1.0);

        assert_eq!(resized.size(), vec2(MIN_WIDTH, MIN_HEIGHT));
    }

    #[test]
    fn narrow_windows_use_compact_title() {
        let mut panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
        panel.last_cols = 42;
        panel.last_rows = 25;

        assert_eq!(panel.window_title(420.0), "Terminal — 42×25");
    }

    #[test]
    fn chrome_zoom_is_capped_at_normal_size() {
        assert_eq!(chrome_zoom(0.5), 0.5);
        assert_eq!(chrome_zoom(1.0), 1.0);
        assert_eq!(chrome_zoom(2.5), 1.0);
    }

    #[test]
    fn title_bar_height_shrinks_when_zooming_out_but_not_when_zooming_in() {
        assert!((title_bar_height(0.5) - TITLE_BAR_HEIGHT * 0.5).abs() < 0.001);
        assert!((title_bar_height(1.0) - TITLE_BAR_HEIGHT).abs() < 0.001);
        assert!((title_bar_height(3.0) - TITLE_BAR_HEIGHT).abs() < 0.001);
    }

    #[test]
    fn tiny_panels_hide_header_details_and_terminal_text() {
        let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(120.0, 70.0));
        let title_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(120.0, 12.0));
        let content_rect = egui::Rect::from_min_size(pos2(0.0, 12.0), vec2(120.0, 58.0));

        assert!(matches!(
            panel_lod(screen_rect, title_rect),
            PanelLod::Compact
        ));
        assert!(should_draw_window_controls(screen_rect, title_rect));
        assert!(!should_draw_title_text(screen_rect, title_rect));
        assert!(!should_draw_resize_grip(screen_rect));
        assert!(should_render_terminal_contents(content_rect, 0.24));
        assert!(!should_render_terminal_contents(content_rect, 0.22));
        assert!(should_render_terminal_contents(content_rect, 0.3));
        assert!(!should_render_terminal_contents(content_rect, 0.05));
    }

    #[test]
    fn large_panels_keep_full_ui_details() {
        let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 260.0));
        let title_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 42.0));
        let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(420.0, 218.0));

        assert!(should_draw_window_controls(screen_rect, title_rect));
        assert!(should_draw_title_text(screen_rect, title_rect));
        assert!(should_draw_resize_grip(screen_rect));
        assert!(should_render_terminal_contents(content_rect, 1.0));
    }

    #[test]
    fn microscopic_panels_switch_to_minimal_lod_and_reduce_corner_radius() {
        let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(80.0, 52.0));
        let title_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(80.0, 7.0));

        assert!(matches!(
            panel_lod(screen_rect, title_rect),
            PanelLod::Minimal
        ));
        assert!(panel_corner_radius(screen_rect) < BORDER_RADIUS);
    }

    #[test]
    fn zoomed_out_panel_roundings_fit_visible_header_and_body() {
        let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(84.0, 34.0));
        let title_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(84.0, 5.0));
        let body_rect = egui::Rect::from_min_max(pos2(0.0, 5.0), pos2(84.0, 34.0));

        let roundings = panel_roundings(screen_rect, title_rect, body_rect);

        assert!(roundings.panel.nw <= title_rect.height() * 0.5);
        assert!(roundings.panel.ne <= title_rect.height() * 0.5);
        assert!(roundings.panel.sw <= body_rect.height() * 0.5);
        assert!(roundings.panel.se <= body_rect.height() * 0.5);
        assert_eq!(roundings.title.nw, roundings.panel.nw);
        assert_eq!(roundings.body.sw, roundings.panel.sw);
    }

    #[test]
    fn fast_path_keeps_live_terminal_rendering_when_panel_is_readable() {
        let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(420.0, 218.0));

        assert!(should_render_live_terminal(
            content_rect,
            1.0,
            PanelLod::Full,
            false
        ));
        assert!(should_render_live_terminal(
            content_rect,
            1.0,
            PanelLod::Full,
            true
        ));
    }

    #[test]
    fn focused_panels_get_full_render_tier() {
        let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(420.0, 218.0));

        assert_eq!(
            render_tier_for_panel(content_rect, 1.0, PanelLod::Full, false, true, false),
            RenderTier::Full
        );
    }

    #[test]
    fn background_streaming_panels_drop_to_preview_tier() {
        let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(200.0, 120.0));

        assert_eq!(
            render_tier_for_panel(content_rect, 1.0, PanelLod::Compact, false, false, true),
            RenderTier::Preview
        );
    }

    #[test]
    fn minimal_panels_keep_preview_badge_visible() {
        let content_rect = egui::Rect::from_min_size(pos2(0.0, 7.0), vec2(84.0, 44.0));

        assert_eq!(
            render_tier_for_panel(content_rect, 0.18, PanelLod::Minimal, false, false, false),
            RenderTier::Preview
        );
    }

    #[test]
    fn fast_path_only_defers_resize_during_active_resize_gesture() {
        let rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(320.0, 220.0));

        assert!(should_defer_terminal_resize(true, Some(rect)));
        assert!(!should_defer_terminal_resize(true, None));
        assert!(!should_defer_terminal_resize(false, Some(rect)));
    }

    #[test]
    fn intelligent_preview_detects_claude_code_from_prompt_command() {
        let label = infer_activity_label("Terminal", "Terminal", "(base) mauro@Mac ~ % claude");

        assert_eq!(label.as_deref(), Some("Claude Code"));
    }

    #[test]
    fn intelligent_preview_prefers_openclaude_over_generic_claude() {
        let label = infer_activity_label("Claude", "Terminal", "running openclaude in this panel");

        assert_eq!(label.as_deref(), Some("OpenClaude"));
    }

    #[test]
    fn preview_label_falls_back_to_clean_title_when_no_agent_is_detected() {
        let label = preview_label_text(None, "Deploy API");

        assert_eq!(label, "Deploy API");
    }

    #[test]
    fn tiny_title_bar_keeps_a_real_drag_hit_area() {
        let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(90.0, 52.0));
        let title_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(90.0, 4.0));
        let hit_rect = title_drag_hit_rect(screen_rect, title_rect);

        assert!(hit_rect.height() >= 16.0);
        assert!(hit_rect.width() > 20.0);
    }

    #[test]
    fn gesture_pointer_uses_latest_pointer_position_when_available() {
        let pointer = gesture_pointer_pos(
            Some(pos2(120.0, 80.0)),
            Some(pos2(100.0, 70.0)),
            Some(pos2(90.0, 60.0)),
        );

        assert_eq!(pointer, Some(pos2(120.0, 80.0)));
    }

    #[test]
    fn compact_panels_drag_from_the_body_instead_of_terminal_selection() {
        let panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
        let viewport = Viewport {
            pan: egui::Vec2::ZERO,
            zoom: 0.2,
        };
        let canvas_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let hit = panel.hit_test(pos2(40.0, 34.0), &viewport, canvas_rect);

        assert!(matches!(hit, Some(PanelHitArea::TitleBar)));
    }
}
