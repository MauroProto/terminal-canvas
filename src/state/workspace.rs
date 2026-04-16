use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::{pos2, Color32, Pos2, Rect, Vec2};
use uuid::Uuid;

use crate::canvas::config::{
    normalize_panel_size, DEFAULT_PANEL_HEIGHT, DEFAULT_PANEL_WIDTH, PANEL_GAP,
};
use crate::collab::{SharedPanelSnapshot, TerminalInputEvent};
use crate::orchestration::PanelRuntimeObservation;
use crate::panel::CanvasPanel;
use crate::runtime::{PtyManager, SessionSpec, UiUpdateBatch};
use crate::state::persistence::{LegacyCanvasState, WorkspaceDesktopState};
use crate::state::WorkspaceState;
use crate::state::{PanelPlacement, SnapSlot};
use crate::terminal::panel::TerminalPanel;

const PANEL_COLORS: &[Color32] = &[
    Color32::from_rgb(90, 130, 200),
    Color32::from_rgb(200, 90, 90),
    Color32::from_rgb(90, 180, 90),
    Color32::from_rgb(200, 160, 60),
    Color32::from_rgb(150, 90, 200),
    Color32::from_rgb(200, 120, 160),
    Color32::from_rgb(80, 170, 200),
    Color32::from_rgb(180, 180, 80),
];

pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    pub cwd: Option<PathBuf>,
    folder_path_label: Option<String>,
    pub panels: Vec<CanvasPanel>,
    pub viewport_pan: egui::Vec2,
    pub viewport_zoom: f32,
    pub next_z: u32,
    pub next_color: usize,
    pty_manager: Arc<Mutex<PtyManager>>,
}

#[derive(Debug, Clone, Default)]
pub struct TerminalSpawnRequest {
    pub title: Option<String>,
    pub cwd: Option<PathBuf>,
    pub startup_command: Option<String>,
    pub startup_input: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct SpawnedTerminal {
    pub panel_id: Uuid,
    pub runtime_session_id: Option<Uuid>,
}

impl Workspace {
    pub fn new(name: impl Into<String>, cwd: Option<PathBuf>) -> Self {
        let folder_path_label = cwd.as_deref().map(workspace_path_label);
        let name = cwd
            .as_deref()
            .map(workspace_name_from_path)
            .unwrap_or_else(|| name.into());
        Self {
            id: Uuid::new_v4(),
            name,
            cwd,
            folder_path_label,
            panels: Vec::new(),
            viewport_pan: egui::Vec2::ZERO,
            viewport_zoom: 1.0,
            next_z: 0,
            next_color: 0,
            pty_manager: Arc::new(Mutex::new(PtyManager::new())),
        }
    }

    pub fn from_folder(cwd: PathBuf) -> Self {
        Self::new(workspace_name_from_path(&cwd), Some(cwd))
    }

    pub fn from_saved(saved: WorkspaceState, ctx: &egui::Context) -> Self {
        let WorkspaceState {
            id,
            name,
            cwd,
            panels: saved_panels,
            desktop,
            legacy_canvas,
        } = saved;
        let workspace_id = Uuid::parse_str(&id).unwrap_or_else(|_| Uuid::new_v4());
        let pty_manager = Arc::new(Mutex::new(PtyManager::new()));
        let mut panels = Vec::new();
        for panel in saved_panels {
            panels.push(CanvasPanel::Terminal(TerminalPanel::from_saved(
                panel,
                ctx,
                cwd.as_deref(),
                Arc::clone(&pty_manager),
            )));
        }
        let mut workspace = Self {
            id: workspace_id,
            name: cwd.as_deref().map(workspace_name_from_path).unwrap_or(name),
            cwd: cwd.clone(),
            folder_path_label: cwd.as_deref().map(workspace_path_label),
            panels,
            viewport_pan: egui::vec2(legacy_canvas.viewport_pan[0], legacy_canvas.viewport_pan[1]),
            viewport_zoom: legacy_canvas.viewport_zoom,
            next_z: desktop.next_z,
            next_color: desktop.next_color,
            pty_manager,
        };
        if workspace
            .panels
            .iter()
            .filter(|panel| panel.focused() && !panel.minimized())
            .count()
            != 1
        {
            workspace.focus_topmost_visible_panel();
        }
        workspace
    }

    pub fn to_saved(&self) -> WorkspaceState {
        WorkspaceState {
            id: self.id.to_string(),
            name: self.name.clone(),
            cwd: self.cwd.clone(),
            panels: self.panels.iter().map(CanvasPanel::to_saved).collect(),
            desktop: WorkspaceDesktopState {
                next_z: self.next_z,
                next_color: self.next_color,
            },
            legacy_canvas: LegacyCanvasState {
                viewport_pan: [self.viewport_pan.x, self.viewport_pan.y],
                viewport_zoom: self.viewport_zoom,
            },
        }
    }

    pub fn spawn_terminal(&mut self, ctx: &egui::Context) -> Uuid {
        self.spawn_terminal_with_request(ctx, TerminalSpawnRequest::default())
            .panel_id
    }

    pub fn spawn_terminal_with_request(
        &mut self,
        _ctx: &egui::Context,
        request: TerminalSpawnRequest,
    ) -> SpawnedTerminal {
        let size = normalize_panel_size(egui::vec2(DEFAULT_PANEL_WIDTH, DEFAULT_PANEL_HEIGHT));
        let position = self.find_free_position(size);
        let color = PANEL_COLORS[self.next_color % PANEL_COLORS.len()];
        let mut panel = TerminalPanel::new(position, size, color, self.next_z);
        if let Some(title) = request
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
        {
            panel.rename_title(title);
        }
        panel.focused = true;
        let cwd = request.cwd.as_deref().or(self.cwd.as_deref());
        panel.attach_session_with_spec(
            Arc::clone(&self.pty_manager),
            cwd,
            SessionSpec {
                title: panel.title.clone(),
                cwd: cwd.map(Path::to_path_buf),
                startup_command: request.startup_command.clone(),
                startup_input: request.startup_input.clone(),
            },
        );
        let id = panel.id;
        let runtime_session_id = panel.runtime_session_id();
        self.panels.push(CanvasPanel::Terminal(panel));
        self.next_z += 1;
        self.next_color += 1;
        self.bring_to_front(id);
        SpawnedTerminal {
            panel_id: id,
            runtime_session_id,
        }
    }

    #[cfg(test)]
    pub fn add_restored_terminal(&mut self, panel: TerminalPanel) {
        self.panels.push(CanvasPanel::Terminal(panel));
    }

    pub fn bring_to_front(&mut self, panel_id: Uuid) {
        if self
            .panels
            .iter()
            .find(|panel| panel.id() == panel_id)
            .map(|panel| panel.minimized())
            .unwrap_or(false)
        {
            return;
        }
        self.next_z += 1;
        for panel in &mut self.panels {
            if panel.id() == panel_id {
                panel.set_z_index(self.next_z);
                panel.set_focused(true);
            } else {
                panel.set_focused(false);
            }
        }
    }

    pub fn unfocus_all(&mut self) {
        for panel in &mut self.panels {
            panel.set_focused(false);
        }
    }

    pub fn close_panel(&mut self, panel_id: Uuid) {
        self.panels.retain(|panel| panel.id() != panel_id);
        self.focus_topmost_visible_panel();
    }

    pub fn focused_panel_mut(&mut self) -> Option<&mut CanvasPanel> {
        self.panels
            .iter_mut()
            .find(|panel| panel.focused() && !panel.minimized())
    }

    pub fn focused_panel(&self) -> Option<&CanvasPanel> {
        self.panels
            .iter()
            .find(|panel| panel.focused() && !panel.minimized())
    }

    pub fn panel_rects_except(&self, panel_id: Uuid) -> Vec<Rect> {
        self.panels
            .iter()
            .filter(|panel| panel.id() != panel_id && !panel.minimized())
            .map(CanvasPanel::rect)
            .collect()
    }

    pub fn find_free_position(&self, size: Vec2) -> Pos2 {
        let visible_panels: Vec<_> = self
            .panels
            .iter()
            .filter(|panel| !panel.minimized())
            .collect();
        if visible_panels.is_empty() {
            return pos2(50.0, 50.0);
        }

        let gap = PANEL_GAP;
        let mut x_edges = Vec::new();
        let mut y_edges = Vec::new();

        for panel in &visible_panels {
            let rect = panel.rect();
            x_edges.extend_from_slice(&[
                rect.left(),
                rect.right(),
                rect.left() - size.x - gap,
                rect.right() + gap,
            ]);
            y_edges.extend_from_slice(&[
                rect.top(),
                rect.bottom(),
                rect.top() - size.y - gap,
                rect.bottom() + gap,
            ]);
        }

        let mut best = None;
        let current_bbox = visible_panels
            .iter()
            .map(|panel| panel.rect())
            .reduce(|a, b| a.union(b))
            .unwrap();
        let center = current_bbox.center();

        for &x in &x_edges {
            for &y in &y_edges {
                let pos = pos2(x, y);
                let candidate = Rect::from_min_size(pos, size);
                if self.overlaps_any(candidate, gap) {
                    continue;
                }
                let new_bbox = current_bbox.union(candidate);
                let growth = new_bbox.width() * new_bbox.height()
                    - current_bbox.width() * current_bbox.height();
                let dist = pos.distance(center);
                let score = growth * 2.0 + dist;
                match best {
                    Some((best_score, _)) if score >= best_score => {}
                    _ => best = Some((score, pos)),
                }
            }
        }

        best.map(|(_, pos)| pos)
            .unwrap_or_else(|| pos2(current_bbox.left(), current_bbox.bottom() + gap))
    }

    fn overlaps_any(&self, candidate_rect: Rect, gap: f32) -> bool {
        let effective_gap = (gap - 0.1).max(0.0);
        self.panels
            .iter()
            .filter(|panel| !panel.minimized())
            .any(|panel| {
                panel
                    .rect()
                    .expand(effective_gap)
                    .intersects(candidate_rect)
            })
    }

    pub fn rename_panel(&mut self, panel_id: Uuid, title: String) {
        if let Some(panel) = self.panels.iter_mut().find(|panel| panel.id() == panel_id) {
            panel.set_title(title);
        }
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn folder_path_label(&self) -> Option<&str> {
        self.folder_path_label.as_deref()
    }

    pub fn runtime_session_counts(&self) -> (usize, usize) {
        self.pty_manager
            .lock()
            .ok()
            .map(|manager| {
                (
                    manager.attached_session_count(),
                    manager.detached_session_count(),
                )
            })
            .unwrap_or((0, 0))
    }

    pub fn drain_runtime_updates(&self) -> UiUpdateBatch {
        self.pty_manager
            .lock()
            .ok()
            .map(|mut manager| manager.drain_ui_updates())
            .unwrap_or_default()
    }

    pub fn matches_cwd(&self, path: &Path) -> bool {
        self.cwd
            .as_deref()
            .map(|cwd| paths_match(cwd, path))
            .unwrap_or(false)
    }

    pub fn panel_count(&self) -> usize {
        self.panels.len()
    }

    pub fn minimized_panel_count(&self) -> usize {
        self.panels.iter().filter(|panel| panel.minimized()).count()
    }

    pub fn panel(&self, panel_id: Uuid) -> Option<&CanvasPanel> {
        self.panels.iter().find(|panel| panel.id() == panel_id)
    }

    pub fn panel_pair_mut(
        &mut self,
        first_id: Uuid,
        second_id: Uuid,
    ) -> Option<(&mut CanvasPanel, &mut CanvasPanel)> {
        let first_index = self.panels.iter().position(|panel| panel.id() == first_id)?;
        let second_index = self.panels.iter().position(|panel| panel.id() == second_id)?;
        if first_index == second_index {
            return None;
        }
        if first_index < second_index {
            let (left, right) = self.panels.split_at_mut(second_index);
            Some((&mut left[first_index], &mut right[0]))
        } else {
            let (left, right) = self.panels.split_at_mut(first_index);
            Some((&mut right[0], &mut left[second_index]))
        }
    }

    pub fn toggle_minimize_panel(&mut self, panel_id: Uuid) {
        let Some(index) = self.panels.iter().position(|panel| panel.id() == panel_id) else {
            return;
        };
        if self.panels[index].minimized() {
            self.restore_panel(panel_id);
            return;
        }
        let was_focused = self.panels[index].focused();
        self.panels[index].set_minimized(true);
        if was_focused {
            self.focus_topmost_visible_panel();
        }
    }

    pub fn restore_panel(&mut self, panel_id: Uuid) {
        if let Some(panel) = self.panels.iter_mut().find(|panel| panel.id() == panel_id) {
            panel.set_minimized(false);
            if matches!(panel.placement(), PanelPlacement::Floating) {
                let rect = panel.current_or_restore_rect();
                panel.apply_resize(rect);
            }
        } else {
            return;
        }
        self.bring_to_front(panel_id);
    }

    pub fn maximize_panel(&mut self, panel_id: Uuid, desktop_rect: Rect) {
        let Some(panel) = self.panels.iter_mut().find(|panel| panel.id() == panel_id) else {
            return;
        };
        if matches!(panel.placement(), PanelPlacement::Maximized) {
            panel.restore_window_placement(desktop_rect);
            panel.set_minimized(false);
        } else {
            panel.maximize(desktop_rect);
            panel.set_minimized(false);
        }
        self.bring_to_front(panel_id);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn snap_panel(&mut self, panel_id: Uuid, slot: SnapSlot, desktop_rect: Rect) {
        let Some(panel) = self.panels.iter_mut().find(|panel| panel.id() == panel_id) else {
            return;
        };
        panel.set_minimized(false);
        panel.snap_to(slot, desktop_rect);
        self.bring_to_front(panel_id);
    }

    pub fn restore_panel_with_desktop(&mut self, panel_id: Uuid, desktop_rect: Rect) {
        let Some(panel) = self.panels.iter_mut().find(|panel| panel.id() == panel_id) else {
            return;
        };
        panel.set_minimized(false);
        panel.restore_window_placement(desktop_rect);
        self.bring_to_front(panel_id);
    }

    pub fn orchestration_observations(&self) -> Vec<PanelRuntimeObservation> {
        self.panels
            .iter()
            .map(|panel| panel.orchestration_observation(self.id))
            .collect()
    }

    pub fn apply_remote_input(&mut self, panel_id: Uuid, events: &[TerminalInputEvent]) -> bool {
        let Some(panel) = self.panels.iter_mut().find(|panel| panel.id() == panel_id) else {
            return false;
        };
        panel.apply_remote_input_events(events);
        true
    }

    pub fn shared_panel_snapshots(&self) -> Vec<SharedPanelSnapshot> {
        self.panels
            .iter()
            .map(CanvasPanel::shared_snapshot)
            .collect()
    }

    fn focus_topmost_visible_panel(&mut self) {
        let target = self
            .panels
            .iter()
            .enumerate()
            .filter(|(_, panel)| !panel.minimized())
            .max_by_key(|(_, panel)| panel.z_index())
            .map(|(index, _)| index);
        for (index, panel) in self.panels.iter_mut().enumerate() {
            panel.set_focused(Some(index) == target);
        }
    }
}

fn normalize_workspace_path(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

fn workspace_name_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn workspace_path_label(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn paths_match(left: &Path, right: &Path) -> bool {
    normalize_workspace_path(left.to_path_buf()) == normalize_workspace_path(right.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use egui::{pos2, vec2, Rect};
    use uuid::Uuid;

    use super::Workspace;
    use crate::canvas::config::PANEL_GAP;
    use crate::collab::PanelShareScope;
    use crate::state::panel_state::{PanelPlacement, PanelState, SavedPanelBounds};
    use crate::state::persistence::{LegacyCanvasState, WorkspaceDesktopState};
    use crate::state::SnapSlot;
    use crate::state::WorkspaceState;
    use crate::terminal::panel::TerminalPanel;

    #[test]
    fn gap_filling_returns_non_overlapping_candidate() {
        let mut workspace = Workspace::new("Default", None);
        workspace.add_restored_terminal(TerminalPanel::new(
            pos2(50.0, 50.0),
            vec2(1904.0, 720.0),
            egui::Color32::LIGHT_BLUE,
            0,
        ));
        workspace.add_restored_terminal(TerminalPanel::new(
            pos2(50.0, 800.0),
            vec2(1904.0, 720.0),
            egui::Color32::LIGHT_RED,
            1,
        ));

        let pos = workspace.find_free_position(vec2(400.0, 200.0));
        let candidate = egui::Rect::from_min_size(pos, vec2(400.0, 200.0));
        let effective_gap = (PANEL_GAP - 0.1).max(0.0);
        assert!(!workspace
            .panels
            .iter()
            .any(|panel| panel.rect().expand(effective_gap).intersects(candidate)));
    }

    #[test]
    fn folder_workspace_uses_selected_folder_name_and_cwd() {
        let path = unique_temp_dir("workspace-folder-name");
        let workspace = Workspace::from_folder(path.clone());

        assert_eq!(workspace.name, "workspace-folder-name");
        assert_eq!(workspace.cwd(), Some(path.as_path()));
        assert_eq!(
            workspace.folder_path_label(),
            Some(path.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn folder_workspace_matches_same_path() {
        let path = unique_temp_dir("workspace-folder-match");
        let workspace = Workspace::from_folder(path.clone());

        assert!(workspace.matches_cwd(&path));
    }

    #[test]
    fn folder_workspace_matches_equivalent_path_representation() {
        let path = unique_temp_dir("workspace-folder-alias");
        let alias = path
            .parent()
            .unwrap()
            .join(".")
            .join(path.file_name().unwrap());
        let workspace = Workspace::from_folder(path);

        assert!(workspace.matches_cwd(&alias));
    }

    #[test]
    fn minimizing_a_panel_hides_it_until_restored() {
        let mut workspace = Workspace::new("Default", None);
        let first = TerminalPanel::new(pos2(0.0, 0.0), vec2(300.0, 200.0), egui::Color32::WHITE, 0);
        let second = TerminalPanel::new(
            pos2(40.0, 40.0),
            vec2(300.0, 200.0),
            egui::Color32::LIGHT_BLUE,
            1,
        );
        let first_id = first.id;
        let second_id = second.id;
        workspace.add_restored_terminal(first);
        workspace.add_restored_terminal(second);
        workspace.bring_to_front(second_id);

        workspace.toggle_minimize_panel(second_id);

        assert!(workspace.panel(second_id).unwrap().minimized());
        assert_eq!(
            workspace.focused_panel().map(|panel| panel.id()),
            Some(first_id)
        );

        workspace.restore_panel(second_id);

        assert!(!workspace.panel(second_id).unwrap().minimized());
        assert_eq!(
            workspace.focused_panel().map(|panel| panel.id()),
            Some(second_id)
        );
    }

    #[test]
    fn restoring_workspace_keeps_terminal_sessions_detached_until_focus() {
        let ctx = egui::Context::default();
        let state = WorkspaceState {
            id: Uuid::new_v4().to_string(),
            name: "Restored".to_owned(),
            cwd: Some(PathBuf::from("/tmp/restored")),
            panels: vec![PanelState {
                id: Uuid::new_v4().to_string(),
                title: "Terminal".to_owned(),
                custom_title: None,
                position: [20.0, 30.0],
                size: [420.0, 260.0],
                color: [90, 130, 200],
                z_index: 1,
                focused: true,
                minimized: false,
                placement: PanelPlacement::Floating,
                restore_placement: None,
                restore_bounds: Some(SavedPanelBounds::new([20.0, 30.0], [420.0, 260.0])),
                share_scope: PanelShareScope::VisibleOnly,
            }],
            desktop: WorkspaceDesktopState {
                next_z: 2,
                next_color: 1,
            },
            legacy_canvas: LegacyCanvasState {
                viewport_pan: [0.0, 0.0],
                viewport_zoom: 1.0,
            },
        };
        let mut workspace = Workspace::from_saved(state, &ctx);

        assert_eq!(workspace.runtime_session_counts(), (0, 1));

        workspace
            .focused_panel_mut()
            .expect("focused panel")
            .handle_input(&ctx);

        assert_eq!(workspace.runtime_session_counts(), (1, 0));
    }

    #[test]
    fn shared_snapshot_does_not_attach_detached_sessions() {
        let ctx = egui::Context::default();
        let state = WorkspaceState {
            id: Uuid::new_v4().to_string(),
            name: "Shared".to_owned(),
            cwd: Some(PathBuf::from("/tmp/shared")),
            panels: vec![PanelState {
                id: Uuid::new_v4().to_string(),
                title: "Terminal".to_owned(),
                custom_title: None,
                position: [20.0, 30.0],
                size: [420.0, 260.0],
                color: [90, 130, 200],
                z_index: 1,
                focused: false,
                minimized: false,
                placement: PanelPlacement::Floating,
                restore_placement: None,
                restore_bounds: Some(SavedPanelBounds::new([20.0, 30.0], [420.0, 260.0])),
                share_scope: PanelShareScope::VisibleOnly,
            }],
            desktop: WorkspaceDesktopState {
                next_z: 2,
                next_color: 1,
            },
            legacy_canvas: LegacyCanvasState {
                viewport_pan: [0.0, 0.0],
                viewport_zoom: 1.0,
            },
        };
        let workspace = Workspace::from_saved(state, &ctx);

        let before = workspace.runtime_session_counts();
        let snapshots = workspace.shared_panel_snapshots();
        let after = workspace.runtime_session_counts();

        assert_eq!(before, (0, 1));
        assert_eq!(after, before);
        assert_eq!(snapshots.len(), 1);
        assert!(snapshots[0].visible_text.is_empty());
        assert!(snapshots[0].history_text.is_empty());
    }

    #[test]
    fn restored_workspace_budget_keeps_twenty_sessions_detached_until_needed() {
        let ctx = egui::Context::default();
        let panels = (0..20)
            .map(|index| PanelState {
                id: Uuid::new_v4().to_string(),
                title: format!("Terminal {index}"),
                custom_title: None,
                position: [20.0 + index as f32 * 8.0, 30.0 + index as f32 * 6.0],
                size: [420.0, 260.0],
                color: [90, 130, 200],
                z_index: index as u32,
                focused: index == 19,
                minimized: false,
                placement: PanelPlacement::Floating,
                restore_placement: None,
                restore_bounds: Some(SavedPanelBounds::new(
                    [20.0 + index as f32 * 8.0, 30.0 + index as f32 * 6.0],
                    [420.0, 260.0],
                )),
                share_scope: PanelShareScope::VisibleOnly,
            })
            .collect();
        let state = WorkspaceState {
            id: Uuid::new_v4().to_string(),
            name: "Budget".to_owned(),
            cwd: Some(PathBuf::from("/tmp/budget")),
            panels,
            desktop: WorkspaceDesktopState {
                next_z: 21,
                next_color: 4,
            },
            legacy_canvas: LegacyCanvasState {
                viewport_pan: [0.0, 0.0],
                viewport_zoom: 1.0,
            },
        };
        let mut workspace = Workspace::from_saved(state, &ctx);

        assert_eq!(workspace.runtime_session_counts(), (0, 20));

        workspace
            .focused_panel_mut()
            .expect("focused panel")
            .handle_input(&ctx);

        assert_eq!(workspace.runtime_session_counts(), (1, 19));
    }

    #[test]
    fn maximize_toggle_restores_previous_floating_bounds() {
        let mut workspace = Workspace::new("Desktop", None);
        let mut panel = TerminalPanel::new(
            pos2(48.0, 64.0),
            vec2(480.0, 320.0),
            egui::Color32::WHITE,
            0,
        );
        let panel_id = panel.id;
        panel.focused = true;
        workspace.add_restored_terminal(panel);
        let desktop = Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 720.0));

        workspace.maximize_panel(panel_id, desktop);

        let panel = workspace.panel(panel_id).expect("panel exists");
        assert!(matches!(panel.placement(), PanelPlacement::Maximized));
        assert_eq!(panel.rect(), desktop);

        workspace.maximize_panel(panel_id, desktop);

        let panel = workspace.panel(panel_id).expect("panel exists");
        assert!(matches!(panel.placement(), PanelPlacement::Floating));
        assert_eq!(
            panel.rect(),
            Rect::from_min_size(pos2(48.0, 64.0), vec2(480.0, 320.0))
        );
    }

    #[test]
    fn maximize_toggle_restores_previous_snapped_slot() {
        let mut workspace = Workspace::new("Desktop", None);
        let panel = TerminalPanel::new(
            pos2(48.0, 64.0),
            vec2(480.0, 320.0),
            egui::Color32::WHITE,
            0,
        );
        let panel_id = panel.id;
        workspace.add_restored_terminal(panel);
        let desktop = Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 720.0));

        workspace.snap_panel(panel_id, SnapSlot::LeftHalf, desktop);
        workspace.maximize_panel(panel_id, desktop);

        let panel = workspace.panel(panel_id).expect("panel exists");
        assert!(matches!(panel.placement(), PanelPlacement::Maximized));
        assert_eq!(panel.rect(), desktop);

        workspace.maximize_panel(panel_id, desktop);

        let panel = workspace.panel(panel_id).expect("panel exists");
        assert!(matches!(
            panel.placement(),
            PanelPlacement::Snapped(SnapSlot::LeftHalf)
        ));
        assert_eq!(
            panel.rect(),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(640.0, 720.0))
        );
    }

    #[test]
    fn minimize_and_restore_with_desktop_keeps_snapped_slot() {
        let mut workspace = Workspace::new("Desktop", None);
        let panel = TerminalPanel::new(
            pos2(48.0, 64.0),
            vec2(480.0, 320.0),
            egui::Color32::WHITE,
            0,
        );
        let panel_id = panel.id;
        workspace.add_restored_terminal(panel);
        let desktop = Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 720.0));

        workspace.snap_panel(panel_id, SnapSlot::RightHalf, desktop);
        workspace.toggle_minimize_panel(panel_id);
        workspace.restore_panel_with_desktop(panel_id, desktop);

        let panel = workspace.panel(panel_id).expect("panel exists");
        assert!(!panel.minimized());
        assert!(matches!(
            panel.placement(),
            PanelPlacement::Snapped(SnapSlot::RightHalf)
        ));
        assert_eq!(
            panel.rect(),
            Rect::from_min_max(pos2(640.0, 0.0), pos2(1280.0, 720.0))
        );
    }

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir()
            .join(format!("workspace-test-{}", Uuid::new_v4()))
            .join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }
}
