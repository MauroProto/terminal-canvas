use egui::{Color32, Pos2, Rect, Vec2};
use uuid::Uuid;

use crate::collab::{SharedPanelSnapshot, TerminalInputEvent};
use crate::orchestration::{AgentProvider, PanelOverlay, PanelRuntimeObservation};
use crate::state::PanelState;
use crate::terminal::panel::{PanelHitArea, PanelInteraction, ResizeHandle, TerminalPanel};

pub enum CanvasPanel {
    Terminal(TerminalPanel),
}

impl CanvasPanel {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Terminal(panel) => panel.id,
        }
    }

    pub fn runtime_session_id(&self) -> Option<Uuid> {
        match self {
            Self::Terminal(panel) => panel.runtime_session_id(),
        }
    }

    pub fn runtime_session_attached(&self) -> bool {
        match self {
            Self::Terminal(panel) => panel.runtime_session_attached(),
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Self::Terminal(panel) => &panel.title,
        }
    }

    pub fn set_title(&mut self, title: String) {
        match self {
            Self::Terminal(panel) => panel.rename_title(title),
        }
    }

    pub fn position(&self) -> Pos2 {
        match self {
            Self::Terminal(panel) => panel.position,
        }
    }

    pub fn set_position(&mut self, pos: Pos2) {
        match self {
            Self::Terminal(panel) => panel.position = pos,
        }
    }

    pub fn size(&self) -> Vec2 {
        match self {
            Self::Terminal(panel) => panel.size,
        }
    }

    pub fn rect(&self) -> Rect {
        Rect::from_min_size(self.position(), self.size())
    }

    pub fn color(&self) -> Color32 {
        match self {
            Self::Terminal(panel) => panel.color,
        }
    }

    pub fn provider_hint(&self) -> Option<AgentProvider> {
        match self {
            Self::Terminal(panel) => panel.provider_hint(),
        }
    }

    pub fn z_index(&self) -> u32 {
        match self {
            Self::Terminal(panel) => panel.z_index,
        }
    }

    pub fn set_z_index(&mut self, z: u32) {
        match self {
            Self::Terminal(panel) => panel.z_index = z,
        }
    }

    pub fn focused(&self) -> bool {
        match self {
            Self::Terminal(panel) => panel.focused,
        }
    }

    pub fn minimized(&self) -> bool {
        match self {
            Self::Terminal(panel) => panel.minimized(),
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        match self {
            Self::Terminal(panel) => panel.focused = focused,
        }
    }

    pub fn set_minimized(&mut self, minimized: bool) {
        match self {
            Self::Terminal(panel) => panel.set_minimized(minimized),
        }
    }

    pub fn is_alive(&self) -> bool {
        match self {
            Self::Terminal(panel) => panel.is_alive(),
        }
    }

    pub fn drag_virtual_pos(&self) -> Option<Pos2> {
        match self {
            Self::Terminal(panel) => panel.drag_virtual_pos,
        }
    }

    pub fn set_drag_virtual_pos(&mut self, pos: Option<Pos2>) {
        match self {
            Self::Terminal(panel) => panel.drag_virtual_pos = pos,
        }
    }

    pub fn resize_virtual_rect(&self) -> Option<Rect> {
        match self {
            Self::Terminal(panel) => panel.resize_virtual_rect,
        }
    }

    pub fn set_resize_virtual_rect(&mut self, rect: Option<Rect>) {
        match self {
            Self::Terminal(panel) => panel.resize_virtual_rect = rect,
        }
    }

    pub fn apply_resize(&mut self, rect: Rect) {
        match self {
            Self::Terminal(panel) => panel.apply_resize(rect),
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        viewport: &crate::canvas::viewport::Viewport,
        canvas_rect: Rect,
        fast_path_render: bool,
        overlay: Option<&PanelOverlay>,
    ) -> PanelInteraction {
        match self {
            Self::Terminal(panel) => {
                panel.show(ui, viewport, canvas_rect, fast_path_render, overlay)
            }
        }
    }

    pub fn handle_input(&mut self, ctx: &egui::Context) {
        match self {
            Self::Terminal(panel) => panel.handle_input(ctx),
        }
    }

    pub fn apply_remote_input_events(&mut self, events: &[TerminalInputEvent]) {
        match self {
            Self::Terminal(panel) => panel.apply_remote_input_events(events),
        }
    }

    pub fn handle_scroll(
        &mut self,
        delta: f32,
        pointer: Option<egui::Pos2>,
        viewport: &crate::canvas::viewport::Viewport,
        canvas_rect: Rect,
        ctx: &egui::Context,
    ) {
        match self {
            Self::Terminal(panel) => {
                panel.handle_scroll(delta, pointer, viewport, canvas_rect, ctx)
            }
        }
    }

    pub fn sync_title(&mut self) {
        match self {
            Self::Terminal(panel) => panel.sync_title(),
        }
    }

    pub fn scroll_hit_test(
        &self,
        pos: egui::Pos2,
        viewport: &crate::canvas::viewport::Viewport,
        canvas_rect: Rect,
    ) -> bool {
        match self {
            Self::Terminal(panel) => panel.scroll_hit_test(pos, viewport, canvas_rect),
        }
    }

    pub fn contains_screen_pos(
        &self,
        pos: egui::Pos2,
        viewport: &crate::canvas::viewport::Viewport,
        canvas_rect: Rect,
    ) -> bool {
        match self {
            Self::Terminal(panel) => panel.contains_screen_pos(pos, viewport, canvas_rect),
        }
    }

    pub fn hit_test(
        &self,
        pos: egui::Pos2,
        viewport: &crate::canvas::viewport::Viewport,
        canvas_rect: Rect,
    ) -> Option<PanelHitArea> {
        match self {
            Self::Terminal(panel) => panel.hit_test(pos, viewport, canvas_rect),
        }
    }

    pub fn drag_to(
        &mut self,
        origin: Pos2,
        pointer_delta: Vec2,
        zoom: f32,
        other_panels: &[Rect],
    ) -> Vec<crate::canvas::snap::SnapGuide> {
        match self {
            Self::Terminal(panel) => panel.drag_to(origin, pointer_delta, zoom, other_panels),
        }
    }

    pub fn resize_to(
        &mut self,
        handle: ResizeHandle,
        origin: Rect,
        pointer_delta: Vec2,
        zoom: f32,
        other_panels: &[Rect],
    ) -> Vec<crate::canvas::snap::SnapGuide> {
        match self {
            Self::Terminal(panel) => {
                panel.resize_to(handle, origin, pointer_delta, zoom, other_panels)
            }
        }
    }

    pub fn to_saved(&self) -> PanelState {
        match self {
            Self::Terminal(panel) => panel.to_saved(),
        }
    }

    pub fn orchestration_observation(&self, workspace_id: Uuid) -> PanelRuntimeObservation {
        match self {
            Self::Terminal(panel) => panel.orchestration_observation(workspace_id),
        }
    }

    pub fn shared_snapshot(&self) -> SharedPanelSnapshot {
        match self {
            Self::Terminal(panel) => panel.shared_snapshot(),
        }
    }
}
