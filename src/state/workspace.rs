use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::{pos2, Color32, Pos2, Rect, Vec2};
use uuid::Uuid;

use crate::canvas::config::{
    normalize_panel_size, DEFAULT_PANEL_HEIGHT, DEFAULT_PANEL_WIDTH, PANEL_GAP,
};
use crate::panel::CanvasPanel;
use crate::runtime::{PtyManager, RuntimeWorkspaceSnapshot, UiUpdateBatch};
use crate::state::WorkspaceState;
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
            viewport_pan,
            viewport_zoom,
            next_z,
            next_color,
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
        Self {
            id: workspace_id,
            name: cwd.as_deref().map(workspace_name_from_path).unwrap_or(name),
            cwd: cwd.clone(),
            folder_path_label: cwd.as_deref().map(workspace_path_label),
            panels,
            viewport_pan: egui::vec2(viewport_pan[0], viewport_pan[1]),
            viewport_zoom,
            next_z,
            next_color,
            pty_manager,
        }
    }

    pub fn to_saved(&self) -> WorkspaceState {
        WorkspaceState {
            id: self.id.to_string(),
            name: self.name.clone(),
            cwd: self.cwd.clone(),
            panels: self.panels.iter().map(CanvasPanel::to_saved).collect(),
            viewport_pan: [self.viewport_pan.x, self.viewport_pan.y],
            viewport_zoom: self.viewport_zoom,
            next_z: self.next_z,
            next_color: self.next_color,
        }
    }

    pub fn spawn_terminal(&mut self, ctx: &egui::Context) -> Uuid {
        let size = normalize_panel_size(egui::vec2(DEFAULT_PANEL_WIDTH, DEFAULT_PANEL_HEIGHT));
        let position = self.find_free_position(size);
        let color = PANEL_COLORS[self.next_color % PANEL_COLORS.len()];
        let mut panel = TerminalPanel::new(position, size, color, self.next_z);
        panel.focused = true;
        panel.attach_session(Arc::clone(&self.pty_manager), ctx, self.cwd.as_deref());
        let id = panel.id;
        self.panels.push(CanvasPanel::Terminal(panel));
        self.next_z += 1;
        self.next_color += 1;
        self.bring_to_front(id);
        id
    }

    pub fn add_restored_terminal(&mut self, panel: TerminalPanel) {
        self.panels.push(CanvasPanel::Terminal(panel));
    }

    pub fn bring_to_front(&mut self, panel_id: Uuid) {
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
        if let Some(last) = self.panels.iter_mut().max_by_key(|panel| panel.z_index()) {
            last.set_focused(true);
        }
    }

    pub fn focused_panel_mut(&mut self) -> Option<&mut CanvasPanel> {
        self.panels.iter_mut().find(|panel| panel.focused())
    }

    pub fn focused_panel(&self) -> Option<&CanvasPanel> {
        self.panels.iter().find(|panel| panel.focused())
    }

    pub fn panel_rects_except(&self, panel_id: Uuid) -> Vec<Rect> {
        self.panels
            .iter()
            .filter(|panel| panel.id() != panel_id)
            .map(CanvasPanel::rect)
            .collect()
    }

    pub fn find_free_position(&self, size: Vec2) -> Pos2 {
        if self.panels.is_empty() {
            return pos2(50.0, 50.0);
        }

        let gap = PANEL_GAP;
        let mut x_edges = Vec::new();
        let mut y_edges = Vec::new();

        for panel in &self.panels {
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
        let current_bbox = self
            .panels
            .iter()
            .map(CanvasPanel::rect)
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
        self.panels.iter().any(|panel| {
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

    pub fn runtime_snapshot(&self) -> RuntimeWorkspaceSnapshot {
        RuntimeWorkspaceSnapshot::new(self.id, self.name.clone(), self.cwd.clone())
    }

    pub fn drain_runtime_updates(&self) -> UiUpdateBatch {
        self.pty_manager
            .lock()
            .ok()
            .map(|manager| manager.drain_ui_updates())
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

    use egui::{pos2, vec2};
    use uuid::Uuid;

    use super::Workspace;
    use crate::canvas::config::PANEL_GAP;
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

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir()
            .join(format!("workspace-test-{}", Uuid::new_v4()))
            .join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }
}
