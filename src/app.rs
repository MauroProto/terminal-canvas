use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::Utc;
use egui::{
    pos2, vec2, Align2, Area, CentralPanel, Color32, FontId, Id, Key, Order, Pos2, Rect, SidePanel,
    Stroke, TopBottomPanel,
};
use uuid::Uuid;

use crate::canvas::config::{CANVAS_BG, SNAP_GUIDE_COLOR, ZOOM_KEYBOARD_FACTOR};
use crate::canvas::scene::handle_canvas_input;
use crate::canvas::snap::guide_endpoints;
use crate::canvas::viewport::Viewport;
use crate::collab::auth::normalize_optional_passphrase;
use crate::collab::{
    bind_addr_for_share_url, draw_remote_workspace, CollabEvent, CollabManager, CollabMode,
    CollabSessionState, HostShareOptions, RemotePanelAction, SerializableKey,
    SerializableModifiers, SharedWorkspaceSnapshot, TerminalInputEvent, TrustedDevice,
};
use crate::command_palette::commands::Command;
use crate::command_palette::CommandPalette;
use crate::orchestration::{
    AgentLaunchRequest, AgentProvider, Orchestrator, PanelRuntimeObservation, WorktreeMode,
};
use crate::runtime::RenderTier;
use crate::shortcuts::shortcut_command;
use crate::sidebar::{Sidebar, SidebarResponse};
use crate::state::persistence::{AutosaveController, AutosaveDecision};
use crate::state::{load_state, save_state, AppState, TerminalSpawnRequest, Workspace};
use crate::terminal::panel::{PanelHitArea, ResizeHandle};
use crate::theme::fonts::setup_fonts;
use crate::update::{RepaintPolicy, UpdateChecker};
use crate::utils::platform::home_dir;

mod dialogs;
mod perf;
mod taskbar;

use self::perf::FramePerfSnapshot;
use self::taskbar::{
    apply_taskbar_layout_to_workspace, clamp_rect_to_desktop, clamp_workspace_panels_to_desktop,
    desktop_canvas_rect, desktop_screen_rect, desktop_snap_rect_for_pointer, taskbar_button_colors,
    taskbar_provider_accent, taskbar_provider_label, taskbar_summary_label, truncate_taskbar_title,
    TaskbarLayoutPreset,
};

const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(2);
const RUNTIME_REPAINT_BATCH: Duration = Duration::from_millis(33);
const VIEWPORT_FOCUS_PADDING: f32 = 72.0;
const VIEWPORT_FOCUS_MAX_ZOOM: f32 = 2.0;
const VIEWPORT_OVERVIEW_PADDING: f32 = 84.0;
const VIEWPORT_OVERVIEW_MAX_ZOOM: f32 = 1.0;
const VIEWPORT_FOCUS_ANIMATION_SECS: f64 = 0.36;
const ORCHESTRATION_REFRESH_INTERVAL: Duration = Duration::from_millis(750);
const DESKTOP_MARGIN: f32 = 0.0;
const DESKTOP_SNAP_EDGE: f32 = 28.0;

#[derive(Clone)]
struct LaunchAgentDraft {
    workspace_id: Uuid,
    provider: AgentProvider,
    task_title: String,
    brief: String,
    worktree_mode: WorktreeMode,
    error: Option<String>,
}

#[derive(Clone)]
struct ShareWorkspaceDraft {
    broker_url: String,
    session_passphrase: String,
    acknowledge_trusted_live: bool,
    error: Option<String>,
}

#[derive(Clone)]
struct JoinSessionDraft {
    invite_code: String,
    display_name: String,
    session_passphrase: String,
    error: Option<String>,
}

#[derive(Clone, Copy)]
enum PanelGestureKind {
    Drag { origin: Pos2 },
    Resize { handle: ResizeHandle, origin: Rect },
}

#[derive(Clone, Copy)]
struct PanelGesture {
    panel_id: Uuid,
    pointer_origin: Pos2,
    kind: PanelGestureKind,
}

#[derive(Clone, Copy)]
struct ViewportAnimation {
    start: Viewport,
    target: Viewport,
    started_at: f64,
    duration: f64,
}

pub struct TerminalApp {
    workspaces: Vec<Workspace>,
    active_ws: usize,
    orchestrator: Orchestrator,
    collab: CollabManager,
    viewport: Viewport,
    sidebar_visible: bool,
    show_grid: bool,
    show_minimap: bool,
    ctx: Option<egui::Context>,
    command_palette: CommandPalette,
    renaming_panel: Option<Uuid>,
    rename_buf: String,
    brand_texture: Option<egui::TextureHandle>,
    sidebar: Sidebar,
    update_checker: UpdateChecker,
    fullscreen: bool,
    panel_gesture: Option<PanelGesture>,
    autosave: AutosaveController,
    persisted_state: Option<AppState>,
    viewport_animation: Option<ViewportAnimation>,
    repaint_policy: RepaintPolicy,
    launch_agent: Option<LaunchAgentDraft>,
    share_workspace_open: bool,
    share_workspace_draft: ShareWorkspaceDraft,
    join_session_open: bool,
    join_session_draft: JoinSessionDraft,
    local_device_id: String,
    trusted_devices: HashMap<String, TrustedDevice>,
    last_orchestration_refresh: Instant,
    last_orchestration_scan_duration: Duration,
    last_perf_snapshot: FramePerfSnapshot,
    layout_menu_open: bool,
}

impl TerminalApp {
    pub fn new(cc: &eframe::CreationContext<'_>, pending_join_invite: Option<String>) -> Self {
        setup_fonts(cc);
        let brand_texture = load_brand_texture(cc);
        let update_checker = UpdateChecker::new(&cc.egui_ctx);
        let loaded_state = load_state();
        let has_saved_state = loaded_state.is_some();

        let mut app = if let Some(saved) = loaded_state {
            let collab = CollabManager::new();
            let broker_url = collab.broker_url().to_owned();
            let orchestration = Orchestrator::from_saved(Some(saved.orchestration.clone()));
            let trusted_devices = saved
                .trusted_devices
                .iter()
                .cloned()
                .map(|device| (device.device_id.clone(), device))
                .collect();
            let local_device_id = saved.local_device_id.clone();
            let mut workspaces = Vec::new();
            for workspace in saved.workspaces {
                workspaces.push(Workspace::from_saved(workspace, &cc.egui_ctx));
            }
            let active_ws = saved.active_ws.min(workspaces.len().saturating_sub(1));
            let viewport = workspaces
                .get(active_ws)
                .map(|workspace| Viewport {
                    pan: workspace.viewport_pan,
                    zoom: workspace.viewport_zoom,
                })
                .unwrap_or_default();
            Self {
                workspaces,
                active_ws,
                orchestrator: orchestration,
                collab,
                viewport,
                sidebar_visible: saved.sidebar_visible,
                show_grid: saved.show_grid,
                show_minimap: saved.show_minimap,
                ctx: Some(cc.egui_ctx.clone()),
                command_palette: CommandPalette::default(),
                renaming_panel: None,
                rename_buf: String::new(),
                brand_texture,
                sidebar: Sidebar::default(),
                update_checker,
                fullscreen: false,
                panel_gesture: None,
                autosave: AutosaveController::new(AUTOSAVE_INTERVAL),
                persisted_state: None,
                viewport_animation: None,
                repaint_policy: RepaintPolicy::new(RUNTIME_REPAINT_BATCH),
                launch_agent: None,
                share_workspace_open: false,
                share_workspace_draft: ShareWorkspaceDraft {
                    broker_url,
                    session_passphrase: String::new(),
                    acknowledge_trusted_live: false,
                    error: None,
                },
                join_session_open: false,
                join_session_draft: JoinSessionDraft {
                    invite_code: String::new(),
                    display_name: default_guest_display_name(),
                    session_passphrase: String::new(),
                    error: None,
                },
                local_device_id,
                trusted_devices,
                last_orchestration_refresh: Instant::now()
                    .checked_sub(ORCHESTRATION_REFRESH_INTERVAL)
                    .unwrap_or_else(Instant::now),
                last_orchestration_scan_duration: Duration::ZERO,
                last_perf_snapshot: FramePerfSnapshot::default(),
                layout_menu_open: false,
            }
        } else {
            let collab = CollabManager::new();
            let broker_url = collab.broker_url().to_owned();
            let mut workspace = Workspace::new("Default", None);
            workspace.spawn_terminal(&cc.egui_ctx);
            Self {
                workspaces: vec![workspace],
                active_ws: 0,
                orchestrator: Orchestrator::new(),
                collab,
                viewport: Viewport::default(),
                sidebar_visible: true,
                show_grid: true,
                show_minimap: true,
                ctx: Some(cc.egui_ctx.clone()),
                command_palette: CommandPalette::default(),
                renaming_panel: None,
                rename_buf: String::new(),
                brand_texture,
                sidebar: Sidebar::default(),
                update_checker,
                fullscreen: false,
                panel_gesture: None,
                autosave: AutosaveController::new(AUTOSAVE_INTERVAL),
                persisted_state: None,
                viewport_animation: None,
                repaint_policy: RepaintPolicy::new(RUNTIME_REPAINT_BATCH),
                launch_agent: None,
                share_workspace_open: false,
                share_workspace_draft: ShareWorkspaceDraft {
                    broker_url,
                    session_passphrase: String::new(),
                    acknowledge_trusted_live: false,
                    error: None,
                },
                join_session_open: false,
                join_session_draft: JoinSessionDraft {
                    invite_code: String::new(),
                    display_name: default_guest_display_name(),
                    session_passphrase: String::new(),
                    error: None,
                },
                local_device_id: Uuid::new_v4().to_string(),
                trusted_devices: HashMap::new(),
                last_orchestration_refresh: Instant::now()
                    .checked_sub(ORCHESTRATION_REFRESH_INTERVAL)
                    .unwrap_or_else(Instant::now),
                last_orchestration_scan_duration: Duration::ZERO,
                last_perf_snapshot: FramePerfSnapshot::default(),
                layout_menu_open: false,
            }
        };

        if let Some(workspace) = app.workspaces.get(app.active_ws) {
            app.viewport.pan = workspace.viewport_pan;
            app.viewport.zoom = workspace.viewport_zoom.max(0.125);
        }

        if has_saved_state {
            app.persisted_state = Some(app.snapshot_state());
        }

        app.reconcile_orchestration();
        app.refresh_orchestration();
        app.share_workspace_draft.broker_url = app.collab.broker_url().to_owned();
        if let Some(invite_code) = pending_join_invite {
            app.join_session_open = true;
            app.join_session_draft.invite_code = invite_code;
            app.join_session_draft.error = None;
        }

        app
    }

    fn ws(&self) -> &Workspace {
        &self.workspaces[self.active_ws]
    }

    fn ws_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active_ws]
    }

    fn snapshot_state(&self) -> AppState {
        let workspaces = self
            .workspaces
            .iter()
            .enumerate()
            .map(|(index, workspace)| {
                let mut saved = workspace.to_saved();
                if index == self.active_ws {
                    saved.viewport_pan = [self.viewport.pan.x, self.viewport.pan.y];
                    saved.viewport_zoom = self.viewport.zoom;
                }
                saved
            })
            .collect();

        AppState {
            workspaces,
            active_ws: self.active_ws,
            sidebar_visible: self.sidebar_visible,
            show_grid: self.show_grid,
            show_minimap: self.show_minimap,
            local_device_id: self.local_device_id.clone(),
            trusted_devices: self.trusted_devices_snapshot(),
            orchestration: self.orchestrator.snapshot(),
        }
    }

    fn trusted_devices_snapshot(&self) -> Vec<TrustedDevice> {
        let mut devices = self.trusted_devices.values().cloned().collect::<Vec<_>>();
        devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        devices
    }

    fn remember_trusted_device(&mut self, device_id: &str, display_name: &str) {
        if device_id.trim().is_empty() {
            return;
        }
        let now = Utc::now();
        let entry = self
            .trusted_devices
            .entry(device_id.to_owned())
            .or_insert_with(|| TrustedDevice {
                device_id: device_id.to_owned(),
                last_display_name: display_name.to_owned(),
                approved_at: now,
                last_seen_at: now,
            });
        entry.last_display_name = display_name.to_owned();
        entry.last_seen_at = now;
    }

    fn reconcile_orchestration(&mut self) {
        let mut live_panel_ids = HashSet::new();
        for workspace in &self.workspaces {
            for panel in &workspace.panels {
                live_panel_ids.insert(panel.id());
                self.orchestrator.ensure_panel_session(
                    workspace.id,
                    workspace.cwd.clone(),
                    panel.id(),
                    panel.runtime_session_id(),
                    panel.title(),
                );
            }
        }
        self.orchestrator.prune_missing_panels(&live_panel_ids);
    }

    fn collect_observations(&self) -> Vec<PanelRuntimeObservation> {
        self.workspaces
            .iter()
            .flat_map(Workspace::orchestration_observations)
            .collect()
    }

    fn refresh_orchestration(&mut self) {
        let observations = self.collect_observations();
        self.orchestrator.apply_observations(observations);
        self.last_orchestration_refresh = Instant::now();
    }

    fn maybe_refresh_orchestration(&mut self) {
        if self.panel_gesture.is_some() {
            return;
        }
        if self.last_orchestration_refresh.elapsed() >= ORCHESTRATION_REFRESH_INTERVAL {
            self.reconcile_orchestration();
            let started_at = Instant::now();
            self.refresh_orchestration();
            self.last_orchestration_scan_duration = started_at.elapsed();
        }
    }

    fn workspace_index_by_id(&self, workspace_id: Uuid) -> Option<usize> {
        self.workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
    }

    fn focus_panel_across_workspaces(&mut self, panel_id: Uuid, canvas_rect: Option<Rect>) {
        if let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.panel(panel_id).is_some())
        {
            self.switch_workspace(index);
            let is_minimized = self
                .ws()
                .panel(panel_id)
                .map(|panel| panel.minimized())
                .unwrap_or(false);
            if is_minimized {
                self.ws_mut().restore_panel(panel_id);
            } else {
                self.ws_mut().bring_to_front(panel_id);
            }
            if let Some(canvas_rect) = canvas_rect {
                if let Some(panel) = self.ws().panel(panel_id) {
                    self.viewport = self.viewport.focus_on_rect(
                        panel.rect(),
                        canvas_rect,
                        VIEWPORT_FOCUS_PADDING,
                        VIEWPORT_FOCUS_MAX_ZOOM,
                    );
                }
            }
        }
    }

    fn open_launch_agent_dialog(&mut self) {
        let workspace_id = self.ws().id;
        self.launch_agent = Some(LaunchAgentDraft {
            workspace_id,
            provider: AgentProvider::ClaudeCode,
            task_title: "".to_owned(),
            brief: "".to_owned(),
            worktree_mode: WorktreeMode::Auto,
            error: None,
        });
    }

    fn open_share_workspace_dialog(&mut self) {
        self.share_workspace_open = true;
        self.share_workspace_draft.error = None;
        self.share_workspace_draft.broker_url = self.collab.broker_url().to_owned();
    }

    fn open_join_session_dialog(&mut self) {
        self.join_session_open = true;
        self.join_session_draft.error = None;
    }

    fn start_share_workspace(&mut self) {
        if !self.share_workspace_draft.acknowledge_trusted_live {
            self.share_workspace_draft.error = Some(
                "Tenés que confirmar que Trusted Live usa terminales reales del host.".to_owned(),
            );
            return;
        }
        let session_passphrase =
            normalize_optional_passphrase(&self.share_workspace_draft.session_passphrase);
        let reachable_url = self.share_workspace_draft.broker_url.trim().to_owned();
        let bind_addr = match bind_addr_for_share_url(&reachable_url) {
            Ok(bind_addr) => bind_addr,
            Err(err) => {
                self.share_workspace_draft.error = Some(err.to_string());
                return;
            }
        };
        self.collab.set_broker_url(reachable_url.clone());
        match self.collab.start_host_session(
            self.ws().id,
            HostShareOptions {
                bind_addr,
                reachable_url,
            },
            session_passphrase,
            self.trusted_devices_snapshot(),
        ) {
            Ok(()) => {
                self.share_workspace_draft.error = None;
                self.share_workspace_open = true;
            }
            Err(err) => {
                self.share_workspace_draft.error = Some(err.to_string());
            }
        }
    }

    fn submit_join_session(&mut self) {
        let invite_code = self.join_session_draft.invite_code.trim().to_owned();
        let display_name = self.join_session_draft.display_name.trim().to_owned();
        let session_passphrase =
            normalize_optional_passphrase(&self.join_session_draft.session_passphrase);
        if invite_code.is_empty() || display_name.is_empty() {
            self.join_session_draft.error =
                Some("Pegá un invite code y un nombre visible.".to_owned());
            return;
        }
        match self.collab.join_session(
            &invite_code,
            display_name,
            session_passphrase,
            self.local_device_id.clone(),
        ) {
            Ok(()) => {
                self.join_session_draft.error = None;
                self.join_session_open = false;
                self.viewport = Viewport::default();
            }
            Err(err) => {
                self.join_session_draft.error = Some(err.to_string());
            }
        }
    }

    fn shared_workspace(&self) -> Option<&Workspace> {
        let workspace_id = self.collab.shared_workspace_id()?;
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
    }

    fn build_shared_workspace_snapshot(&self) -> Option<SharedWorkspaceSnapshot> {
        let workspace = self.shared_workspace()?;
        Some(SharedWorkspaceSnapshot {
            workspace_id: workspace.id,
            workspace_name: workspace.name.clone(),
            generated_at: Utc::now(),
            guests: Vec::new(),
            terminal_controls: Vec::new(),
            panels: workspace.shared_panel_snapshots(),
        })
    }

    fn publish_collab_snapshot(&mut self) {
        if !matches!(self.collab.mode(), CollabMode::Host) {
            return;
        }
        if let Some(snapshot) = self.build_shared_workspace_snapshot() {
            self.collab.publish_snapshot(snapshot);
        }
    }

    fn handle_collab_events(&mut self) {
        let shared_workspace_id = self.collab.shared_workspace_id();
        for event in self.collab.drain_events() {
            match event {
                CollabEvent::RemoteInput { guest_id, input } => {
                    if self.collab.controller_for(input.terminal_id) != Some(guest_id) {
                        continue;
                    }
                    let Some(workspace_id) = shared_workspace_id else {
                        continue;
                    };
                    if let Some(index) = self.workspace_index_by_id(workspace_id) {
                        self.workspaces[index].apply_remote_input(input.terminal_id, &input.events);
                    }
                }
            }
        }
    }

    fn submit_launch_agent(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.launch_agent.clone() else {
            return;
        };
        let request = AgentLaunchRequest {
            workspace_id: draft.workspace_id,
            task_id: None,
            base_cwd: self
                .workspace_index_by_id(draft.workspace_id)
                .and_then(|index| self.workspaces.get(index))
                .and_then(|workspace| workspace.cwd.clone()),
            provider: draft.provider,
            task_title: draft.task_title.clone(),
            brief: draft.brief.clone(),
            worktree_mode: draft.worktree_mode,
        };
        let plan = match self.orchestrator.prepare_launch(request) {
            Ok(plan) => plan,
            Err(err) => {
                if let Some(current) = self.launch_agent.as_mut() {
                    current.error = Some(err.to_string());
                }
                return;
            }
        };
        let Some(workspace_index) = self.workspace_index_by_id(draft.workspace_id) else {
            if let Some(current) = self.launch_agent.as_mut() {
                current.error = Some("Workspace not found".to_owned());
            }
            return;
        };

        let spawned = {
            let workspace = &mut self.workspaces[workspace_index];
            workspace.spawn_terminal_with_request(
                ctx,
                TerminalSpawnRequest {
                    title: Some(plan.panel_title.clone()),
                    cwd: plan.cwd.clone(),
                    startup_command: plan.startup_command.clone(),
                    startup_input: plan.startup_input.clone(),
                },
            )
        };
        self.orchestrator.bind_launch_to_panel(
            plan.session_id,
            spawned.panel_id,
            spawned.runtime_session_id,
        );
        self.switch_workspace(workspace_index);
        self.ws_mut().bring_to_front(spawned.panel_id);
        self.launch_agent = None;
        self.reconcile_orchestration();
        self.refresh_orchestration();
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) -> Option<Command> {
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            {
                if modifiers.ctrl && modifiers.shift && key == Key::P {
                    self.command_palette.toggle();
                    return None;
                }
                if let Some(command) = shortcut_command(&modifiers, key) {
                    return Some(command);
                }
            }
        }
        None
    }

    fn execute_command(&mut self, command: Command, ctx: &egui::Context, canvas_rect: Rect) {
        if matches!(self.collab.mode(), CollabMode::Guest)
            && !matches!(
                command,
                Command::ZoomToFitAll
                    | Command::ToggleSidebar
                    | Command::ToggleMinimap
                    | Command::ToggleGrid
                    | Command::ZoomIn
                    | Command::ZoomOut
                    | Command::ResetZoom
                    | Command::ToggleFullscreen
            )
        {
            return;
        }
        self.viewport_animation = None;
        match command {
            Command::NewTerminal => {
                self.ws_mut().spawn_terminal(ctx);
                self.reconcile_orchestration();
            }
            Command::LaunchAgent => self.open_launch_agent_dialog(),
            Command::ShareWorkspace => self.open_share_workspace_dialog(),
            Command::JoinSharedSession => self.open_join_session_dialog(),
            Command::OpenFolder => self.pick_workspace_folder(ctx),
            Command::CloseTerminal => {
                if let Some(panel_id) = self.ws().focused_panel().map(|panel| panel.id()) {
                    self.ws_mut().close_panel(panel_id);
                    self.reconcile_orchestration();
                }
            }
            Command::RenameTerminal => {
                if let Some(panel) = self.ws().focused_panel() {
                    let panel_id = panel.id();
                    let panel_title = panel.title().to_owned();
                    self.renaming_panel = Some(panel_id);
                    self.rename_buf = panel_title;
                }
            }
            Command::FocusNext => self.focus_relative(1),
            Command::FocusPrev => self.focus_relative(-1),
            Command::ZoomToFitAll => self.zoom_to_fit_all(canvas_rect),
            Command::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            Command::ToggleMinimap => self.show_minimap = !self.show_minimap,
            Command::ToggleGrid => self.show_grid = !self.show_grid,
            Command::ZoomIn => {
                let center = canvas_rect.center();
                self.viewport
                    .zoom_around(center, canvas_rect, ZOOM_KEYBOARD_FACTOR);
            }
            Command::ZoomOut => {
                let center = canvas_rect.center();
                self.viewport
                    .zoom_around(center, canvas_rect, 1.0 / ZOOM_KEYBOARD_FACTOR);
            }
            Command::ResetZoom => {
                self.viewport.zoom = 1.0;
                self.viewport.pan = egui::Vec2::ZERO;
            }
            Command::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            }
        }
    }

    fn focus_relative(&mut self, direction: isize) {
        if !self.ws().panels.iter().any(|panel| !panel.minimized()) {
            return;
        }
        let mut order: Vec<_> = self
            .ws()
            .panels
            .iter()
            .filter(|panel| !panel.minimized())
            .map(|panel| (panel.z_index(), panel.id()))
            .collect();
        order.sort_by_key(|(z, _)| *z);
        let current = self
            .ws()
            .focused_panel()
            .map(|panel| panel.id())
            .and_then(|id| order.iter().position(|(_, current)| *current == id))
            .unwrap_or(0);
        let next = (current as isize + direction).rem_euclid(order.len() as isize) as usize;
        self.ws_mut().bring_to_front(order[next].1);
    }

    fn zoom_to_fit_all(&mut self, canvas_rect: Rect) {
        if !self.ws().panels.iter().any(|panel| !panel.minimized()) {
            return;
        }
        let bounds = self
            .ws()
            .panels
            .iter()
            .filter(|panel| !panel.minimized())
            .map(|panel| panel.rect())
            .reduce(|a, b| a.union(b))
            .unwrap()
            .expand(50.0);

        let scale_x = canvas_rect.width() / bounds.width().max(1.0);
        let scale_y = canvas_rect.height() / bounds.height().max(1.0);
        self.viewport.zoom = scale_x.min(scale_y).clamp(0.125, 4.0);
        self.viewport.pan_to_center(bounds.center(), canvas_rect);
    }

    fn switch_workspace(&mut self, index: usize) {
        if index == self.active_ws || index >= self.workspaces.len() {
            return;
        }
        self.viewport_animation = None;
        self.workspaces[self.active_ws].viewport_pan = self.viewport.pan;
        self.workspaces[self.active_ws].viewport_zoom = self.viewport.zoom;
        self.active_ws = index;
        self.viewport.pan = self.workspaces[self.active_ws].viewport_pan;
        self.viewport.zoom = self.workspaces[self.active_ws].viewport_zoom;
    }

    fn pick_workspace_folder(&mut self, ctx: &egui::Context) {
        let start_dir = self
            .workspaces
            .get(self.active_ws)
            .and_then(|workspace| workspace.cwd().map(|path| path.to_path_buf()))
            .or_else(home_dir);
        let mut dialog = rfd::FileDialog::new();
        if let Some(start_dir) = start_dir {
            dialog = dialog.set_directory(start_dir);
        }

        if let Some(path) = dialog.pick_folder() {
            self.open_workspace_folder(ctx, path);
        }
    }

    fn open_workspace_folder(&mut self, ctx: &egui::Context, path: PathBuf) {
        let already_open = self
            .workspaces
            .iter()
            .any(|workspace| workspace.matches_cwd(&path));
        let index = upsert_workspace_for_folder(&mut self.workspaces, path);
        self.switch_workspace(index);
        if !already_open || self.ws().panels.is_empty() {
            self.ws_mut().spawn_terminal(ctx);
        }
        self.reconcile_orchestration();
        self.refresh_orchestration();
    }

    fn handle_sidebar_responses(&mut self, responses: Vec<SidebarResponse>, ctx: &egui::Context) {
        for response in responses {
            match response {
                SidebarResponse::SwitchWorkspace(index) => self.switch_workspace(index),
                SidebarResponse::OpenFolder => self.pick_workspace_folder(ctx),
                SidebarResponse::DeleteWorkspace(index) => {
                    if self.workspaces.len() > 1 && index < self.workspaces.len() {
                        self.workspaces.remove(index);
                        self.active_ws =
                            self.active_ws.min(self.workspaces.len().saturating_sub(1));
                    }
                }
                SidebarResponse::FocusPanel(panel_id) => {
                    self.focus_panel_across_workspaces(panel_id, Some(ctx.available_rect()));
                }
                SidebarResponse::SpawnTerminal(index) => {
                    if let Some(workspace) = self.workspaces.get_mut(index) {
                        workspace.spawn_terminal(ctx);
                    }
                    self.reconcile_orchestration();
                }
                SidebarResponse::RenamePanel(panel_id) => {
                    self.renaming_panel = Some(panel_id);
                    if let Some(panel) =
                        self.ws().panels.iter().find(|panel| panel.id() == panel_id)
                    {
                        self.rename_buf = panel.title().to_owned();
                    }
                }
                SidebarResponse::ClosePanel(panel_id) => {
                    self.ws_mut().close_panel(panel_id);
                    self.reconcile_orchestration();
                }
                SidebarResponse::OpenShareWorkspace => self.open_share_workspace_dialog(),
                SidebarResponse::OpenJoinSession => self.open_join_session_dialog(),
                SidebarResponse::OpenCollabSession => match self.collab.mode() {
                    CollabMode::Inactive | CollabMode::Host => {
                        self.open_share_workspace_dialog();
                    }
                    CollabMode::Guest => {
                        self.open_join_session_dialog();
                    }
                },
                SidebarResponse::StopCollabSession => {
                    self.collab.stop_session();
                }
            }
        }
        self.reconcile_orchestration();
    }

    fn maybe_persist_state(&mut self, ctx: &egui::Context) {
        let snapshot = self.snapshot_state();
        let now = Instant::now();
        match self
            .autosave
            .should_persist(&snapshot, self.persisted_state.as_ref(), now)
        {
            AutosaveDecision::Idle => {}
            AutosaveDecision::ScheduleAfter(delay) => ctx.request_repaint_after(delay),
            AutosaveDecision::SaveNow => {
                match crate::state::persistence::try_save_state(&snapshot) {
                    Ok(()) => {
                        self.persisted_state = Some(snapshot);
                        self.autosave.mark_saved(now);
                    }
                    Err(err) => {
                        log::warn!("Autosave failed: {err}");
                        ctx.request_repaint_after(AUTOSAVE_INTERVAL);
                    }
                }
            }
        }
    }

    fn start_focus_animation(&mut self, panel_id: Uuid, canvas_rect: Rect, now: f64) {
        let Some(panel) = self.ws().panels.iter().find(|panel| panel.id() == panel_id) else {
            return;
        };
        let target = self.viewport.focus_on_rect(
            panel.rect(),
            canvas_rect,
            VIEWPORT_FOCUS_PADDING,
            VIEWPORT_FOCUS_MAX_ZOOM,
        );
        if target.pan == self.viewport.pan && (target.zoom - self.viewport.zoom).abs() < 0.001 {
            self.viewport_animation = None;
            return;
        }
        self.viewport_animation = Some(ViewportAnimation {
            start: self.viewport,
            target,
            started_at: now,
            duration: VIEWPORT_FOCUS_ANIMATION_SECS,
        });
    }

    fn start_overview_animation(&mut self, canvas_rect: Rect, now: f64) {
        let target = overview_viewport_for_panels(
            &self.ws().panels,
            canvas_rect,
            VIEWPORT_OVERVIEW_PADDING,
            VIEWPORT_OVERVIEW_MAX_ZOOM,
        );
        if target.pan == self.viewport.pan && (target.zoom - self.viewport.zoom).abs() < 0.001 {
            self.viewport_animation = None;
            return;
        }
        self.viewport_animation = Some(ViewportAnimation {
            start: self.viewport,
            target,
            started_at: now,
            duration: VIEWPORT_FOCUS_ANIMATION_SECS,
        });
    }

    fn update_viewport_animation(&mut self, ctx: &egui::Context) {
        let Some(animation) = self.viewport_animation else {
            return;
        };
        let now = ctx.input(|i| i.time);
        let progress = ((now - animation.started_at) / animation.duration).clamp(0.0, 1.0) as f32;
        let eased = ease_in_out_cubic(progress);
        self.viewport = interpolate_viewport(animation.start, animation.target, eased);
        if progress >= 1.0 {
            self.viewport_animation = None;
        } else {
            ctx.request_repaint();
        }
    }
}

impl eframe::App for TerminalApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let frame_started_at = Instant::now();
        let mut perf_snapshot = FramePerfSnapshot {
            orchestration_scan_time: self.last_orchestration_scan_duration,
            ..FramePerfSnapshot::default()
        };
        self.ctx = Some(ctx.clone());
        self.handle_collab_events();
        self.maybe_refresh_orchestration();

        if let Some(command) = self.handle_shortcuts(ctx) {
            let canvas_rect = ctx.available_rect();
            self.execute_command(command, ctx, canvas_rect);
        }

        let runtime_updates = self.ws().drain_runtime_updates();
        if !runtime_updates.session_updates.is_empty() {
            let dirty_sessions = runtime_updates
                .session_updates
                .iter()
                .map(|update| update.session_id)
                .collect::<HashSet<_>>();
            let focused_dirty = self
                .ws()
                .focused_panel()
                .and_then(|panel| panel.runtime_session_id())
                .map(|session_id| dirty_sessions.contains(&session_id))
                .unwrap_or(false);
            if focused_dirty {
                self.repaint_policy.note_focused_runtime_event();
            } else {
                self.repaint_policy.note_runtime_event();
            }
            for panel in &mut self.ws_mut().panels {
                if panel
                    .runtime_session_id()
                    .map(|session_id| dirty_sessions.contains(&session_id))
                    .unwrap_or(false)
                {
                    panel.sync_title();
                }
            }
        }
        let runtime_repaint_now = self.repaint_policy.should_repaint_now();
        perf_snapshot.runtime_repaint = runtime_repaint_now;
        let (attached_sessions, detached_sessions) = self
            .workspaces
            .iter()
            .map(Workspace::runtime_session_counts)
            .fold(
                (0, 0),
                |(attached_acc, detached_acc), (attached, detached)| {
                    (attached_acc + attached, detached_acc + detached)
                },
            );
        perf_snapshot.attached_sessions = attached_sessions;
        perf_snapshot.detached_sessions = detached_sessions;

        if !self.command_palette.open
            && self.renaming_panel.is_none()
            && !matches!(self.collab.mode(), CollabMode::Guest)
        {
            let focused_panel_id = self.ws().focused_panel().map(|panel| panel.id());
            if let Some(panel_id) = focused_panel_id {
                if matches!(self.collab.mode(), CollabMode::Host)
                    && host_terminal_input_pending(ctx)
                    && self.collab.controller_for(panel_id).is_some()
                {
                    self.collab.revoke_control(panel_id, "Host took control");
                }
            }
            if let Some(panel) = self.ws_mut().focused_panel_mut() {
                panel.handle_input(ctx);
            }
        }

        if self.sidebar_visible && !matches!(self.collab.mode(), CollabMode::Guest) {
            SidePanel::left("sidebar")
                .resizable(false)
                .default_width(212.0)
                .show(ctx, |ui| {
                    let state = self.update_checker.snapshot();
                    let responses = self.sidebar.show(
                        ui,
                        self.brand_texture.as_ref(),
                        &self.workspaces,
                        self.active_ws,
                        &state,
                        self.collab.mode(),
                        self.collab.session_state(),
                    );
                    self.handle_sidebar_responses(responses, ctx);
                });
        }

        let mut requested_layout = None;
        if !matches!(self.collab.mode(), CollabMode::Guest) {
            let mut requested_panel = None;
            let mut layout_menu_anchor = None;
            let mut layout_button_hovered = false;
            let mut taskbar_panels: Vec<_> = self
                .ws()
                .panels
                .iter()
                .map(|panel| {
                    let provider = taskbar_provider_label(
                        self.orchestrator
                            .panel_overlay(panel.id())
                            .map(|overlay| overlay.provider),
                        panel.provider_hint(),
                        panel.title(),
                    );
                    (
                        panel.z_index(),
                        panel.id(),
                        panel.title().to_owned(),
                        panel.minimized(),
                        panel.focused(),
                        provider,
                    )
                })
                .collect();
            taskbar_panels.sort_by_key(|(z, ..)| *z);
            let summary =
                taskbar_summary_label(self.ws().panel_count(), self.ws().minimized_panel_count());
            TopBottomPanel::bottom("window-taskbar")
                .resizable(false)
                .exact_height(42.0)
                .show(ctx, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(summary)
                                .size(11.5)
                                .color(Color32::from_rgb(150, 150, 158)),
                        );
                        ui.add_space(6.0);
                        let layout_button = ui.add(
                            egui::Button::new(
                                egui::RichText::new("Acomodar")
                                    .size(11.5)
                                    .color(Color32::WHITE),
                            )
                            .fill(Color32::from_rgb(44, 44, 52))
                            .stroke(Stroke::new(1.0, Color32::from_rgb(74, 74, 84)))
                            .rounding(8.0),
                        );
                        layout_menu_anchor = Some(layout_button.rect);
                        layout_button_hovered = layout_button.hovered();
                        if layout_button.clicked() {
                            self.layout_menu_open = !self.layout_menu_open;
                        }
                        ui.add_space(8.0);
                        for (_, panel_id, title, minimized, focused, provider) in &taskbar_panels {
                            let label = if *minimized {
                                format!("  □ {}", truncate_taskbar_title(title))
                            } else {
                                format!("  {}", truncate_taskbar_title(title))
                            };
                            let (fill, stroke_color, text_color) =
                                taskbar_button_colors(*provider, *focused, *minimized);
                            let accent = taskbar_provider_accent(*provider);
                            let response = ui.add(
                                egui::Button::new(
                                    egui::RichText::new(label).size(11.5).color(text_color),
                                )
                                .fill(fill)
                                .stroke(Stroke::new(1.0, stroke_color))
                                .rounding(8.0),
                            );
                            let accent_rect = Rect::from_min_max(
                                pos2(response.rect.left() + 5.0, response.rect.top() + 5.0),
                                pos2(response.rect.left() + 11.0, response.rect.bottom() - 5.0),
                            );
                            ui.painter().rect_filled(accent_rect, 3.0, accent);
                            if response.clicked() {
                                requested_panel = Some(*panel_id);
                            }
                        }
                    });
                });
            if self.layout_menu_open {
                if let Some(anchor) = layout_menu_anchor {
                    let menu_response = Area::new(Id::new("taskbar-layout-menu"))
                        .order(Order::Foreground)
                        .fixed_pos(pos2(anchor.left(), anchor.top() - 152.0))
                        .show(ctx, |ui| {
                            egui::Frame::default()
                                .fill(Color32::from_rgb(26, 26, 30))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(74, 74, 84)))
                                .rounding(12.0)
                                .show(ui, |ui| {
                                    ui.set_min_width(180.0);
                                    ui.label(
                                        egui::RichText::new("Acomodar ventanas")
                                            .size(11.5)
                                            .color(Color32::from_rgb(166, 166, 174)),
                                    );
                                    ui.add_space(6.0);
                                    for preset in [
                                        TaskbarLayoutPreset::SideBySide,
                                        TaskbarLayoutPreset::Stacked,
                                        TaskbarLayoutPreset::Grid,
                                        TaskbarLayoutPreset::Cascade,
                                    ] {
                                        if ui.button(preset.label()).clicked() {
                                            requested_layout = Some(preset);
                                            self.layout_menu_open = false;
                                        }
                                    }
                                })
                                .response
                        })
                        .inner;
                    if ctx.input(|i| i.pointer.primary_clicked())
                        && !layout_button_hovered
                        && !menu_response.hovered()
                    {
                        self.layout_menu_open = false;
                    }
                } else {
                    self.layout_menu_open = false;
                }
            }
            if let Some(panel_id) = requested_panel {
                self.focus_panel_across_workspaces(panel_id, Some(ctx.available_rect()));
            }
        }

        CentralPanel::default().show(ctx, |ui| {
            let canvas_rect = ui.max_rect();
            ui.painter().rect_filled(canvas_rect, 0.0, CANVAS_BG);

            if matches!(self.collab.mode(), CollabMode::Guest) {
                let guest_snapshot = self.collab.guest_view().snapshot.clone();
                let guest_focused_panel = self.collab.guest_view().focused_panel;
                let guest_id = self.collab.guest_view().my_guest_id;
                let scroll_offsets = self.collab.guest_view().scroll_offsets.clone();
                let canvas_input =
                    handle_canvas_input(ui, &mut self.viewport, canvas_rect, false, false);

                if let Some(snapshot) = guest_snapshot {
                    if let Some(action) = draw_remote_workspace(
                        ui,
                        &snapshot,
                        &self.viewport,
                        canvas_rect,
                        self.collab.session_state(),
                        guest_focused_panel,
                        guest_id,
                        &scroll_offsets,
                    ) {
                        match action {
                            RemotePanelAction::Focus(panel_id) => {
                                self.collab.focus_remote_panel(panel_id)
                            }
                            RemotePanelAction::RequestControl(panel_id) => {
                                self.collab.request_control(panel_id)
                            }
                        }
                    }

                    if !matches!(self.collab.session_state(), CollabSessionState::Live) {
                        draw_guest_session_banner(ui, canvas_rect, self.collab.session_state());
                    }

                    let (smooth_scroll_delta, zoom_delta, modifiers) =
                        ctx.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers));
                    if smooth_scroll_delta.y != 0.0
                        && !panel_zoom_gesture_active(smooth_scroll_delta, zoom_delta, modifiers)
                    {
                        if let Some(panel_id) = guest_focused_panel {
                            let controlled_by_me = snapshot
                                .panels
                                .iter()
                                .find(|panel| panel.panel_id == panel_id)
                                .and_then(|panel| panel.controller)
                                == guest_id;
                            if controlled_by_me {
                                self.collab.send_guest_input(
                                    panel_id,
                                    vec![TerminalInputEvent::Scroll {
                                        delta: smooth_scroll_delta.y,
                                    }],
                                );
                            } else {
                                let delta_lines = (smooth_scroll_delta.y.signum()
                                    * (smooth_scroll_delta.y.abs() / 24.0).max(1.0))
                                    as i32;
                                self.collab.scroll_remote_panel(panel_id, delta_lines);
                            }
                        }
                    }

                    if let Some(panel_id) = guest_focused_panel {
                        let controlled_by_me = snapshot
                            .panels
                            .iter()
                            .find(|panel| panel.panel_id == panel_id)
                            .and_then(|panel| panel.controller)
                            == guest_id;
                        if controlled_by_me
                            && !self.command_palette.open
                            && !self.join_session_open
                            && !self.share_workspace_open
                        {
                            let events = collect_guest_terminal_input(ctx);
                            if !events.is_empty() {
                                self.collab.send_guest_input(panel_id, events);
                            }
                        }
                    }
                } else {
                    ui.painter().text(
                        canvas_rect.center(),
                        Align2::CENTER_CENTER,
                        "Waiting for shared workspace…",
                        FontId::proportional(20.0),
                        Color32::from_rgb(188, 188, 194),
                    );
                }

                if canvas_input.navigating {
                    ui.ctx().request_repaint();
                }
                return;
            }

            let (
                latest_pos,
                hover_pos,
                interact_pos,
                primary_pressed,
                primary_released,
                primary_clicked,
                primary_double_clicked,
                primary_down,
                smooth_scroll_delta,
                zoom_delta,
                modifiers,
            ) = ctx.input(|i| {
                (
                    i.pointer.latest_pos(),
                    i.pointer.hover_pos(),
                    i.pointer.interact_pos(),
                    i.pointer.primary_pressed(),
                    i.pointer.primary_released(),
                    i.pointer.primary_clicked(),
                    i.pointer
                        .button_double_clicked(egui::PointerButton::Primary),
                    i.pointer.primary_down(),
                    i.smooth_scroll_delta,
                    i.zoom_delta(),
                    i.modifiers,
                )
            });
            self.viewport = Viewport::default();
            self.viewport_animation = None;
            let desktop_rect = desktop_canvas_rect(canvas_rect);
            let desktop_screen = desktop_screen_rect(canvas_rect, desktop_rect);
            if let Some(preset) = requested_layout {
                apply_taskbar_layout_to_workspace(self.ws_mut(), preset, desktop_rect);
            }
            clamp_workspace_panels_to_desktop(self.ws_mut(), desktop_rect);
            ui.painter()
                .rect_filled(desktop_screen, 16.0, Color32::from_rgb(16, 16, 20));
            ui.painter().rect_stroke(
                desktop_screen,
                16.0,
                Stroke::new(1.0, Color32::from_rgb(44, 44, 54)),
            );
            let pointer_pos = gesture_pointer_pos(latest_pos, interact_pos, hover_pos);
            let hovered_hit = pointer_pos
                .filter(|pos| desktop_screen.contains(*pos))
                .and_then(|pos| top_panel_hit(self.ws(), pos, &self.viewport, canvas_rect));
            let scroll_target = pointer_pos
                .filter(|pos| desktop_screen.contains(*pos))
                .and_then(|pos| top_panel_scroll_hit(self.ws(), pos, &self.viewport, canvas_rect));
            let hovered_panel = hovered_hit.is_some();
            let scroll_capture_active = panel_scroll_capture_active(
                hovered_panel,
                smooth_scroll_delta,
                zoom_delta,
                modifiers,
            );

            if scroll_capture_active {
                if let (Some(index), scroll_y) = (scroll_target, smooth_scroll_delta.y) {
                    if scroll_y != 0.0 {
                        let panel_id = self.ws().panels[index].id();
                        let viewport = self.viewport;
                        if matches!(self.collab.mode(), CollabMode::Host)
                            && self.collab.controller_for(panel_id).is_some()
                        {
                            self.collab.revoke_control(panel_id, "Host took control");
                        }
                        let panel = &mut self.ws_mut().panels[index];
                        panel.handle_scroll(scroll_y, pointer_pos, &viewport, canvas_rect, ctx);
                    }
                }
            }

            let mut guides = Vec::new();
            let mut snap_preview_rect = None;
            let fast_path_render = self.panel_gesture.is_some();
            let needs_interaction_repaint = scroll_capture_active || self.panel_gesture.is_some();
            if primary_pressed {
                match hovered_hit {
                    Some(hit) => {
                        let panel_id = self.ws().panels[hit.index].id();
                        let already_focused =
                            self.ws().focused_panel().map(|panel| panel.id()) == Some(panel_id);
                        if !already_focused {
                            self.ws_mut().bring_to_front(panel_id);
                        }
                        match hit.area {
                            PanelHitArea::TitleBar => {
                                let origin = self.ws().panels[hit.index].position();
                                self.panel_gesture = Some(PanelGesture {
                                    panel_id,
                                    pointer_origin: hit.pointer,
                                    kind: PanelGestureKind::Drag { origin },
                                });
                                if let Some(panel) = self
                                    .ws_mut()
                                    .panels
                                    .iter_mut()
                                    .find(|panel| panel.id() == panel_id)
                                {
                                    panel.set_drag_virtual_pos(Some(origin));
                                }
                            }
                            PanelHitArea::Resize(handle) => {
                                let origin = self.ws().panels[hit.index].rect();
                                self.panel_gesture = Some(PanelGesture {
                                    panel_id,
                                    pointer_origin: hit.pointer,
                                    kind: PanelGestureKind::Resize { handle, origin },
                                });
                                if let Some(panel) = self
                                    .ws_mut()
                                    .panels
                                    .iter_mut()
                                    .find(|panel| panel.id() == panel_id)
                                {
                                    panel.set_resize_virtual_rect(Some(origin));
                                }
                            }
                            PanelHitArea::Body
                            | PanelHitArea::CloseButton
                            | PanelHitArea::MinimizeButton => {
                                self.panel_gesture = None;
                            }
                        }
                    }
                    None => {
                        self.panel_gesture = None;
                        self.ws_mut().unfocus_all();
                    }
                }
            }

            if primary_down {
                if let (Some(gesture), Some(pointer)) = (self.panel_gesture, pointer_pos) {
                    let pointer_delta = pointer - gesture.pointer_origin;
                    let other_rects = self.ws().panel_rects_except(gesture.panel_id);
                    let zoom = self.viewport.zoom;
                    let pointer_canvas = self.viewport.screen_to_canvas(pointer, canvas_rect);
                    if let Some(panel) = self
                        .ws_mut()
                        .panels
                        .iter_mut()
                        .find(|panel| panel.id() == gesture.panel_id)
                    {
                        guides = match gesture.kind {
                            PanelGestureKind::Drag { origin } => {
                                panel.set_drag_virtual_pos(Some(origin));
                                let guides =
                                    panel.drag_to(origin, pointer_delta, zoom, &other_rects);
                                let clamped = clamp_rect_to_desktop(panel.rect(), desktop_rect);
                                panel.apply_resize(clamped);
                                snap_preview_rect =
                                    desktop_snap_rect_for_pointer(pointer_canvas, desktop_rect);
                                guides
                            }
                            PanelGestureKind::Resize { handle, origin } => {
                                panel.set_resize_virtual_rect(Some(origin));
                                let guides = panel.resize_to(
                                    handle,
                                    origin,
                                    pointer_delta,
                                    zoom,
                                    &other_rects,
                                );
                                let clamped = clamp_rect_to_desktop(panel.rect(), desktop_rect);
                                panel.apply_resize(clamped);
                                guides
                            }
                        };
                    }
                }
            }

            if primary_released {
                if let Some(gesture) = self.panel_gesture.take() {
                    let release_snap = match (gesture.kind, pointer_pos) {
                        (PanelGestureKind::Drag { .. }, Some(pointer)) => {
                            let pointer_canvas =
                                self.viewport.screen_to_canvas(pointer, canvas_rect);
                            desktop_snap_rect_for_pointer(pointer_canvas, desktop_rect)
                        }
                        _ => None,
                    };
                    if let Some(panel) = self
                        .ws_mut()
                        .panels
                        .iter_mut()
                        .find(|panel| panel.id() == gesture.panel_id)
                    {
                        if let Some(snap_rect) = release_snap {
                            panel.apply_resize(snap_rect);
                        } else {
                            let clamped = clamp_rect_to_desktop(panel.rect(), desktop_rect);
                            panel.apply_resize(clamped);
                        }
                        panel.set_drag_virtual_pos(None);
                        panel.set_resize_virtual_rect(None);
                    }
                }
            }

            if primary_clicked {
                if let Some(hit) = hovered_hit {
                    if matches!(hit.area, PanelHitArea::CloseButton) {
                        let panel_id = self.ws().panels[hit.index].id();
                        self.ws_mut().close_panel(panel_id);
                        self.reconcile_orchestration();
                    } else if matches!(hit.area, PanelHitArea::MinimizeButton) {
                        let panel_id = self.ws().panels[hit.index].id();
                        self.ws_mut().toggle_minimize_panel(panel_id);
                    }
                }
            }

            if primary_double_clicked {
                if let Some(hit) = hovered_hit {
                    if matches!(hit.area, PanelHitArea::Body | PanelHitArea::TitleBar) {
                        let panel_id = self.ws().panels[hit.index].id();
                        self.ws_mut().bring_to_front(panel_id);
                    }
                }
            }

            let mut panel_order: Vec<_> = (0..self.ws().panels.len()).collect();
            panel_order.sort_by_key(|index| self.ws().panels[*index].z_index());

            for index in panel_order {
                if self.ws().panels[index].minimized() {
                    continue;
                }
                if !self
                    .viewport
                    .is_visible(self.ws().panels[index].rect(), canvas_rect)
                {
                    continue;
                }
                let viewport = self.viewport;
                let overlay = self
                    .orchestrator
                    .panel_overlay(self.ws().panels[index].id());
                let interaction = {
                    let panel = &mut self.ws_mut().panels[index];
                    panel.show(
                        ui,
                        &viewport,
                        canvas_rect,
                        fast_path_render,
                        overlay.as_ref(),
                    )
                };
                perf_snapshot.visible_panels += 1;
                perf_snapshot.note_render(interaction.render_tier, interaction.cache_hit);
                guides.extend(interaction.guides);
            }

            if let Some(snap_rect) = snap_preview_rect {
                let preview_screen = Rect::from_min_size(
                    self.viewport.canvas_to_screen(snap_rect.min, canvas_rect),
                    snap_rect.size() * self.viewport.zoom,
                );
                ui.painter().rect_filled(
                    preview_screen,
                    14.0,
                    Color32::from_rgba_premultiplied(86, 124, 255, 38),
                );
                ui.painter().rect_stroke(
                    preview_screen,
                    14.0,
                    Stroke::new(1.0, Color32::from_rgb(106, 146, 255)),
                );
            }

            for guide in guides {
                let [start, end] = guide_endpoints(guide);
                let start = self.viewport.canvas_to_screen(start, canvas_rect);
                let end = self.viewport.canvas_to_screen(end, canvas_rect);
                ui.painter()
                    .line_segment([start, end], egui::Stroke::new(1.0, SNAP_GUIDE_COLOR));
            }

            ui.painter().text(
                canvas_rect.left_bottom() + vec2(12.0, -10.0),
                Align2::LEFT_BOTTOM,
                format!("Ventanas {}", self.ws().panel_count()),
                FontId::proportional(11.0),
                Color32::from_rgb(115, 115, 115),
            );

            if needs_interaction_repaint {
                ui.ctx().request_repaint();
            }
        });

        if let Some(command) = self.command_palette.show(ctx) {
            self.execute_command(command, ctx, ctx.available_rect());
        }

        self.publish_collab_snapshot();
        self.show_share_workspace_dialog(ctx);
        self.show_join_session_dialog(ctx);
        self.show_launch_dialog(ctx);
        self.show_rename_dialog(ctx);
        self.maybe_persist_state(ctx);

        if runtime_repaint_now {
            ctx.request_repaint();
        }

        if let Some(delay) = self.repaint_policy.next_repaint_delay(Instant::now()) {
            ctx.request_repaint_after(delay.max(Duration::from_millis(1)));
        } else if self
            .ws()
            .panels
            .iter()
            .any(|panel| panel.focused() && panel.is_alive())
        {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
        perf_snapshot.frame_time = frame_started_at.elapsed();
        self.last_perf_snapshot = perf_snapshot;
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.collab.stop_session();
        save_state(&self.snapshot_state());
    }
}

fn load_brand_texture(cc: &eframe::CreationContext<'_>) -> Option<egui::TextureHandle> {
    let image = image::load_from_memory(include_bytes!("../assets/brand.png")).ok()?;
    let rgba = image.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let pixels = rgba.into_raw();
    Some(cc.egui_ctx.load_texture(
        "brand",
        egui::ColorImage::from_rgba_unmultiplied(size, &pixels),
        Default::default(),
    ))
}

#[derive(Clone, Copy)]
struct PanelHit {
    index: usize,
    area: PanelHitArea,
    pointer: Pos2,
}

pub(crate) fn gesture_pointer_pos(
    latest_pos: Option<Pos2>,
    interact_pos: Option<Pos2>,
    hover_pos: Option<Pos2>,
) -> Option<Pos2> {
    latest_pos.or(interact_pos).or(hover_pos)
}

fn host_terminal_input_pending(ctx: &egui::Context) -> bool {
    ctx.input(|input| {
        input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Text(_)
                    | egui::Event::Paste(_)
                    | egui::Event::Key { pressed: true, .. }
            )
        })
    })
}

fn collect_guest_terminal_input(ctx: &egui::Context) -> Vec<TerminalInputEvent> {
    ctx.input(|input| {
        input
            .events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Text(text) if !input.modifiers.ctrl && !input.modifiers.command => {
                    Some(TerminalInputEvent::Text(text.clone()))
                }
                egui::Event::Paste(text) => Some(TerminalInputEvent::Paste(text.clone())),
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => SerializableKey::from_egui(*key).map(|key| TerminalInputEvent::Key {
                    key,
                    modifiers: SerializableModifiers {
                        ctrl: modifiers.ctrl,
                        alt: modifiers.alt,
                        shift: modifiers.shift,
                        command: modifiers.command,
                    },
                }),
                _ => None,
            })
            .collect()
    })
}

fn panel_scroll_capture_active(
    hovered_panel: bool,
    smooth_scroll_delta: egui::Vec2,
    zoom_delta: f32,
    modifiers: egui::Modifiers,
) -> bool {
    hovered_panel
        && smooth_scroll_delta != egui::Vec2::ZERO
        && !panel_zoom_gesture_active(smooth_scroll_delta, zoom_delta, modifiers)
}

fn panel_zoom_gesture_active(
    smooth_scroll_delta: egui::Vec2,
    zoom_delta: f32,
    modifiers: egui::Modifiers,
) -> bool {
    (zoom_delta - 1.0).abs() > f32::EPSILON
        || ((modifiers.ctrl || modifiers.command) && smooth_scroll_delta != egui::Vec2::ZERO)
}

fn upsert_workspace_for_folder(workspaces: &mut Vec<Workspace>, path: PathBuf) -> usize {
    if let Some(index) = workspaces
        .iter()
        .position(|workspace| workspace.matches_cwd(&path))
    {
        return index;
    }

    workspaces.push(Workspace::from_folder(path));
    workspaces.len() - 1
}

fn overview_viewport_for_panels(
    panels: &[crate::panel::CanvasPanel],
    screen_rect: Rect,
    padding: f32,
    max_zoom: f32,
) -> Viewport {
    let Some(bounds) = panels
        .iter()
        .filter(|panel| !panel.minimized())
        .map(|panel| panel.rect())
        .reduce(|a, b| a.union(b))
    else {
        return Viewport::default();
    };

    Viewport::fit_rect(bounds.expand(48.0), screen_rect, padding, max_zoom)
}

fn interpolate_viewport(start: Viewport, target: Viewport, progress: f32) -> Viewport {
    let progress = progress.clamp(0.0, 1.0);
    Viewport {
        pan: start.pan + (target.pan - start.pan) * progress,
        zoom: start.zoom + (target.zoom - start.zoom) * progress,
    }
}

fn ease_in_out_cubic(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    if progress < 0.5 {
        4.0 * progress * progress * progress
    } else {
        1.0 - (-2.0 * progress + 2.0).powi(3) * 0.5
    }
}

fn default_guest_display_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "Guest".to_owned())
}

fn collab_state_badge(state: CollabSessionState) -> (&'static str, Color32) {
    match state {
        CollabSessionState::NotSharing => ("Not sharing", Color32::from_rgb(170, 170, 176)),
        CollabSessionState::Starting => ("Connecting", Color32::from_rgb(196, 162, 88)),
        CollabSessionState::Live => ("Live", Color32::from_rgb(110, 202, 144)),
        CollabSessionState::Disconnected => {
            ("Waiting for reconnect", Color32::from_rgb(239, 146, 68))
        }
        CollabSessionState::Ended => ("Ended", Color32::from_rgb(239, 68, 68)),
    }
}

fn draw_guest_session_banner(ui: &egui::Ui, canvas_rect: Rect, session_state: CollabSessionState) {
    let (title, body, color) = match session_state {
        CollabSessionState::Starting => (
            "Connecting to shared workspace",
            "Esperando la aprobación del host o la conexión inicial.",
            Color32::from_rgb(196, 162, 88),
        ),
        CollabSessionState::Disconnected => (
            "Host temporarily unavailable",
            "La sesión sigue abierta, pero el host perdió la conexión. Vamos a reintentar.",
            Color32::from_rgb(239, 146, 68),
        ),
        CollabSessionState::Ended => (
            "Shared session ended",
            "El host cerró la sesión o dejó de estar disponible.",
            Color32::from_rgb(239, 68, 68),
        ),
        _ => return,
    };

    let banner_rect = Rect::from_min_size(
        pos2(canvas_rect.center().x - 190.0, canvas_rect.top() + 20.0),
        vec2(380.0, 62.0),
    );
    let painter = ui.painter();
    painter.rect_filled(
        banner_rect,
        14.0,
        Color32::from_rgba_premultiplied(18, 19, 24, 232),
    );
    painter.rect_stroke(banner_rect, 14.0, Stroke::new(1.0, color));
    painter.text(
        banner_rect.left_top() + vec2(14.0, 12.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(16.0),
        color,
    );
    painter.text(
        banner_rect.left_top() + vec2(14.0, 34.0),
        Align2::LEFT_TOP,
        body,
        FontId::proportional(12.0),
        Color32::from_rgb(190, 190, 198),
    );
}

fn top_panel_hit(
    workspace: &Workspace,
    pointer: Pos2,
    viewport: &Viewport,
    canvas_rect: Rect,
) -> Option<PanelHit> {
    let mut panel_order: Vec<_> = (0..workspace.panels.len()).collect();
    panel_order.sort_by_key(|index| workspace.panels[*index].z_index());
    panel_order.reverse();

    panel_order.into_iter().find_map(|index| {
        if workspace.panels[index].minimized() {
            None
        } else {
            workspace.panels[index]
                .hit_test(pointer, viewport, canvas_rect)
                .map(|area| PanelHit {
                    index,
                    area,
                    pointer,
                })
        }
    })
}

fn top_panel_scroll_hit(
    workspace: &Workspace,
    pointer: Pos2,
    viewport: &Viewport,
    canvas_rect: Rect,
) -> Option<usize> {
    let mut panel_order: Vec<_> = (0..workspace.panels.len()).collect();
    panel_order.sort_by_key(|index| workspace.panels[*index].z_index());
    panel_order.reverse();

    panel_order
        .into_iter()
        .filter(|index| !workspace.panels[*index].minimized())
        .find(|index| workspace.panels[*index].scroll_hit_test(pointer, viewport, canvas_rect))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use egui::{pos2, vec2, CentralPanel, Color32, Modifiers, RawInput, Rect};
    use uuid::Uuid;

    use super::taskbar::desktop_taskbar_layout_rects;
    use super::{
        clamp_rect_to_desktop, desktop_canvas_rect, desktop_snap_rect_for_pointer,
        interpolate_viewport, overview_viewport_for_panels, panel_scroll_capture_active,
        taskbar_button_colors, taskbar_provider_accent, taskbar_provider_label,
        taskbar_summary_label, top_panel_hit, top_panel_scroll_hit, upsert_workspace_for_folder,
        TaskbarLayoutPreset,
    };
    use crate::canvas::config::{MINIMAP_BG, MINIMAP_HEIGHT, MINIMAP_PADDING, MINIMAP_WIDTH};
    use crate::canvas::minimap;
    use crate::canvas::viewport::Viewport;
    use crate::orchestration::AgentProvider;
    use crate::panel::CanvasPanel;
    use crate::state::Workspace;
    use crate::terminal::panel::{TerminalPanel, PANEL_BG};

    #[test]
    fn top_panel_hit_prefers_frontmost_panel() {
        let mut workspace = Workspace::new("Default", None);
        let mut back = TerminalPanel::new(pos2(0.0, 0.0), vec2(300.0, 200.0), Color32::WHITE, 0);
        back.z_index = 1;
        let mut front =
            TerminalPanel::new(pos2(20.0, 20.0), vec2(300.0, 200.0), Color32::LIGHT_BLUE, 1);
        front.z_index = 2;
        workspace.add_restored_terminal(back);
        workspace.add_restored_terminal(front);

        let hit = top_panel_hit(
            &workspace,
            pos2(50.0, 50.0),
            &Viewport::default(),
            Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)),
        )
        .unwrap();

        assert_eq!(hit.index, 1);
    }

    #[test]
    fn top_panel_scroll_hit_prefers_frontmost_terminal_body() {
        let mut workspace = Workspace::new("Default", None);
        let mut back = TerminalPanel::new(pos2(0.0, 0.0), vec2(300.0, 200.0), Color32::WHITE, 0);
        back.z_index = 1;
        let mut front =
            TerminalPanel::new(pos2(20.0, 20.0), vec2(300.0, 200.0), Color32::LIGHT_BLUE, 1);
        front.z_index = 2;
        workspace.add_restored_terminal(back);
        workspace.add_restored_terminal(front);

        let hit = top_panel_scroll_hit(
            &workspace,
            pos2(80.0, 120.0),
            &Viewport::default(),
            Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)),
        );

        assert_eq!(hit, Some(1));
    }

    #[test]
    fn top_panel_hit_ignores_minimized_panels() {
        let mut workspace = Workspace::new("Default", None);
        let back = TerminalPanel::new(pos2(0.0, 0.0), vec2(300.0, 200.0), Color32::WHITE, 0);
        let front =
            TerminalPanel::new(pos2(20.0, 20.0), vec2(300.0, 200.0), Color32::LIGHT_BLUE, 1);
        let front_id = front.id;
        workspace.add_restored_terminal(back);
        workspace.add_restored_terminal(front);
        workspace.bring_to_front(front_id);
        workspace.toggle_minimize_panel(front_id);

        let hit = top_panel_hit(
            &workspace,
            pos2(50.0, 50.0),
            &Viewport::default(),
            Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)),
        )
        .unwrap();

        assert_eq!(hit.index, 0);
    }

    #[test]
    fn taskbar_summary_reports_total_and_minimized_windows() {
        assert_eq!(taskbar_summary_label(5, 2), "5 abiertas · 2 minimizadas");
    }

    #[test]
    fn taskbar_provider_prefers_overlay_metadata() {
        let provider = taskbar_provider_label(Some(AgentProvider::CodexCli), None, "Claude Code");

        assert_eq!(provider, AgentProvider::CodexCli);
    }

    #[test]
    fn taskbar_provider_falls_back_to_title_detection() {
        let provider = taskbar_provider_label(None, None, "OpenCode session");

        assert_eq!(provider, AgentProvider::OpenCode);
    }

    #[test]
    fn taskbar_provider_uses_panel_hint_before_title_detection() {
        let provider = taskbar_provider_label(None, Some(AgentProvider::ClaudeCode), "Terminal");

        assert_eq!(provider, AgentProvider::ClaudeCode);
    }

    #[test]
    fn taskbar_colors_use_blue_family_for_codex() {
        let (fill, stroke, text) = taskbar_button_colors(AgentProvider::CodexCli, true, false);

        assert_eq!(fill, Color32::from_rgb(38, 82, 156));
        assert_eq!(stroke, Color32::from_rgb(126, 184, 255));
        assert_eq!(text, Color32::from_rgb(232, 242, 255));
    }

    #[test]
    fn taskbar_colors_use_orange_family_for_claude() {
        let (fill, stroke, text) = taskbar_button_colors(AgentProvider::ClaudeCode, false, false);

        assert_eq!(fill, Color32::from_rgb(94, 54, 18));
        assert_eq!(stroke, Color32::from_rgb(222, 142, 62));
        assert_eq!(text, Color32::from_rgb(255, 238, 216));
    }

    #[test]
    fn taskbar_colors_use_gray_family_for_opencode_minimized() {
        let (fill, stroke, text) = taskbar_button_colors(AgentProvider::OpenCode, false, true);

        assert_eq!(fill, Color32::from_rgb(36, 36, 42));
        assert_eq!(stroke, Color32::from_rgb(124, 124, 132));
        assert_eq!(text, Color32::from_rgb(182, 182, 188));
    }

    #[test]
    fn taskbar_accent_is_bright_for_codex() {
        assert_eq!(
            taskbar_provider_accent(AgentProvider::CodexCli),
            Color32::from_rgb(120, 190, 255)
        );
    }

    #[test]
    fn side_by_side_layout_splits_visible_windows_evenly() {
        let desktop = Rect::from_min_max(pos2(0.0, 0.0), pos2(1200.0, 800.0));

        let rects = desktop_taskbar_layout_rects(TaskbarLayoutPreset::SideBySide, 2, desktop);

        assert_eq!(rects.len(), 2);
        assert!(approx_rect(
            rects[0],
            Rect::from_min_max(pos2(0.0, 0.0), pos2(600.0, 800.0))
        ));
        assert!(approx_rect(
            rects[1],
            Rect::from_min_max(pos2(600.0, 0.0), pos2(1200.0, 800.0))
        ));
    }

    #[test]
    fn grid_layout_places_three_windows_in_balanced_cells() {
        let desktop = Rect::from_min_max(pos2(0.0, 0.0), pos2(1200.0, 800.0));

        let rects = desktop_taskbar_layout_rects(TaskbarLayoutPreset::Grid, 3, desktop);

        assert_eq!(rects.len(), 3);
        assert!(approx_rect(
            rects[0],
            Rect::from_min_max(pos2(0.0, 0.0), pos2(600.0, 400.0))
        ));
        assert!(approx_rect(
            rects[1],
            Rect::from_min_max(pos2(600.0, 0.0), pos2(1200.0, 400.0))
        ));
        assert!(approx_rect(
            rects[2],
            Rect::from_min_max(pos2(0.0, 400.0), pos2(600.0, 800.0))
        ));
    }

    #[test]
    fn cascade_layout_offsets_each_window_and_stays_inside_desktop() {
        let desktop = Rect::from_min_max(pos2(0.0, 0.0), pos2(1200.0, 800.0));

        let rects = desktop_taskbar_layout_rects(TaskbarLayoutPreset::Cascade, 3, desktop);

        assert_eq!(rects.len(), 3);
        assert!(desktop.contains_rect(rects[0]));
        assert!(desktop.contains_rect(rects[1]));
        assert!(desktop.contains_rect(rects[2]));
        assert!(rects[1].min.x > rects[0].min.x);
        assert!(rects[1].min.y > rects[0].min.y);
        assert!(rects[2].min.x > rects[1].min.x);
        assert!(rects[2].min.y > rects[1].min.y);
    }

    #[test]
    fn desktop_canvas_rect_reaches_platform_edges() {
        let canvas_rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 720.0));

        let desktop = desktop_canvas_rect(canvas_rect);

        assert_eq!(desktop.min, pos2(0.0, 0.0));
        assert_eq!(desktop.max, pos2(1280.0, 720.0));
    }

    #[test]
    fn clamp_rect_to_desktop_keeps_window_inside_bounds() {
        let desktop = Rect::from_min_max(pos2(16.0, 16.0), pos2(1016.0, 716.0));
        let rect = Rect::from_min_size(pos2(900.0, 660.0), vec2(320.0, 220.0));

        let clamped = clamp_rect_to_desktop(rect, desktop);

        assert!(desktop.contains_rect(clamped));
    }

    #[test]
    fn snap_rect_for_pointer_uses_left_half_on_left_edge() {
        let desktop = Rect::from_min_max(pos2(16.0, 16.0), pos2(1016.0, 716.0));

        let snap = desktop_snap_rect_for_pointer(pos2(18.0, 320.0), desktop).unwrap();

        assert_eq!(snap.left(), desktop.left());
        assert_eq!(snap.top(), desktop.top());
        assert_eq!(snap.bottom(), desktop.bottom());
        assert!((snap.width() - desktop.width() * 0.5).abs() < 0.001);
    }

    #[test]
    fn snap_rect_for_pointer_uses_top_right_quadrant_on_corner() {
        let desktop = Rect::from_min_max(pos2(16.0, 16.0), pos2(1016.0, 716.0));

        let snap = desktop_snap_rect_for_pointer(pos2(1014.0, 18.0), desktop).unwrap();

        assert_eq!(snap.right(), desktop.right());
        assert_eq!(snap.top(), desktop.top());
        assert!((snap.width() - desktop.width() * 0.5).abs() < 0.001);
        assert!((snap.height() - desktop.height() * 0.5).abs() < 0.001);
    }

    #[test]
    fn zoom_scroll_over_panel_does_not_get_captured_as_terminal_scroll() {
        assert!(!panel_scroll_capture_active(
            true,
            vec2(0.0, 120.0),
            1.0,
            Modifiers {
                command: true,
                ..Default::default()
            },
        ));
    }

    #[test]
    fn plain_scroll_over_panel_stays_captured_by_terminal() {
        assert!(panel_scroll_capture_active(
            true,
            vec2(0.0, 120.0),
            1.0,
            Modifiers::default(),
        ));
    }

    #[test]
    fn minimap_paints_above_overlapping_panels() {
        let ctx = egui::Context::default();
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 700.0))),
            ..Default::default()
        };
        let viewport = Viewport::default();
        let panel_pos = pos2(560.0, 420.0);
        let panel_size = vec2(280.0, 220.0);
        let mut canvas_rect = Rect::NOTHING;

        let output = ctx.run(raw_input, |ctx| {
            CentralPanel::default().show(ctx, |ui| {
                canvas_rect = ui.max_rect();

                let mut drawn_panel =
                    TerminalPanel::new(panel_pos, panel_size, Color32::LIGHT_BLUE, 1);
                drawn_panel.show(ui, &viewport, canvas_rect, false, None);

                let minimap_panels = [CanvasPanel::Terminal(TerminalPanel::new(
                    panel_pos,
                    panel_size,
                    Color32::LIGHT_BLUE,
                    1,
                ))];
                minimap::show(ui, &minimap_panels, &viewport, canvas_rect);
            });
        });

        let panel_rect = Rect::from_min_size(
            canvas_rect.min + panel_pos.to_vec2(),
            panel_size * viewport.zoom,
        );
        let minimap_rect = Rect::from_min_size(
            pos2(
                canvas_rect.right() - MINIMAP_WIDTH - MINIMAP_PADDING,
                canvas_rect.bottom() - MINIMAP_HEIGHT - MINIMAP_PADDING,
            ),
            vec2(MINIMAP_WIDTH, MINIMAP_HEIGHT),
        );

        let panel_bg_idx = last_rect_shape_index(&output.shapes, panel_rect, PANEL_BG)
            .expect("expected panel background shape");
        let minimap_bg_idx = last_rect_shape_index(&output.shapes, minimap_rect, MINIMAP_BG)
            .expect("expected minimap background shape");

        assert!(
            minimap_bg_idx > panel_bg_idx,
            "minimap should paint after overlapping panels, got panel idx {panel_bg_idx} and minimap idx {minimap_bg_idx}"
        );
    }

    #[test]
    fn upsert_workspace_for_folder_reuses_existing_workspace() {
        let path = unique_temp_dir("existing-folder");
        let mut workspaces = vec![Workspace::from_folder(path.clone())];

        let index = upsert_workspace_for_folder(&mut workspaces, path);

        assert_eq!(index, 0);
        assert_eq!(workspaces.len(), 1);
    }

    #[test]
    fn upsert_workspace_for_folder_creates_workspace_for_new_folder() {
        let first = unique_temp_dir("first-folder");
        let second = unique_temp_dir("second-folder");
        let mut workspaces = vec![Workspace::from_folder(first)];

        let index = upsert_workspace_for_folder(&mut workspaces, second.clone());

        assert_eq!(index, 1);
        assert_eq!(workspaces.len(), 2);
        assert_eq!(workspaces[index].cwd(), Some(second.as_path()));
    }

    #[test]
    fn overview_viewport_contains_all_panels() {
        let panels = vec![
            CanvasPanel::Terminal(TerminalPanel::new(
                pos2(-320.0, -160.0),
                vec2(640.0, 420.0),
                Color32::WHITE,
                0,
            )),
            CanvasPanel::Terminal(TerminalPanel::new(
                pos2(980.0, 760.0),
                vec2(760.0, 460.0),
                Color32::LIGHT_BLUE,
                1,
            )),
        ];
        let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 820.0));

        let overview = overview_viewport_for_panels(&panels, screen, 84.0, 1.0);
        let visible = overview.visible_canvas_rect(screen);
        let bounds = panels
            .iter()
            .map(CanvasPanel::rect)
            .reduce(|a, b| a.union(b))
            .unwrap()
            .expand(48.0);

        assert!(visible.contains_rect(bounds));
        assert!(overview.zoom <= 1.0);
    }

    #[test]
    fn overview_viewport_defaults_when_no_panels_exist() {
        let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 820.0));

        let overview = overview_viewport_for_panels(&[], screen, 84.0, 1.0);

        assert_eq!(overview.pan, egui::Vec2::ZERO);
        assert_eq!(overview.zoom, 1.0);
    }

    #[test]
    fn interpolate_viewport_reaches_target_at_completion() {
        let start = Viewport {
            pan: vec2(-120.0, 40.0),
            zoom: 0.7,
        };
        let target = Viewport {
            pan: vec2(280.0, -160.0),
            zoom: 1.8,
        };

        let interpolated = interpolate_viewport(start, target, 1.0);

        assert_eq!(interpolated.pan, target.pan);
        assert_eq!(interpolated.zoom, target.zoom);
    }

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn last_rect_shape_index(
        shapes: &[egui::epaint::ClippedShape],
        expected_rect: Rect,
        expected_fill: Color32,
    ) -> Option<usize> {
        shapes
            .iter()
            .enumerate()
            .filter_map(|(index, clipped)| match &clipped.shape {
                egui::epaint::Shape::Rect(rect_shape)
                    if rect_shape.fill == expected_fill
                        && approx_rect(rect_shape.rect, expected_rect) =>
                {
                    Some(index)
                }
                _ => None,
            })
            .last()
    }

    fn approx_rect(a: Rect, b: Rect) -> bool {
        approx_eq(a.min.x, b.min.x)
            && approx_eq(a.min.y, b.min.y)
            && approx_eq(a.max.x, b.max.x)
            && approx_eq(a.max.y, b.max.y)
    }

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() <= 0.5
    }
}
