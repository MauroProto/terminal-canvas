use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Rounding, Sense, Stroke, Vec2};
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
use crate::collab::{
    PanelShareScope, SerializableModifiers, SharedPanelSnapshot, TerminalInputEvent,
};
use crate::orchestration::{AgentProvider, PanelOverlay, PanelRuntimeObservation};
use crate::runtime::{PtyManager, RenderTier, SessionSpec, SharedPtyHandle};
use crate::state::panel_state::{PanelPlacement, SavedPanelBounds, SnapSlot};
use crate::state::PanelState;
use crate::terminal::input::{
    is_paste_shortcut, key_to_bytes, paste_bytes, should_copy_selection, wheel_action, WheelAction,
};
use crate::terminal::layout::{
    cell_side_from_position, grid_metrics, grid_point_from_position, terminal_cell_from_pointer,
};
use crate::terminal::pty::{PtyHandle, TerminalScrollState};
use crate::terminal::renderer::{
    compute_grid_size, render_terminal, render_terminal_preview, render_terminal_reduced,
    TerminalGridCache, FONT_SIZE, MIN_TEXT_RENDER_FONT_SIZE, PAD_X, PAD_Y,
};
use crate::terminal::scrollbar::{
    render_scrollbar, scrollbar_pointer_to_scrollback, scrollbar_thumb_height, terminal_body_rect,
    terminal_scrollbar_rect,
};
use crate::terminal::session_controller::{session_spec, SessionController};
use crate::utils::platform::default_shell;

pub const TITLE_BAR_HEIGHT: f32 = 42.0;
pub const BORDER_RADIUS: f32 = 16.0;
pub const MIN_WIDTH: f32 = 260.0;
pub const MIN_HEIGHT: f32 = 180.0;
pub const RESIZE_GRIP_SIZE: f32 = 32.0;
pub const RESIZE_HIT_THICKNESS: f32 = 12.0;
pub const RESIZE_CORNER_SIZE: f32 = 28.0;
pub const PANEL_BG: Color32 = Color32::from_rgb(30, 30, 30);
pub const TITLE_BG: Color32 = Color32::from_rgb(38, 38, 58);
pub const BORDER_DEFAULT: Color32 = Color32::from_rgb(72, 72, 84);
pub const BORDER_FOCUS: Color32 = Color32::from_rgb(110, 110, 124);
pub const FG: Color32 = Color32::from_rgb(232, 232, 234);
pub const DIM_FG: Color32 = Color32::from_rgb(146, 146, 152);
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

fn share_scope_badge_colors(scope: PanelShareScope) -> (Color32, Color32, Color32) {
    match scope {
        PanelShareScope::Private => (
            Color32::from_rgb(70, 42, 46),
            Color32::from_rgb(176, 92, 102),
            Color32::from_rgb(248, 212, 218),
        ),
        PanelShareScope::VisibleOnly => (
            Color32::from_rgb(48, 56, 78),
            Color32::from_rgb(104, 138, 212),
            Color32::from_rgb(222, 232, 255),
        ),
        PanelShareScope::VisibleAndHistory => (
            Color32::from_rgb(68, 58, 34),
            Color32::from_rgb(188, 164, 92),
            Color32::from_rgb(248, 238, 204),
        ),
        PanelShareScope::Controllable => (
            Color32::from_rgb(42, 72, 48),
            Color32::from_rgb(98, 186, 122),
            Color32::from_rgb(224, 248, 230),
        ),
    }
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
    MinimizeButton,
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
    pub render_tier: Option<RenderTier>,
    pub cache_hit: bool,
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
    minimized: bool,
    placement: PanelPlacement,
    restore_placement: Option<PanelPlacement>,
    restore_bounds: Option<Rect>,
    session: SessionController,
    pub drag_virtual_pos: Option<Pos2>,
    pub resize_virtual_rect: Option<Rect>,
    bell_flash_until: f64,
    activity_label: Option<String>,
    command_buffer: String,
    last_activity_scan_at: f64,
    share_scope: PanelShareScope,
    render_cache: TerminalGridCache,
    last_scrollbar_state: Option<TerminalScrollState>,
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
            minimized: false,
            placement: PanelPlacement::Floating,
            restore_placement: None,
            restore_bounds: Some(Rect::from_min_size(position, normalize_panel_size(size))),
            session: SessionController::default(),
            drag_virtual_pos: None,
            resize_virtual_rect: None,
            bell_flash_until: 0.0,
            activity_label: None,
            command_buffer: String::new(),
            last_activity_scan_at: 0.0,
            share_scope: PanelShareScope::VisibleOnly,
            render_cache: TerminalGridCache::default(),
            last_scrollbar_state: None,
        }
    }

    pub fn from_saved(
        saved: PanelState,
        _ctx: &egui::Context,
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
        panel.focused = saved.focused && !saved.minimized;
        panel.minimized = saved.minimized;
        panel.placement = saved.placement.clone();
        panel.restore_placement = saved.restore_placement.clone();
        panel.restore_bounds = saved
            .restore_bounds
            .map(saved_bounds_to_rect)
            .or_else(|| Some(panel.rect()));
        panel.share_scope = saved.share_scope;
        let (cols, rows) = compute_grid_size(panel.size.x, panel.size.y - TITLE_BAR_HEIGHT);
        panel.session.restore_detached_with_spec(
            pty_manager,
            session_spec(panel.title.clone(), cwd.map(Path::to_path_buf), None, None),
            cols,
            rows,
        );
        panel
    }

    pub fn attach_session_with_spec(
        &mut self,
        pty_manager: Arc<Mutex<PtyManager>>,
        cwd: Option<&Path>,
        spec: SessionSpec,
    ) {
        let (cols, rows) = compute_grid_size(self.size.x, self.size.y - TITLE_BAR_HEIGHT);
        self.cwd_label = cwd_label(cwd);
        self.shell_label = shell_label();
        self.session
            .attach_new_with_spec(pty_manager, spec, cwd, cols, rows);
    }

    pub fn runtime_session_id(&self) -> Option<Uuid> {
        self.session.runtime_session_id()
    }

    pub fn runtime_session_attached(&self) -> bool {
        self.session.is_attached()
    }

    pub fn set_share_scope(&mut self, scope: PanelShareScope) {
        self.share_scope = scope;
    }

    pub fn provider_hint(&self) -> Option<AgentProvider> {
        self.activity_label
            .as_deref()
            .and_then(AgentProvider::detect)
            .or_else(|| AgentProvider::detect(&self.title))
            .or_else(|| AgentProvider::detect(&self.shell_title))
    }

    fn session_handle(&self) -> Option<SharedPtyHandle> {
        self.session.session_handle()
    }

    fn close_runtime_session(&mut self) {
        self.session.close();
    }

    fn with_pty<R>(&self, f: impl FnOnce(&PtyHandle) -> R) -> Option<R> {
        self.session.with_pty(f)
    }

    pub fn apply_resize(&mut self, rect: Rect) {
        self.position = rect.min;
        self.size = rect.size();
    }

    pub fn rect(&self) -> Rect {
        Rect::from_min_size(self.position, self.size)
    }

    pub fn is_alive(&self) -> bool {
        self.session.is_alive()
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
            minimized: self.minimized,
            placement: self.placement.clone(),
            restore_placement: self.restore_placement.clone(),
            restore_bounds: Some(rect_to_saved_bounds(
                self.restore_bounds.unwrap_or_else(|| self.rect()),
            )),
            share_scope: self.share_scope,
        }
    }

    pub fn minimized(&self) -> bool {
        self.minimized
    }

    pub fn set_minimized(&mut self, minimized: bool) {
        self.minimized = minimized;
        if minimized {
            self.focused = false;
            self.drag_virtual_pos = None;
            self.resize_virtual_rect = None;
        }
    }

    pub fn placement(&self) -> &PanelPlacement {
        &self.placement
    }

    pub fn set_placement(&mut self, placement: PanelPlacement) {
        self.placement = placement;
    }

    pub fn set_restore_placement(&mut self, placement: Option<PanelPlacement>) {
        self.restore_placement = placement;
    }

    pub fn set_restore_bounds(&mut self, rect: Option<Rect>) {
        self.restore_bounds = rect;
    }

    pub fn current_or_restore_rect(&self) -> Rect {
        self.restore_bounds.unwrap_or_else(|| self.rect())
    }

    pub fn capture_restore_bounds(&mut self) {
        self.restore_bounds = Some(self.rect());
    }

    pub fn maximize(&mut self, desktop_rect: Rect) {
        if !matches!(self.placement, PanelPlacement::Maximized) {
            self.capture_restore_bounds();
            self.restore_placement = Some(self.placement.clone());
        }
        self.placement = PanelPlacement::Maximized;
        self.apply_resize(desktop_rect);
    }

    pub fn snap_to(&mut self, slot: SnapSlot, desktop_rect: Rect) {
        if matches!(self.placement, PanelPlacement::Floating) {
            self.capture_restore_bounds();
        }
        self.placement = PanelPlacement::Snapped(slot);
        self.restore_placement = None;
        self.apply_resize(snap_slot_rect(slot, desktop_rect));
    }

    pub fn restore_window_placement(&mut self, desktop_rect: Rect) {
        match self.placement {
            PanelPlacement::Floating => {
                if let Some(rect) = self.restore_bounds {
                    self.apply_resize(rect);
                }
            }
            PanelPlacement::Snapped(slot) => {
                let rect = self.rect();
                self.apply_resize(normalize_snapped_rect(slot, rect, desktop_rect));
            }
            PanelPlacement::Maximized => {
                match self
                    .restore_placement
                    .take()
                    .unwrap_or(PanelPlacement::Floating)
                {
                    PanelPlacement::Floating => {
                        self.placement = PanelPlacement::Floating;
                        if let Some(rect) = self.restore_bounds {
                            self.apply_resize(rect);
                        }
                    }
                    PanelPlacement::Snapped(slot) => {
                        self.placement = PanelPlacement::Snapped(slot);
                        let rect = self.current_or_restore_rect();
                        self.apply_resize(normalize_snapped_rect(slot, rect, desktop_rect));
                    }
                    PanelPlacement::Maximized => {
                        self.placement = PanelPlacement::Maximized;
                        self.apply_resize(desktop_rect);
                    }
                }
            }
        }
    }

    pub fn sync_title(&mut self) {
        let shell_title = self.session.title_snapshot();
        if let Some(shell_title) = shell_title {
            self.apply_shell_title(shell_title);
            if let Some(activity_label) = infer_activity_label(&self.title, &self.shell_title, "") {
                self.activity_label = Some(activity_label);
            }
        }
    }

    pub fn orchestration_observation(&self, workspace_id: Uuid) -> PanelRuntimeObservation {
        let mut visible_text = String::new();
        let attached = self.runtime_session_attached();
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
            visible_text: if self.minimized || !attached {
                String::new()
            } else {
                visible_text
            },
            alive: self.is_alive(),
            recent_output: if self.minimized || !attached {
                false
            } else {
                recent_output
            },
            attached,
            minimized: self.minimized,
        }
    }

    pub fn handle_input(&mut self, ctx: &egui::Context) {
        if !self.focused {
            return;
        }

        let _ = ctx;
        self.session.ensure_attached();
        let mode = self.session.input_mode();
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

        self.session.ensure_attached();
        let mode = self.session.input_mode();
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
                    self.apply_scroll_delta(*delta, None, &Viewport::default(), Rect::EVERYTHING);
                }
            }
        }
    }

    pub fn handle_scroll(
        &mut self,
        delta: f32,
        pointer: Option<Pos2>,
        viewport: &Viewport,
        canvas_rect: Rect,
        _ctx: &egui::Context,
    ) {
        self.apply_scroll_delta(delta, pointer, viewport, canvas_rect);
    }

    fn apply_scroll_delta(
        &mut self,
        delta: f32,
        pointer: Option<Pos2>,
        viewport: &Viewport,
        canvas_rect: Rect,
    ) {
        self.session.ensure_attached();
        if !self.session.is_attached() {
            return;
        }
        let mode = self.session.input_mode();
        let point = pointer
            .and_then(|pointer| self.mouse_cell_from_pointer(pointer, viewport, canvas_rect));

        match wheel_action(delta, &mode, point) {
            Some(WheelAction::Pty(bytes)) => {
                let _ = self.with_pty(|pty| pty.write_all(&bytes));
            }
            Some(WheelAction::Scrollback(lines)) => {
                let _ = self.with_pty(|pty| pty.scroll_display(Scroll::Delta(lines)));
            }
            None => {}
        }
    }

    pub fn shared_snapshot(&self) -> SharedPanelSnapshot {
        let mut visible_text = String::new();
        let mut history_text = String::new();
        if self.share_scope.allows_visible_text() {
            if let Some(handle) = self.session_handle() {
                if let Ok(pty) = handle.lock() {
                    if let Ok(term) = pty.term.try_lock() {
                        visible_text = visible_text_snapshot(&term, 18, 180);
                        if self.share_scope.allows_history() {
                            history_text = visible_text_snapshot(&term, 80, 220);
                        }
                    }
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
            minimized: self.minimized,
            alive: self.is_alive(),
            preview_label: self.preview_label(),
            share_scope: self.share_scope,
            visible_text,
            history_text,
            controller: None,
            controller_name: None,
            queue_len: 0,
        }
    }

    pub fn scroll_hit_test(&self, pos: Pos2, viewport: &Viewport, canvas_rect: Rect) -> bool {
        if self.minimized {
            return false;
        }
        self.content_screen_rect(viewport, canvas_rect)
            .intersect(canvas_rect)
            .contains(pos)
            || self
                .scrollbar_screen_rect(viewport, canvas_rect)
                .intersect(canvas_rect)
                .contains(pos)
    }

    fn mouse_cell_from_pointer(
        &self,
        pointer: Pos2,
        viewport: &Viewport,
        canvas_rect: Rect,
    ) -> Option<crate::terminal::input::GridPoint> {
        let content_rect = self
            .content_screen_rect(viewport, canvas_rect)
            .intersect(canvas_rect);
        let (column, row) = terminal_mouse_cell_from_pointer(content_rect, pointer, viewport.zoom)?;
        let (last_cols, last_rows) = self.session.last_grid_size();
        let max_column = last_cols as usize - 1;
        let max_row = last_rows as usize - 1;
        Some(crate::terminal::input::GridPoint {
            column: column.min(max_column),
            line: row.min(max_row),
        })
    }

    pub fn hit_test(
        &self,
        pos: Pos2,
        viewport: &Viewport,
        canvas_rect: Rect,
    ) -> Option<PanelHitArea> {
        if self.minimized {
            return None;
        }
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
        if should_draw_window_controls(screen_rect, title_rect)
            && minimize_rect(title_rect)
                .intersect(canvas_rect)
                .contains(pos)
        {
            return Some(PanelHitArea::MinimizeButton);
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
        let content_rect = terminal_body_rect(body_rect);
        let scrollbar_rect = terminal_scrollbar_rect(body_rect);
        let lod = panel_lod(screen_rect, title_rect);
        let painter = ui.painter().with_clip_rect(canvas_rect);
        let body_hit_rect = body_input_rect(content_rect).intersect(canvas_rect);
        let scrollbar_hit_rect = scrollbar_rect
            .expand2(vec2(2.0, 2.0))
            .intersect(canvas_rect);

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
                self.session.clear_selection();
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

        let scrollbar_response =
            if scrollbar_hit_rect.width() > 0.0 && scrollbar_hit_rect.height() > 0.0 {
                Some(ui.interact(
                    scrollbar_hit_rect,
                    ui.id().with(("scrollbar", self.id)),
                    Sense::click_and_drag(),
                ))
            } else {
                None
            };
        if let Some(scrollbar_response) = &scrollbar_response {
            if scrollbar_response.clicked() || scrollbar_response.dragged() {
                if let Some(pointer) = scrollbar_response.interact_pointer_pos() {
                    let _ = self.with_pty(|pty| {
                        if let Some(scroll_state) = pty.scroll_state() {
                            let thumb_height = scrollbar_thumb_height(
                                scrollbar_rect.height(),
                                scroll_state.visible_rows,
                                scroll_state.history_size,
                            );
                            let target = scrollbar_pointer_to_scrollback(
                                pointer,
                                scrollbar_rect,
                                thumb_height,
                                scroll_state.history_size,
                            );
                            pty.scroll_to_display_offset(target);
                        }
                    });
                }
            }
        }

        if !ui.ctx().input(|i| i.pointer.primary_down()) {
            self.drag_virtual_pos = None;
            self.resize_virtual_rect = None;
        }

        if self.session.take_bell() {
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
        if matches!(lod, PanelLod::Full | PanelLod::Compact) && title_rect.width() >= 220.0 {
            let badge_text = self.share_scope.label();
            let badge_width =
                ((badge_text.len() as f32 * 7.2 + 18.0) * chrome_zoom).clamp(52.0, 96.0);
            let badge_height = (20.0 * chrome_zoom).clamp(16.0, 20.0);
            let badge_rect = Rect::from_center_size(
                pos2(
                    title_rect.right() - (14.0 * chrome_zoom).clamp(8.0, 14.0) - badge_width * 0.5,
                    title_rect.center().y,
                ),
                vec2(badge_width, badge_height),
            );
            let (fill, stroke, text) = share_scope_badge_colors(self.share_scope);
            chrome_painter.rect_filled(badge_rect, badge_height * 0.5, fill);
            chrome_painter.rect_stroke(badge_rect, badge_height * 0.5, Stroke::new(1.0, stroke));
            chrome_painter.text(
                badge_rect.center(),
                Align2::CENTER_CENTER,
                badge_text,
                FontId::proportional((11.0 * chrome_zoom).clamp(7.0, 11.0)),
                text,
            );
        }
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
        let mut scrollbar_state = self.last_scrollbar_state;
        let render_tier = render_tier_for_panel(
            content_rect,
            zoom,
            lod,
            fast_path_render,
            self.focused,
            self.session.with_pty(is_streaming_output).unwrap_or(false),
        );
        interaction.render_tier = Some(render_tier);
        if matches!(render_tier, RenderTier::Full | RenderTier::ReducedLive) {
            let (cols, rows) = compute_grid_size(self.size.x, self.size.y - TITLE_BAR_HEIGHT);
            let defer_resize =
                should_defer_terminal_resize(fast_path_render, self.resize_virtual_rect);
            self.session.sync_grid_size(cols, rows, defer_resize);
        }

        if let Some(handle) = self.session_handle() {
            if let Ok(pty) = handle.lock() {
                let scan_activity = should_refresh_activity_label(self.last_activity_scan_at, now);
                let mut scanned_activity_label = None;
                if matches!(render_tier, RenderTier::Full | RenderTier::ReducedLive) {
                    if let Ok(mut term) = pty.term.try_lock() {
                        term.is_focused = self.focused;
                        scrollbar_state = Some(TerminalScrollState {
                            display_offset: term.grid().display_offset(),
                            visible_rows: term.screen_lines(),
                            history_size: term.grid().history_size(),
                        });
                        if scan_activity {
                            scanned_activity_label = Some(infer_activity_label_from_term(
                                &title_snapshot,
                                &shell_title_snapshot,
                                &term,
                            ));
                        }
                        match render_tier {
                            RenderTier::Full => {
                                interaction.cache_hit = render_terminal(
                                    &content_painter,
                                    content_rect,
                                    &term,
                                    self.focused,
                                    now,
                                    zoom,
                                    content_rounding,
                                    Some(&mut self.render_cache),
                                    pty.render_revision(),
                                );
                            }
                            RenderTier::ReducedLive => {
                                interaction.cache_hit = render_terminal_reduced(
                                    &content_painter,
                                    content_rect,
                                    &term,
                                    self.focused,
                                    now,
                                    zoom,
                                    content_rounding,
                                    Some(&mut self.render_cache),
                                    pty.render_revision(),
                                );
                            }
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
        } else if let Some(error) = self.session.spawn_error() {
            scrollbar_state = None;
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
        self.last_scrollbar_state = scrollbar_state;

        if let Some(scroll_state) = scrollbar_state {
            render_scrollbar(
                &chrome_painter,
                scrollbar_rect.intersect(canvas_rect),
                scroll_state.display_offset,
                scroll_state.visible_rows,
                scroll_state.history_size,
                self.focused
                    || scrollbar_response
                        .as_ref()
                        .is_some_and(|response| response.hovered()),
            );
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
        self.session.selected_text()
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

    fn content_screen_rect(&self, viewport: &Viewport, canvas_rect: Rect) -> Rect {
        terminal_body_rect(self.body_screen_rect(viewport, canvas_rect))
    }

    fn scrollbar_screen_rect(&self, viewport: &Viewport, canvas_rect: Rect) -> Rect {
        terminal_scrollbar_rect(self.body_screen_rect(viewport, canvas_rect))
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
        self.session.update_session_title_hint(&self.title);
    }

    fn window_title(&self, screen_width: f32) -> String {
        let (cols, rows) = self.session.last_grid_size();
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
        pty.mark_render_dirty();
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
        pty.mark_render_dirty();
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
        let term = pty.term.try_lock().ok()?;
        let point = terminal_cell_from_pointer(
            content_rect,
            pointer,
            zoom,
            term.screen_lines() as u16,
            term.columns() as u16,
        )?;
        let side = cell_side_from_position(content_rect, pointer, zoom, point);

        Some((
            viewport_to_point(
                term.grid().display_offset(),
                Point::new(point.line, Column(point.column)),
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

fn minimize_rect(title_rect: Rect) -> Rect {
    let chrome_zoom = chrome_zoom_from_title_rect(title_rect);
    Rect::from_center_size(
        pos2(
            title_rect.left() + 46.0 * chrome_zoom,
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

fn rect_to_saved_bounds(rect: Rect) -> SavedPanelBounds {
    SavedPanelBounds::new([rect.min.x, rect.min.y], [rect.width(), rect.height()])
}

fn saved_bounds_to_rect(bounds: SavedPanelBounds) -> Rect {
    Rect::from_min_size(
        pos2(bounds.position[0], bounds.position[1]),
        vec2(bounds.size[0], bounds.size[1]),
    )
}

pub fn snap_slot_rect(slot: SnapSlot, desktop_rect: Rect) -> Rect {
    let half_width = desktop_rect.width() * 0.5;
    let half_height = desktop_rect.height() * 0.5;

    match slot {
        SnapSlot::LeftHalf => {
            Rect::from_min_size(desktop_rect.min, vec2(half_width, desktop_rect.height()))
        }
        SnapSlot::RightHalf => Rect::from_min_size(
            pos2(desktop_rect.center().x, desktop_rect.top()),
            vec2(half_width, desktop_rect.height()),
        ),
        SnapSlot::TopHalf => {
            Rect::from_min_size(desktop_rect.min, vec2(desktop_rect.width(), half_height))
        }
        SnapSlot::BottomHalf => Rect::from_min_size(
            pos2(desktop_rect.left(), desktop_rect.center().y),
            vec2(desktop_rect.width(), half_height),
        ),
        SnapSlot::TopLeft => Rect::from_min_size(desktop_rect.min, vec2(half_width, half_height)),
        SnapSlot::TopRight => Rect::from_min_size(
            pos2(desktop_rect.center().x, desktop_rect.top()),
            vec2(half_width, half_height),
        ),
        SnapSlot::BottomLeft => Rect::from_min_size(
            pos2(desktop_rect.left(), desktop_rect.center().y),
            vec2(half_width, half_height),
        ),
        SnapSlot::BottomRight => {
            Rect::from_min_size(desktop_rect.center(), vec2(half_width, half_height))
        }
        SnapSlot::Maximized => desktop_rect,
    }
}

pub fn normalize_snapped_rect(slot: SnapSlot, rect: Rect, desktop_rect: Rect) -> Rect {
    let min_width = MIN_WIDTH.min(desktop_rect.width());
    let min_height = MIN_HEIGHT.min(desktop_rect.height());

    match slot {
        SnapSlot::LeftHalf => {
            let width = rect.width().clamp(min_width, desktop_rect.width());
            Rect::from_min_max(
                desktop_rect.min,
                pos2(
                    (desktop_rect.left() + width).min(desktop_rect.right()),
                    desktop_rect.bottom(),
                ),
            )
        }
        SnapSlot::RightHalf => {
            let width = rect.width().clamp(min_width, desktop_rect.width());
            Rect::from_min_max(
                pos2(
                    (desktop_rect.right() - width).max(desktop_rect.left()),
                    desktop_rect.top(),
                ),
                desktop_rect.max,
            )
        }
        SnapSlot::TopHalf => {
            let height = rect.height().clamp(min_height, desktop_rect.height());
            Rect::from_min_max(
                desktop_rect.min,
                pos2(
                    desktop_rect.right(),
                    (desktop_rect.top() + height).min(desktop_rect.bottom()),
                ),
            )
        }
        SnapSlot::BottomHalf => {
            let height = rect.height().clamp(min_height, desktop_rect.height());
            Rect::from_min_max(
                pos2(
                    desktop_rect.left(),
                    (desktop_rect.bottom() - height).max(desktop_rect.top()),
                ),
                desktop_rect.max,
            )
        }
        SnapSlot::TopLeft => {
            let width = rect.width().clamp(min_width, desktop_rect.width());
            let height = rect.height().clamp(min_height, desktop_rect.height());
            Rect::from_min_max(
                desktop_rect.min,
                pos2(
                    (desktop_rect.left() + width).min(desktop_rect.right()),
                    (desktop_rect.top() + height).min(desktop_rect.bottom()),
                ),
            )
        }
        SnapSlot::TopRight => {
            let width = rect.width().clamp(min_width, desktop_rect.width());
            let height = rect.height().clamp(min_height, desktop_rect.height());
            Rect::from_min_max(
                pos2(
                    (desktop_rect.right() - width).max(desktop_rect.left()),
                    desktop_rect.top(),
                ),
                pos2(
                    desktop_rect.right(),
                    (desktop_rect.top() + height).min(desktop_rect.bottom()),
                ),
            )
        }
        SnapSlot::BottomLeft => {
            let width = rect.width().clamp(min_width, desktop_rect.width());
            let height = rect.height().clamp(min_height, desktop_rect.height());
            Rect::from_min_max(
                pos2(
                    desktop_rect.left(),
                    (desktop_rect.bottom() - height).max(desktop_rect.top()),
                ),
                pos2(
                    (desktop_rect.left() + width).min(desktop_rect.right()),
                    desktop_rect.bottom(),
                ),
            )
        }
        SnapSlot::BottomRight => {
            let width = rect.width().clamp(min_width, desktop_rect.width());
            let height = rect.height().clamp(min_height, desktop_rect.height());
            Rect::from_min_max(
                pos2(
                    (desktop_rect.right() - width).max(desktop_rect.left()),
                    (desktop_rect.bottom() - height).max(desktop_rect.top()),
                ),
                desktop_rect.max,
            )
        }
        SnapSlot::Maximized => desktop_rect,
    }
}

#[cfg(test)]
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

fn terminal_mouse_cell_from_pointer(
    content_rect: Rect,
    pointer: Pos2,
    zoom: f32,
) -> Option<(usize, usize)> {
    let metrics = grid_metrics(zoom);
    let rect = Rect::from_min_max(
        Pos2::new(
            content_rect.left() + PAD_X * zoom.max(0.01),
            content_rect.top() + PAD_Y * zoom.max(0.01),
        ),
        content_rect.right_bottom(),
    );
    let point = grid_point_from_position(rect, pointer, &metrics, u16::MAX, u16::MAX)?;
    Some((point.column, point.line))
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
    _focused: bool,
    _streaming: bool,
) -> RenderTier {
    let previewable = content_rect.width() >= 24.0 && content_rect.height() >= 18.0;
    if !previewable {
        return RenderTier::Hidden;
    }
    if matches!(lod, PanelLod::Minimal) || !should_render_terminal_contents(content_rect, zoom) {
        return RenderTier::Preview;
    }
    RenderTier::Full
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

#[cfg(test)]
mod tests {
    use crate::app::gesture_pointer_pos;
    use crate::canvas::viewport::Viewport;
    use crate::collab::PanelShareScope;
    use crate::runtime::RenderTier;
    use crate::terminal::input::{
        alt_screen_scroll_sequence, mouse_scroll_button, mouse_scroll_sgr_sequence,
        scroll_lines_from_input_delta, scrollback_delta_from_input,
    };
    use crate::terminal::layout::{grid_point_from_position, GridMetrics};
    use crate::terminal::scrollbar::{
        scrollbar_pointer_to_scrollback, scrollbar_thumb_height, terminal_scrollbar_rect,
    };
    use egui::{pos2, vec2, Color32};

    use super::{
        chrome_zoom, close_rect, drag_target_from_origin, infer_activity_label, minimize_rect,
        panel_corner_radius, panel_lod, panel_roundings, preview_label_text, render_tier_for_panel,
        resize_target_from_origin, shell_label, should_defer_terminal_resize,
        should_draw_resize_grip, should_draw_title_text, should_draw_window_controls,
        should_render_live_terminal, should_render_terminal_contents,
        terminal_mouse_cell_from_pointer, title_bar_height, title_drag_hit_rect, PanelHitArea,
        PanelLod, ResizeHandle, TerminalPanel, BORDER_RADIUS, MIN_HEIGHT, MIN_WIDTH,
        TITLE_BAR_HEIGHT,
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
        panel.session.set_last_grid_size_for_tests(80, 24);

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
    fn minimize_button_sits_to_right_of_close_button() {
        let title_rect = egui::Rect::from_min_size(pos2(100.0, 50.0), vec2(500.0, 42.0));
        let close = close_rect(title_rect);
        let minimize = minimize_rect(title_rect);

        assert!(minimize.center().x > close.center().x);
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
        panel.session.set_last_grid_size_for_tests(42, 25);

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
    fn fast_path_keeps_background_panels_live() {
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
    fn fast_path_still_keeps_focused_panels_live() {
        let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(420.0, 218.0));

        assert_eq!(
            render_tier_for_panel(content_rect, 1.0, PanelLod::Full, true, true, false),
            RenderTier::Full
        );
    }

    #[test]
    fn background_streaming_panels_stay_live_when_renderable() {
        let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(200.0, 120.0));

        assert_eq!(
            render_tier_for_panel(content_rect, 1.0, PanelLod::Compact, false, false, true),
            RenderTier::Full
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
    fn mac_native_upward_input_scroll_moves_toward_recent_output() {
        assert_eq!(scroll_lines_from_input_delta(-48.0), 2);
        #[cfg(target_os = "macos")]
        {
            assert_eq!(scrollback_delta_from_input(-48.0), -2);
            assert_eq!(alt_screen_scroll_sequence(-1.0), b"\x1b[B");
            assert_eq!(mouse_scroll_button(-1.0), 65);
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(scrollback_delta_from_input(-48.0), 2);
            assert_eq!(alt_screen_scroll_sequence(-1.0), b"\x1b[A");
            assert_eq!(mouse_scroll_button(-1.0), 64);
        }
    }

    #[test]
    fn mac_native_downward_input_scroll_moves_toward_history() {
        assert_eq!(scroll_lines_from_input_delta(48.0), 2);
        #[cfg(target_os = "macos")]
        {
            assert_eq!(scrollback_delta_from_input(48.0), 2);
            assert_eq!(alt_screen_scroll_sequence(1.0), b"\x1b[A");
            assert_eq!(mouse_scroll_button(1.0), 64);
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(scrollback_delta_from_input(48.0), -2);
            assert_eq!(alt_screen_scroll_sequence(1.0), b"\x1b[B");
            assert_eq!(mouse_scroll_button(1.0), 65);
        }
    }

    #[test]
    fn mouse_mode_scroll_reports_pointer_cell_instead_of_fixed_origin() {
        let content_rect = egui::Rect::from_min_size(pos2(10.0, 20.0), vec2(400.0, 240.0));
        let pointer = pos2(10.0 + 7.2 * 5.4, 20.0 + 14.4 * 3.2);

        let (column, row) = terminal_mouse_cell_from_pointer(content_rect, pointer, 1.0).unwrap();
        let seq = mouse_scroll_sgr_sequence(64, column, row);

        assert_eq!((column, row), (3, 2));
        assert_eq!(seq, b"\x1b[<64;4;3M".to_vec());
    }

    #[test]
    fn grid_point_from_position_clamps_to_visible_terminal_bounds() {
        let rect = egui::Rect::from_min_size(pos2(100.0, 80.0), vec2(80.0, 48.0));
        let metrics = GridMetrics {
            char_width: 8.0,
            line_height: 16.0,
        };

        let point =
            grid_point_from_position(rect, pos2(179.0, 127.0), &metrics, 3, 10).expect("point");

        assert_eq!(point.line, 2);
        assert_eq!(point.column, 9);
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

    #[test]
    fn hit_test_detects_minimize_button() {
        let panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
        let viewport = Viewport::default();
        let canvas_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let title_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 42.0));
        let hit = panel.hit_test(minimize_rect(title_rect).center(), &viewport, canvas_rect);

        assert_eq!(hit, Some(PanelHitArea::MinimizeButton));
    }

    #[test]
    fn resize_hit_areas_are_slightly_more_generous() {
        let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 260.0));

        assert!(ResizeHandle::Right.hit_rect(screen_rect).width() >= 12.0);
        assert!(ResizeHandle::Bottom.hit_rect(screen_rect).height() >= 12.0);
        assert!(ResizeHandle::BottomRight.hit_rect(screen_rect).width() >= 28.0);
        assert!(ResizeHandle::BottomRight.hit_rect(screen_rect).height() >= 28.0);
    }

    #[test]
    fn scrollbar_thumb_height_stays_within_track_bounds() {
        assert!((scrollbar_thumb_height(12.0, 50, 0) - 12.0).abs() <= f32::EPSILON);

        let thumb_height = scrollbar_thumb_height(120.0, 24, 240);
        assert!(thumb_height >= 18.0);
        assert!(thumb_height <= 120.0);
    }

    #[test]
    fn scrollbar_pointer_maps_to_expected_scrollback_extremes() {
        let track_rect = egui::Rect::from_min_size(pos2(10.0, 20.0), vec2(12.0, 100.0));
        let thumb_height = 20.0;

        assert_eq!(
            scrollbar_pointer_to_scrollback(
                pos2(16.0, track_rect.max.y),
                track_rect,
                thumb_height,
                200
            ),
            0
        );
        assert_eq!(
            scrollbar_pointer_to_scrollback(
                pos2(16.0, track_rect.min.y),
                track_rect,
                thumb_height,
                200
            ),
            200
        );
    }

    #[test]
    fn scroll_hit_target_includes_scrollbar_track() {
        let panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
        let viewport = Viewport::default();
        let canvas_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let body_rect = egui::Rect::from_min_max(pos2(0.0, 42.0), pos2(400.0, 300.0));
        let pointer = terminal_scrollbar_rect(body_rect).center();

        assert!(panel.scroll_hit_test(pointer, &viewport, canvas_rect));
    }

    #[test]
    fn shared_snapshot_reports_private_scope() {
        let mut panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
        panel.set_share_scope(PanelShareScope::Private);

        let snapshot = panel.shared_snapshot();

        assert_eq!(snapshot.share_scope, PanelShareScope::Private);
        assert!(snapshot.visible_text.is_empty());
        assert!(snapshot.history_text.is_empty());
    }
}
