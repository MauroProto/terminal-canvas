use std::sync::{Arc, Mutex};

use egui::{Color32, Pos2, Rect, Vec2};
use uuid::Uuid;

use crate::collab::PanelShareScope;
use crate::collab::{SharedPanelSnapshot, TerminalInputEvent};
use crate::orchestration::{AgentProvider, PanelOverlay, PanelRuntimeObservation};
use crate::state::{PanelPlacement, PanelState, SnapSlot};
use crate::terminal::panel::{PanelHitArea, PanelInteraction, ResizeHandle, TerminalPanel};

pub enum WorkspacePanel {
    Terminal(TerminalPanel),
}

pub type CanvasPanel = WorkspacePanel;

impl WorkspacePanel {
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

    pub fn all_runtime_session_ids(&self) -> Vec<Uuid> {
        match self {
            Self::Terminal(panel) => panel.all_runtime_session_ids(),
        }
    }

    pub fn focused_runtime_session_id(&self) -> Option<Uuid> {
        match self {
            Self::Terminal(panel) => panel.focused_runtime_session_id(),
        }
    }

    pub fn current_cwd(&self) -> Option<String> {
        match self {
            Self::Terminal(panel) => panel.current_cwd(),
        }
    }

    pub fn focused_leaf_id(&self) -> Uuid {
        match self {
            Self::Terminal(panel) => panel.focused_leaf_id(),
        }
    }

    pub fn live_leaf_cwds(&self) -> Vec<std::path::PathBuf> {
        match self {
            Self::Terminal(panel) => panel
                .leaf_ids()
                .into_iter()
                .filter_map(|id| {
                    let handle = panel.leaf_session_handle(id)?;
                    let session = handle.lock().ok()?;
                    session.current_cwd().map(std::path::PathBuf::from)
                })
                .collect(),
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        match self {
            Self::Terminal(panel) => panel.selected_text(),
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
            Self::Terminal(panel) => panel.focused(),
        }
    }

    pub fn minimized(&self) -> bool {
        match self {
            Self::Terminal(panel) => panel.minimized(),
        }
    }

    pub fn set_share_scope(&mut self, scope: PanelShareScope) {
        match self {
            Self::Terminal(panel) => panel.set_share_scope(scope),
        }
    }

    pub fn share_scope(&self) -> PanelShareScope {
        match self {
            Self::Terminal(panel) => panel.share_scope(),
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        match self {
            Self::Terminal(panel) => panel.set_focused(focused),
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

    pub fn set_drag_virtual_pos(&mut self, pos: Option<Pos2>) {
        match self {
            Self::Terminal(panel) => panel.drag_virtual_pos = pos,
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

    pub fn placement(&self) -> &PanelPlacement {
        match self {
            Self::Terminal(panel) => panel.placement(),
        }
    }

    pub fn set_placement(&mut self, placement: PanelPlacement) {
        match self {
            Self::Terminal(panel) => panel.set_placement(placement),
        }
    }

    pub fn set_restore_bounds(&mut self, rect: Option<Rect>) {
        match self {
            Self::Terminal(panel) => panel.set_restore_bounds(rect),
        }
    }

    pub fn set_restore_placement(&mut self, placement: Option<PanelPlacement>) {
        match self {
            Self::Terminal(panel) => panel.set_restore_placement(placement),
        }
    }

    pub fn current_or_restore_rect(&self) -> Rect {
        match self {
            Self::Terminal(panel) => panel.current_or_restore_rect(),
        }
    }

    pub fn maximize(&mut self, desktop_rect: Rect) {
        match self {
            Self::Terminal(panel) => panel.maximize(desktop_rect),
        }
    }

    pub fn snap_to(&mut self, slot: SnapSlot, desktop_rect: Rect) {
        match self {
            Self::Terminal(panel) => panel.snap_to(slot, desktop_rect),
        }
    }

    pub fn restore_window_placement(&mut self, desktop_rect: Rect) {
        match self {
            Self::Terminal(panel) => panel.restore_window_placement(desktop_rect),
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

    pub fn search_active(&self) -> bool {
        match self {
            Self::Terminal(panel) => panel.search_active(),
        }
    }

    pub fn search_query(&self) -> &str {
        match self {
            Self::Terminal(panel) => panel.search_query(),
        }
    }

    pub fn search_open(&mut self) {
        match self {
            Self::Terminal(panel) => panel.search_open(),
        }
    }

    pub fn search_close(&mut self) {
        match self {
            Self::Terminal(panel) => panel.search_close(),
        }
    }

    pub fn search_set_query(&mut self, query: String) {
        match self {
            Self::Terminal(panel) => panel.search_set_query(query),
        }
    }

    pub fn search_found(&self) -> Option<bool> {
        match self {
            Self::Terminal(panel) => panel.search_found(),
        }
    }

    pub fn search_find_next(&mut self) {
        match self {
            Self::Terminal(panel) => panel.search_find_next(),
        }
    }

    pub fn send_prompt(&mut self, text: &str) {
        match self {
            Self::Terminal(panel) => panel.send_prompt(text),
        }
    }

    pub fn replace_focused_session(
        &mut self,
        pty_manager: Arc<Mutex<crate::runtime::PtyManager>>,
        cwd: Option<&std::path::Path>,
        workspace_id: Uuid,
        startup_command: String,
        agent_command: String,
    ) -> bool {
        let Self::Terminal(panel) = self;
        panel.replace_focused_session(
            pty_manager,
            cwd,
            workspace_id,
            startup_command,
            agent_command,
        )
    }

    pub fn insert_text(&mut self, text: &str) {
        match self {
            Self::Terminal(panel) => panel.insert_text(text),
        }
    }

    pub fn scrollback_text(&self) -> Option<String> {
        match self {
            Self::Terminal(panel) => panel.scrollback_text(),
        }
    }

    pub fn leaf_scrollbacks(&self) -> Vec<(Option<uuid::Uuid>, String, usize)> {
        match self {
            Self::Terminal(panel) => panel.leaf_scrollbacks(),
        }
    }

    pub fn leaf_ids(&self) -> Vec<uuid::Uuid> {
        match self {
            Self::Terminal(panel) => panel.leaf_ids(),
        }
    }

    pub fn root_leaf_id(&self) -> uuid::Uuid {
        match self {
            Self::Terminal(panel) => panel.root_leaf_id(),
        }
    }

    pub fn restore_leaf_histories(
        &mut self,
        histories: &[(
            Option<uuid::Uuid>,
            String,
            Vec<crate::state::scrollback_log::Frame>,
        )],
    ) -> bool {
        let Self::Terminal(panel) = self;
        panel.restore_leaf_histories(histories)
    }

    pub fn scrollback_ansi(&self) -> Option<String> {
        match self {
            Self::Terminal(panel) => panel.scrollback_ansi(),
        }
    }

    pub fn drain_pending_log(&self) -> Option<Vec<u8>> {
        match self {
            Self::Terminal(panel) => panel.drain_pending_log(),
        }
    }

    pub fn pending_leaf_logs(&self) -> Vec<(uuid::Uuid, Vec<u8>)> {
        match self {
            Self::Terminal(panel) => panel.pending_leaf_logs(),
        }
    }

    pub fn acknowledge_leaf_log(&self, leaf_id: uuid::Uuid, written_bytes: usize) {
        match self {
            Self::Terminal(panel) => panel.acknowledge_leaf_log(leaf_id, written_bytes),
        }
    }

    pub fn unread(&self) -> bool {
        match self {
            Self::Terminal(panel) => panel.unread(),
        }
    }

    pub fn set_agent_session_id(&mut self, session_id: Option<String>) {
        let Self::Terminal(panel) = self;
        panel.set_agent_session_id(session_id);
    }

    pub fn set_agent_session_id_for_leaf(
        &mut self,
        leaf_id: uuid::Uuid,
        session_id: Option<String>,
    ) {
        let Self::Terminal(panel) = self;
        panel.set_agent_session_id_for_leaf(leaf_id, session_id);
    }

    pub fn agent_session_id(&self) -> Option<&str> {
        match self {
            Self::Terminal(panel) => panel.agent_session_id(),
        }
    }

    pub fn agent_command(&self) -> Option<&str> {
        match self {
            Self::Terminal(panel) => panel.agent_command(),
        }
    }

    pub fn linked_issue(&self) -> Option<u64> {
        match self {
            Self::Terminal(panel) => panel.linked_issue(),
        }
    }

    pub fn set_linked_issue(&mut self, issue: Option<u64>) {
        let Self::Terminal(panel) = self;
        panel.set_linked_issue(issue);
    }

    /// Cierra el panel para siempre (lo cerró el usuario): mata la sesión del
    /// daemon si la tiene.
    pub fn close_for_good(&mut self) {
        let Self::Terminal(panel) = self;
        panel.close_for_good();
    }

    pub fn set_unread(&mut self, unread: bool) {
        match self {
            Self::Terminal(panel) => panel.set_unread(unread),
        }
    }

    pub fn leaf_count(&self) -> usize {
        match self {
            Self::Terminal(panel) => panel.leaf_count(),
        }
    }

    pub fn focused_leaf_index(&self) -> usize {
        match self {
            Self::Terminal(panel) => panel.focused_leaf_index(),
        }
    }

    pub fn is_split(&self) -> bool {
        match self {
            Self::Terminal(panel) => panel.is_split(),
        }
    }

    pub fn split_focused(
        &mut self,
        axis: crate::terminal::split_tree::Axis,
        pty_manager: Arc<Mutex<crate::runtime::PtyManager>>,
        cwd: Option<&std::path::Path>,
        workspace_id: Option<Uuid>,
        cols: u16,
        rows: u16,
    ) {
        let Self::Terminal(panel) = self;
        panel.split_focused(axis, pty_manager, cwd, workspace_id, cols, rows);
    }

    pub fn close_focused_leaf(&mut self) -> bool {
        match self {
            Self::Terminal(panel) => panel.close_focused_leaf(),
        }
    }

    pub fn focus_next_leaf(&mut self) {
        let Self::Terminal(panel) = self;
        panel.focus_next_leaf();
    }

    pub fn restore_session(
        &mut self,
        checkpoint: &str,
        frames: &[crate::state::scrollback_log::Frame],
    ) -> bool {
        match self {
            Self::Terminal(panel) => panel.restore_session(checkpoint, frames),
        }
    }

    pub fn restore_history(&mut self, text: &str) -> bool {
        match self {
            Self::Terminal(panel) => panel.restore_history(text),
        }
    }
}
