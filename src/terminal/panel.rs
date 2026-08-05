mod activity;
mod chrome;
#[cfg(test)]
mod tests;

use activity::*;
use chrome::*;
pub use chrome::{normalize_snapped_rect, snap_slot_rect};

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{
    pos2, vec2, Align2, Color32, FontId, Modifiers, PointerButton, Pos2, Rect, Rounding, Sense,
    Stroke, Vec2,
};
use uuid::Uuid;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{point_to_viewport, viewport_to_point, Term};

use crate::canvas::config::normalize_panel_size;
use crate::canvas::config::SNAP_THRESHOLD;
use crate::canvas::snap::{snap_resize, SnapGuide};
use crate::canvas::viewport::Viewport;
use crate::collab::{
    PanelShareScope, SerializableModifiers, SharedPanelSnapshot, TerminalInputEvent,
};
use crate::orchestration::{AgentProvider, PanelOverlay, PanelRuntimeObservation};
use crate::runtime::{PtyManager, RenderTier, SessionSpec, SharedPtyHandle};
use crate::state::panel_state::{PanelPlacement, SavedPanelBounds, SnapSlot};
use crate::state::PanelState;
#[cfg(feature = "ghostty-vt")]
use crate::terminal::backend::TerminalBackendKind;
use crate::terminal::input::{
    agent_prompt_bytes, clipboard_event_fallback_bytes, is_paste_shortcut, key_to_bytes,
    mouse_click_sgr_sequence, mouse_motion_sgr_sequence, paste_bytes, should_copy_selection,
    wheel_action, InputMode, ScrollAccumulator, WheelAction,
};
use crate::terminal::layout::{
    cell_side_from_position, grid_metrics, grid_padding, grid_point_from_position,
    terminal_cell_from_pointer,
};
use crate::terminal::metrics::{base_font_size, MIN_TEXT_RENDER_FONT_SIZE, PAD_X, PAD_Y};
use crate::terminal::pty::{PtyHandle, TerminalScrollState};
use crate::terminal::renderer::{
    compute_grid_size, render_terminal, render_terminal_preview, render_terminal_reduced,
    TerminalGridCache,
};
#[cfg(feature = "ghostty-vt")]
use crate::terminal::renderer::{render_ghostty_text_snapshot, GhosttyGridCache};
use crate::terminal::scrollbar::{
    scrollbar_pointer_to_scrollback, scrollbar_thumb_height, terminal_body_rect,
    terminal_scrollbar_rect,
};
use crate::terminal::search::{
    display_offset_for_match, find_next, SearchQuery, MAX_HIGHLIGHT_LINES,
};
use crate::terminal::session_controller::{session_spec, SessionController};
use crate::utils::platform::default_shell;

pub const TITLE_BAR_HEIGHT: f32 = 28.0;
pub const BORDER_RADIUS: f32 = 0.0;
pub const MIN_WIDTH: f32 = 260.0;
pub const MIN_HEIGHT: f32 = 180.0;
#[allow(dead_code)]
pub const RESIZE_GRIP_SIZE: f32 = 32.0;
pub const RESIZE_HIT_THICKNESS: f32 = 12.0;
#[allow(dead_code)]
pub const RESIZE_CORNER_SIZE: f32 = 28.0;
pub const PANEL_BG: Color32 = Color32::from_rgb(18, 18, 18);
pub const TITLE_BG: Color32 = Color32::from_rgb(26, 26, 26);
pub const BORDER_DEFAULT: Color32 = Color32::from_rgb(56, 56, 56);
pub const BORDER_FOCUS: Color32 = Color32::from_rgb(110, 110, 110);
pub const FG: Color32 = Color32::from_rgb(244, 244, 244);
pub const DIM_FG: Color32 = Color32::from_rgb(110, 110, 110);
/// Fondo del badge de branch: apenas más claro que la barra de título.
const BRANCH_BADGE_BG: Color32 = Color32::from_rgb(38, 38, 38);
/// Punto que marca "hay cambios sin commitear" (ámbar, no rojo: no es error).
const BRANCH_DIRTY_DOT: Color32 = Color32::from_rgb(226, 178, 96);
pub const MAC_RED: Color32 = Color32::from_rgb(244, 244, 244);
pub const MAC_YELLOW: Color32 = Color32::from_rgb(170, 170, 170);
pub const MAC_GREEN: Color32 = Color32::from_rgb(208, 208, 208);
pub const CHROME_ZOOM_MAX: f32 = 1.0;
pub const MIN_CONTROL_STRIP_WIDTH: f32 = 72.0;
pub const MIN_TITLE_TEXT_WIDTH: f32 = 132.0;
#[allow(dead_code)]
pub const MIN_RESIZE_GRIP_WIDTH: f32 = 150.0;
#[allow(dead_code)]
pub const MIN_RESIZE_GRIP_HEIGHT: f32 = 110.0;
pub fn min_terminal_render_zoom() -> f32 {
    MIN_TEXT_RENDER_FONT_SIZE / base_font_size()
}
pub const MIN_TERMINAL_RENDER_WIDTH: f32 = 40.0;
pub const MIN_TERMINAL_RENDER_HEIGHT: f32 = 28.0;

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
    #[allow(dead_code)]
    Resize(ResizeHandle),
}

#[derive(Default)]
pub struct PanelInteraction {
    pub clicked: bool,
    pub hovered_terminal: bool,
    pub guides: Vec<SnapGuide>,
    pub render_tier: Option<RenderTier>,
    pub cache_hit: bool,
}

/// Estado de búsqueda en el scrollback de este panel. La consulta se edita
/// desde la barra de búsqueda de la app; el match actual se resalta y Enter
/// salta al siguiente (con wrap-around).
#[derive(Default, Clone)]
pub struct PanelSearch {
    pub query: String,
    pub current: Option<(Point, Point)>,
    /// Resultado del último `search_find_next`: `Some(false)` = sin
    /// coincidencias (para mostrar feedback), `None` = todavía no se buscó.
    pub found: Option<bool>,
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
    // Privado a propósito: el foco lo administra el Workspace (dueño de la
    // invariante "a lo sumo un panel enfocado") vía set_focused.
    focused: bool,
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
    #[cfg(feature = "ghostty-vt")]
    ghostty_render_cache: GhosttyGridCache,
    last_scrollbar_state: Option<TerminalScrollState>,
    scroll_accumulator: ScrollAccumulator,
    last_mouse_cell: Option<(usize, usize)>,
    search: Option<PanelSearch>,
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
            #[cfg(feature = "ghostty-vt")]
            ghostty_render_cache: GhosttyGridCache::default(),
            last_scrollbar_state: None,
            scroll_accumulator: ScrollAccumulator::default(),
            last_mouse_cell: None,
            search: None,
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

    /// Último cwd reportado por el shell vía OSC 7, si el shell lo emite.
    pub fn current_cwd(&self) -> Option<String> {
        self.with_pty(|pty| pty.current_cwd()).flatten()
    }

    pub fn set_share_scope(&mut self, scope: PanelShareScope) {
        self.share_scope = scope;
    }

    pub fn share_scope(&self) -> PanelShareScope {
        self.share_scope
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

    pub fn focused(&self) -> bool {
        self.focused
    }

    /// Único punto de escritura del foco desde afuera del panel. Lo llama el
    /// Workspace, que mantiene la invariante de foco único; no usar directo.
    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
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
        let mut agent_status = None;
        let attached = self.runtime_session_attached();
        let recent_output = self
            .with_pty(|pty| {
                if let Ok(term) = pty.term.try_lock() {
                    visible_text = visible_text_snapshot(&term, 16, 180);
                }
                agent_status = pty.agent_status_snapshot();
                pty.output_elapsed() <= Duration::from_secs(4)
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
            agent_status: if self.minimized || !attached {
                None
            } else {
                agent_status
            },
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
                    // egui-winit intercepta Cmd+C/Cmd+X (y Ctrl+C/Ctrl+X donde
                    // Ctrl es "command") y entrega estos eventos en lugar de
                    // la tecla: el atajo de copiar solo llega por acá.
                    egui::Event::Copy | egui::Event::Cut => {
                        if has_selection {
                            if let Some(text) = self.selected_text() {
                                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                    let _ = clipboard.set_text(text);
                                }
                            }
                        } else if let Some(bytes) =
                            clipboard_event_fallback_bytes(matches!(event, egui::Event::Cut))
                        {
                            let _ = self.with_pty(|pty| pty.write_all(bytes));
                        }
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

        match wheel_action(delta, &mode, point, &mut self.scroll_accumulator) {
            Some(WheelAction::Pty(bytes)) => {
                let _ = self.with_pty(|pty| pty.write_all(&bytes));
            }
            Some(WheelAction::Scrollback(lines)) => {
                let _ = self.with_pty(|pty| pty.scroll_display(Scroll::Delta(lines)));
            }
            None => {}
        }
    }

    pub fn search_active(&self) -> bool {
        self.search.is_some()
    }

    pub fn search_query(&self) -> &str {
        self.search
            .as_ref()
            .map(|search| search.query.as_str())
            .unwrap_or("")
    }

    pub fn search_open(&mut self) {
        if self.search.is_none() {
            self.search = Some(PanelSearch::default());
        }
    }

    pub fn search_close(&mut self) {
        if self.search.take().is_some() {
            self.session.with_pty(PtyHandle::mark_render_dirty);
        }
    }

    /// Actualiza la consulta; si cambió, descarta el match actual (Enter lo
    /// re-busca desde el inicio).
    pub fn search_set_query(&mut self, query: String) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        if search.query != query {
            search.query = query;
            search.current = None;
            search.found = None;
        }
    }

    /// Resultado del último `search_find_next` (para feedback en la barra).
    pub fn search_found(&self) -> Option<bool> {
        self.search.as_ref().and_then(|search| search.found)
    }

    /// Busca el próximo match desde el actual (wrap-around al fondo del
    /// historial si no queda nada adelante) y lo revela haciendo scroll.
    pub fn search_find_next(&mut self) {
        let Some((query_text, after)) = self
            .search
            .as_ref()
            .map(|search| (search.query.clone(), search.current.map(|(_, end)| end)))
        else {
            return;
        };
        let mut query = match SearchQuery::compile(&query_text) {
            Some(query) => query,
            None => {
                if let Some(search) = self.search.as_mut() {
                    search.current = None;
                }
                return;
            }
        };
        let Some(handle) = self.session_handle() else {
            return;
        };
        let Ok(pty) = handle.lock() else {
            return;
        };
        let new_match = {
            let Ok(term) = pty.term.try_lock() else {
                return;
            };
            find_next(&term, &mut query, after).map(|matched| (*matched.start(), *matched.end()))
        };
        if let Some((start, _)) = new_match {
            if let Some(state) = pty.scroll_state() {
                if let Some(target) = display_offset_for_match(
                    start,
                    state.display_offset,
                    state.visible_rows,
                    state.history_size,
                ) {
                    pty.scroll_to_display_offset(target);
                }
            }
        }
        if let Some(search) = self.search.as_mut() {
            search.found = Some(new_match.is_some());
            search.current = new_match;
        }
        pty.mark_render_dirty();
    }

    /// Inyecta un prompt/feedback en el terminal del agente: sanitiza bytes de
    /// escape, lo manda como bracketed paste (si el TUI lo activó) y lo
    /// submits con Enter. Si el panel estaba detached (recién spawneado),
    /// difiere la inyección hasta que el TUI renderice algo, para no mandar el
    /// texto a un agente que todavía arranca.
    pub fn send_prompt(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let was_attached = self.session.is_attached();
        self.session.ensure_attached();
        if was_attached {
            // Ya está corriendo: inyectá directo.
            let mode = self.session.input_mode();
            let bytes = agent_prompt_bytes(text, &mode);
            let _ = self.with_pty(|pty| pty.write_all(&bytes));
        } else {
            // Recién spawneado: diferí hasta que renderice.
            self.session.queue_prompt(text);
        }
    }

    /// Badge `⎇ branch` con un punto cuando el repo está sucio. Se dibuja sólo
    /// si `branch_badge_rect` confirma que hay lugar libre a la derecha del
    /// título.
    #[allow(clippy::too_many_arguments)]
    fn draw_branch_badge(
        &self,
        painter: &egui::Painter,
        title_rect: Rect,
        title_right: f32,
        branch: &str,
        dirty: bool,
        chrome_zoom: f32,
    ) {
        let font = FontId::proportional((11.0 * chrome_zoom).clamp(7.0, 11.0));
        let label = format!("⎇ {branch}");
        let galley = painter.layout_no_wrap(label, font, DIM_FG);
        // El punto de "sucio" va después del texto, con su propio aire.
        let dot_radius = (2.5 * chrome_zoom).clamp(1.5, 2.5);
        let dot_space = if dirty { dot_radius * 2.0 + 5.0 } else { 0.0 };
        let padding = vec2(8.0, 3.0);
        let size = vec2(
            galley.size().x + dot_space + padding.x * 2.0,
            galley.size().y + padding.y * 2.0,
        );
        let Some(rect) = branch_badge_rect(title_rect, title_right, size) else {
            return;
        };
        painter.rect_filled(rect, 5.0, BRANCH_BADGE_BG);
        let text_pos = pos2(
            rect.left() + padding.x,
            rect.center().y - galley.size().y * 0.5,
        );
        painter.galley(text_pos, galley, DIM_FG);
        if dirty {
            painter.circle_filled(
                pos2(rect.right() - padding.x - dot_radius, rect.center().y),
                dot_radius,
                BRANCH_DIRTY_DOT,
            );
        }
    }

    /// Reinyecta el historial guardado de una corrida anterior en el grid.
    /// Devuelve `false` si el panel todavía no tiene terminal (sigue detached),
    /// para que quien llama pueda reintentarlo cuando se attachee.
    pub fn restore_history(&mut self, text: &str) -> bool {
        let bytes = crate::state::scrollback_store::replay_bytes(text);
        self.with_pty(|pty| pty.replay_history(&bytes)).is_some()
    }

    /// Historial completo (scrollback + pantalla) como texto plano. `None` si
    /// la sesión está detached y no tiene un terminal vivo que leer.
    pub fn scrollback_text(&self) -> Option<String> {
        self.with_pty(|pty| pty.with_term(|term| crate::terminal::export::scrollback_to_text(term)))
            .flatten()
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

    fn report_mouse_click(
        &mut self,
        button: u8,
        release: bool,
        modifiers: &Modifiers,
        pointer: Pos2,
        viewport: &Viewport,
        canvas_rect: Rect,
    ) {
        let Some(cell) = self.mouse_cell_from_pointer(pointer, viewport, canvas_rect) else {
            return;
        };
        self.last_mouse_cell = Some((cell.column, cell.line));
        let bytes = mouse_click_sgr_sequence(button, release, modifiers, cell.column, cell.line);
        let _ = self.with_pty(|pty| pty.write_all(&bytes));
    }

    fn report_mouse_motion_if_moved(
        &mut self,
        mode: &InputMode,
        modifiers: &Modifiers,
        primary_down: bool,
        pointer: Pos2,
        viewport: &Viewport,
        canvas_rect: Rect,
    ) {
        let drag_ok = primary_down && mode.mouse_drag;
        let hover_ok = !primary_down && mode.mouse_motion;
        if !drag_ok && !hover_ok {
            return;
        }
        let Some(cell) = self.mouse_cell_from_pointer(pointer, viewport, canvas_rect) else {
            return;
        };
        let cell_key = (cell.column, cell.line);
        if self.last_mouse_cell == Some(cell_key) {
            return;
        }
        self.last_mouse_cell = Some(cell_key);
        // SGR: botón 0 = arrastre con izquierdo; 3 = movimiento sin botón.
        let button = if primary_down { 0 } else { 3 };
        let bytes = mouse_motion_sgr_sequence(button, modifiers, cell.column, cell.line);
        let _ = self.with_pty(|pty| pty.write_all(&bytes));
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

        // Resize from corners/edges is intentionally disabled: panels are
        // always auto-tiled into one of the fixed slots, never freely resized.
        let _ = ResizeHandle::ALL;

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
            let input_mode = self.session.input_mode();
            let pointer_in_body = ui
                .ctx()
                .input(|input| input.pointer.latest_pos())
                .filter(|pos| body_hit_rect.contains(*pos));
            // Si el TUI pidió reporte de mouse (htop, vim, codex, apps
            // ratatui) los clicks/drags se reenvían como secuencias SGR 1006
            // en vez de iniciar selección local.
            let mouse_reporting = input_mode.mouse_mode && pointer_in_body.is_some();
            if mouse_reporting {
                self.session.ensure_attached();
                if let Some(pointer) = pointer_in_body {
                    let modifiers = ui.ctx().input(|input| input.modifiers);
                    let button_events: Vec<(u8, bool, bool)> = ui.ctx().input(|input| {
                        [
                            (PointerButton::Primary, 0_u8),
                            (PointerButton::Middle, 1),
                            (PointerButton::Secondary, 2),
                        ]
                        .iter()
                        .map(|(button, code)| {
                            (
                                *code,
                                input.pointer.button_pressed(*button),
                                input.pointer.button_released(*button),
                            )
                        })
                        .collect()
                    });
                    for (code, pressed, released) in button_events {
                        if pressed {
                            interaction.clicked = true;
                            self.report_mouse_click(
                                code,
                                false,
                                &modifiers,
                                pointer,
                                viewport,
                                canvas_rect,
                            );
                        }
                        if released {
                            self.report_mouse_click(
                                code,
                                true,
                                &modifiers,
                                pointer,
                                viewport,
                                canvas_rect,
                            );
                        }
                    }
                    let primary_down = ui.ctx().input(|input| input.pointer.primary_down());
                    if !primary_down || input_mode.mouse_drag || input_mode.mouse_motion {
                        self.report_mouse_motion_if_moved(
                            &input_mode,
                            &modifiers,
                            primary_down,
                            pointer,
                            viewport,
                            canvas_rect,
                        );
                    }
                }
            } else {
                self.last_mouse_cell = None;
                if body_response.clicked() {
                    // Cmd+click (macOS) / Ctrl+click abre la URL bajo el cursor.
                    let url_mods = ui.ctx().input(|input| input.modifiers);
                    let url_click = if cfg!(target_os = "macos") {
                        url_mods.command
                    } else {
                        url_mods.ctrl
                    };
                    let mut opened_url = false;
                    if url_click {
                        if let Some(pointer) = body_response.interact_pointer_pos() {
                            opened_url = self.try_open_url_at_pointer(
                                pointer,
                                content_rect,
                                canvas_rect,
                                zoom,
                            );
                        }
                    }
                    interaction.clicked = true;
                    if !opened_url {
                        self.session.clear_selection();
                    }
                }
                if body_response.double_clicked() {
                    interaction.clicked = true;
                    if let Some(pointer) = body_response.interact_pointer_pos() {
                        self.begin_selection(
                            pointer,
                            content_rect,
                            canvas_rect,
                            zoom,
                            SelectionType::Semantic,
                        );
                        self.copy_selection_if_enabled();
                    }
                }
                if body_response.triple_clicked() {
                    interaction.clicked = true;
                    if let Some(pointer) = body_response.interact_pointer_pos() {
                        self.begin_selection(
                            pointer,
                            content_rect,
                            canvas_rect,
                            zoom,
                            SelectionType::Lines,
                        );
                        self.copy_selection_if_enabled();
                    }
                }
                if body_response.drag_started() {
                    interaction.clicked = true;
                    if let Some(pointer) = body_response.interact_pointer_pos() {
                        self.begin_selection(
                            pointer,
                            content_rect,
                            canvas_rect,
                            zoom,
                            SelectionType::Simple,
                        );
                    }
                }
                if body_response.dragged() {
                    if let Some(pointer) = body_response.interact_pointer_pos() {
                        self.update_selection(pointer, content_rect, canvas_rect, zoom);
                    }
                }
                if body_response.drag_stopped() && !body_response.clicked() {
                    self.copy_selection_if_enabled();
                }
            }
            interaction.hovered_terminal = body_response.hovered();
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
            Color32::from_rgb(255, 255, 255)
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
            let title_galley_rect = chrome_painter.text(
                title_rect.left_center() + vec2(title_offset, 0.0),
                Align2::LEFT_CENTER,
                title_text,
                FontId::proportional((15.5 * chrome_zoom).clamp(7.0, 15.5)),
                if self.is_alive() { FG } else { DIM_FG },
            );
            // Badge de branch a la derecha, sólo en LOD Full y sólo si entra
            // sin pisar el título (branch_badge_rect decide). El resto de los
            // badges se sacó antes por ruido visual; este se limita a mostrar
            // el contexto git, que es lo que importa en multi-agente.
            if matches!(lod, PanelLod::Full) {
                if let Some(overlay) = overlay {
                    if let Some(branch) = overlay.branch.as_deref().filter(|b| !b.is_empty()) {
                        self.draw_branch_badge(
                            &chrome_painter,
                            title_rect,
                            title_galley_rect.right(),
                            branch,
                            overlay.dirty,
                            chrome_zoom,
                        );
                    }
                }
            }
        }
        let content_clip_rect = content_rect.intersect(canvas_rect);
        let content_painter = painter.with_clip_rect(content_clip_rect);
        let content_rounding = roundings.body;
        let now = ui.ctx().input(|i| i.time);
        let title_snapshot: &str = &self.title;
        let shell_title_snapshot: &str = &self.shell_title;
        let fallback_preview_title: &str = if let Some(overlay) = overlay {
            &overlay.preview_label
        } else if is_generic_terminal_name(&self.title) {
            &self.cwd_label
        } else {
            &self.title
        };
        // Only allocate a new activity_label when an actual scan updates it.
        // Most frames don't scan, so we read self.activity_label by borrow.
        let mut updated_activity_label: Option<Option<String>> = None;
        let mut activity_label_scan_at = None;
        let mut scrollbar_state = self.last_scrollbar_state;
        let render_tier =
            render_tier_for_panel(content_rect, zoom, lod, fast_path_render, self.focused);
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
                #[cfg(feature = "ghostty-vt")]
                let mut rendered_ghostty = false;
                #[cfg(feature = "ghostty-vt")]
                if matches!(render_tier, RenderTier::Full | RenderTier::ReducedLive)
                    && pty.backend_kind() == TerminalBackendKind::Ghostty
                {
                    if let Some(snapshot) = pty.ghostty_snapshot() {
                        scrollbar_state = Some(snapshot.scroll_state);
                        if scan_activity {
                            let visible_text = snapshot.rows.join("\n");
                            scanned_activity_label = Some(infer_activity_label(
                                title_snapshot,
                                shell_title_snapshot,
                                &visible_text,
                            ));
                        }
                        interaction.cache_hit = render_ghostty_text_snapshot(
                            &content_painter,
                            content_rect,
                            &snapshot,
                            self.focused,
                            now,
                            zoom,
                            content_rounding,
                            Some(&mut self.ghostty_render_cache),
                        );
                        rendered_ghostty = true;
                    }
                }
                #[cfg(not(feature = "ghostty-vt"))]
                let rendered_ghostty = false;

                if !rendered_ghostty
                    && matches!(render_tier, RenderTier::Full | RenderTier::ReducedLive)
                {
                    if let Ok(mut term) = pty.term.try_lock() {
                        term.is_focused = self.focused;
                        let display_offset_now = term.grid().display_offset();
                        scrollbar_state = Some(TerminalScrollState {
                            display_offset: term.grid().display_offset(),
                            visible_rows: term.screen_lines(),
                            history_size: term.grid().history_size(),
                        });
                        if scan_activity {
                            scanned_activity_label = Some(infer_activity_label_from_term(
                                title_snapshot,
                                shell_title_snapshot,
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
                        if let Some(search) = self.search.as_ref() {
                            if let Some((start, end)) = search.current {
                                draw_search_highlight(
                                    &content_painter,
                                    content_rect,
                                    zoom,
                                    display_offset_now,
                                    start,
                                    end,
                                );
                            }
                        }
                    } else {
                        let preview_label = overlay
                            .map(|overlay| overlay.preview_label.clone())
                            .unwrap_or_else(|| {
                                preview_label_text(
                                    self.activity_label.as_deref(),
                                    fallback_preview_title,
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
                } else if !rendered_ghostty && !matches!(render_tier, RenderTier::Hidden) {
                    let preview_label = overlay
                        .map(|overlay| overlay.preview_label.clone())
                        .unwrap_or_else(|| {
                            preview_label_text(
                                self.activity_label.as_deref(),
                                fallback_preview_title,
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
                if let Some(detected_label) = scanned_activity_label.take() {
                    updated_activity_label = Some(detected_label.or_else(|| {
                        infer_activity_label(title_snapshot, shell_title_snapshot, "")
                    }));
                    activity_label_scan_at = Some(now);
                }
            } else {
                let preview_label = overlay
                    .map(|overlay| overlay.preview_label.clone())
                    .unwrap_or_else(|| {
                        preview_label_text(self.activity_label.as_deref(), fallback_preview_title)
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
                FontId::monospace(base_font_size()),
                Color32::from_rgb(244, 244, 244),
            );
        }
        if let Some(new) = updated_activity_label {
            self.activity_label = new;
        }
        if let Some(scanned_at) = activity_label_scan_at {
            self.last_activity_scan_at = scanned_at;
        }
        self.last_scrollbar_state = scrollbar_state;

        chrome_painter.rect_stroke(stroke_rect, panel_rounding, Stroke::new(0.75, border_color));
        if matches!(lod, PanelLod::Full) {
            chrome_painter.line_segment(
                [
                    pos2(screen_rect.left() + separator_inset, separator_y),
                    pos2(screen_rect.right() - separator_inset, separator_y),
                ],
                Stroke::new(1.0, border_color),
            );
        }

        // Resize grip removido: las terminales solo viven en los slots fijos
        // del auto-tile, no se redimensionan manualmente desde la esquina.
        let _ = lod;

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

    /// Copia la selección al portapapeles si `copy_on_select` está activo.
    fn copy_selection_if_enabled(&self) {
        if !crate::config::runtime_config().copy_on_select {
            return;
        }
        if let Some(text) = self.selected_text().filter(|text| !text.is_empty()) {
            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                let _ = clipboard.set_text(text);
            }
        }
    }

    /// Si hay una URL en la celda bajo el puntero, la abre con la app default
    /// del SO. Devuelve true si abrió algo.
    fn try_open_url_at_pointer(
        &self,
        pointer: Pos2,
        content_rect: Rect,
        canvas_rect: Rect,
        zoom: f32,
    ) -> bool {
        let Some(handle) = self.session_handle() else {
            return false;
        };
        let Ok(pty) = handle.lock() else {
            return false;
        };
        let Ok(term) = pty.term.try_lock() else {
            return false;
        };
        let visible_rows = term.screen_lines() as u16;
        let visible_cols = term.columns() as u16;
        let cell = terminal_cell_from_pointer(
            content_rect.intersect(canvas_rect),
            pointer,
            zoom,
            visible_rows,
            visible_cols,
        );
        let url = cell.and_then(|cell| url_at_cell(&term, cell.line, cell.column));
        drop(term);
        let Some(url) = url else {
            return false;
        };
        let _ = crate::utils::platform::open_path_external(std::path::Path::new(&url));
        true
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
        let _ = screen_width;
        if let Some(custom_title) = &self.custom_title {
            return custom_title.clone();
        }
        if !self.title.is_empty() && self.title != "Terminal" {
            return self.title.clone();
        }
        self.shell_label.clone()
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

const SEARCH_HIGHLIGHT: Color32 = Color32::from_rgb(212, 160, 60);

/// Resalta el match de búsqueda visible: un rect por línea del rango que
/// entra en el viewport (las líneas fuera de pantalla se saltean).
fn draw_search_highlight(
    painter: &egui::Painter,
    content_rect: Rect,
    zoom: f32,
    display_offset: usize,
    start: Point,
    end: Point,
) {
    let metrics = grid_metrics(zoom);
    let (pad_x, pad_y) = grid_padding(zoom);
    let cols = {
        let last_col = end.column.0.max(start.column.0);
        last_col + 1
    };
    let mut line = start.line;
    let mut drawn_lines = 0usize;
    loop {
        let col_start = if line == start.line {
            start.column.0
        } else {
            0
        };
        let col_end = if line == end.line {
            end.column.0
        } else {
            cols.saturating_sub(1)
        };
        if let Some(viewport_point) =
            point_to_viewport(display_offset, Point::new(line, Column(col_start)))
        {
            let x0 = content_rect.left() + pad_x + col_start as f32 * metrics.char_width;
            let x1 = content_rect.left() + pad_x + (col_end as f32 + 1.0) * metrics.char_width;
            let y = content_rect.top() + pad_y + viewport_point.line as f32 * metrics.line_height;
            let rect =
                Rect::from_min_size(pos2(x0, y), vec2((x1 - x0).max(1.0), metrics.line_height));
            painter.rect_filled(
                rect,
                2.0,
                Color32::from_rgba_premultiplied(
                    SEARCH_HIGHLIGHT.r(),
                    SEARCH_HIGHLIGHT.g(),
                    SEARCH_HIGHLIGHT.b(),
                    70,
                ),
            );
            painter.rect_stroke(rect, 2.0, Stroke::new(1.0, SEARCH_HIGHLIGHT));
        }
        drawn_lines += 1;
        if line == end.line || drawn_lines >= MAX_HIGHLIGHT_LINES {
            break;
        }
        line = Line(line.0 + 1);
    }
}
