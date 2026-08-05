use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{
    pos2, vec2, Area, CentralPanel, Color32, Id, Key, Order, Pos2, Rect, SidePanel, Stroke,
    TopBottomPanel,
};
use uuid::Uuid;

use crate::canvas::config::{CANVAS_BG, ZOOM_KEYBOARD_FACTOR};
use crate::canvas::viewport::Viewport;
use crate::collab::{
    CollabManager, CollabMode, CollabSessionState, PanelShareScope, TrustedDevice,
};
use crate::command_palette::commands::Command;
use crate::command_palette::CommandPalette;
use crate::orchestration::{AgentProvider, Orchestrator, WorktreeMode};
use crate::runtime::RenderTier;
use crate::shortcuts::shortcut_command;
use crate::sidebar::{Sidebar, SidebarResponse};
use crate::state::persistence::{AutosaveController, AutosaveDecision};
use crate::state::{load_state, save_state, AppState, Workspace};
use crate::theme::colors as palette;
use crate::theme::fonts::setup_fonts;
use crate::update::{RepaintPolicy, UpdateChecker};
use crate::utils::platform::home_dir;

mod broadcast_ui;
mod code_highlight;
mod code_review_ui;
mod collab_ui;
mod desktop;
mod dialogs;
mod export_action;
mod file_viewer_ui;
mod orchestration_ui;
mod perf;
mod quick_open_ui;
mod settings_ui;
mod taskbar;
#[cfg(test)]
mod tests;
mod toast;
mod windowing;

use self::code_review_ui::CodeReviewState;
use self::collab_ui::{
    default_guest_display_name, host_terminal_input_pending, JoinSessionDraft, ShareWorkspaceDraft,
};
#[cfg(test)]
use self::desktop::{interpolate_viewport, overview_viewport_for_panels};
use self::desktop::{
    panel_scroll_capture_active, split_resize_hit, top_panel_hit, top_panel_scroll_hit,
    upsert_workspace_for_folder, SplitResizeAxis,
};
use self::orchestration_ui::{LaunchAgentDraft, ORCHESTRATION_REFRESH_INTERVAL};
use self::perf::FramePerfSnapshot;
use self::quick_open_ui::QuickOpenState;
use self::taskbar::{clamp_workspace_panels_to_desktop, desktop_canvas_rect, desktop_screen_rect};

use self::windowing::{
    panel_id_for_hit, panel_id_for_index, split_resize_panel_ids, GlobalSashDrag, PanelGesture,
    WindowTransition, WindowTransitionKind,
};

const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(2);
const RUNTIME_REPAINT_BATCH: Duration = Duration::from_millis(33);
const VIEWPORT_FOCUS_PADDING: f32 = 72.0;
const VIEWPORT_FOCUS_MAX_ZOOM: f32 = 2.0;
const DESKTOP_MARGIN: f32 = 0.0;
const DESKTOP_SNAP_EDGE: f32 = 28.0;

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
    search_open: bool,
    search_buf: String,
    search_panel_id: Option<Uuid>,
    code_review: Option<CodeReviewState>,
    diff_loader: crate::orchestration::DiffLoader,
    quick_open: Option<QuickOpenState>,
    quick_open_rx: Option<std::sync::mpsc::Receiver<Vec<String>>>,
    file_viewer: Option<file_viewer_ui::FileViewerState>,
    settings_open: bool,
    settings_draft: Option<settings_ui::SettingsDraft>,
    broadcast: Option<broadcast_ui::BroadcastState>,
    file_tree: crate::sidebar::file_tree::FileTreeState,
    /// Paneles cuyo scrollback persistido ya se reinyectó en esta corrida.
    scrollback_restored: HashSet<Uuid>,
    highlighter: code_highlight::Highlighter,
    toasts: toast::Toasts,
    /// Último estado de agente por sesión que vimos, para notificar solo en la
    /// transición hacia un estado de atención (no repetirlo cada refresh).
    agent_status_seen: HashMap<Uuid, crate::orchestration::AgentStatus>,
    brand_texture: Option<egui::TextureHandle>,
    sidebar: Sidebar,
    update_checker: UpdateChecker,
    fullscreen: bool,
    panel_gesture: Option<PanelGesture>,
    global_sash_drag: Option<GlobalSashDrag>,
    autosave: AutosaveController,
    persisted_state: Option<AppState>,
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
    taskbar_button_rects: HashMap<Uuid, Rect>,
    window_transitions: HashMap<Uuid, WindowTransition>,
    consecutive_update_panics: u32,
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
                show_grid: saved.legacy_canvas_ui.show_grid,
                show_minimap: saved.legacy_canvas_ui.show_minimap,
                ctx: Some(cc.egui_ctx.clone()),
                command_palette: CommandPalette::default(),
                renaming_panel: None,
                rename_buf: String::new(),
                search_open: false,
                search_buf: String::new(),
                search_panel_id: None,
                code_review: None,
                diff_loader: crate::orchestration::DiffLoader::default(),
                quick_open: None,
                quick_open_rx: None,
                file_viewer: None,
                settings_open: false,
                settings_draft: None,
                broadcast: None,
                file_tree: Default::default(),
                scrollback_restored: HashSet::new(),
                highlighter: code_highlight::Highlighter::new(),
                toasts: Default::default(),
                agent_status_seen: HashMap::new(),
                brand_texture,
                sidebar: Sidebar::default(),
                update_checker,
                fullscreen: false,
                panel_gesture: None,
                global_sash_drag: None,
                autosave: AutosaveController::new(AUTOSAVE_INTERVAL),
                persisted_state: None,
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
                    submitting: false,
                },
                local_device_id,
                trusted_devices,
                last_orchestration_refresh: Instant::now()
                    .checked_sub(ORCHESTRATION_REFRESH_INTERVAL)
                    .unwrap_or_else(Instant::now),
                last_orchestration_scan_duration: Duration::ZERO,
                last_perf_snapshot: FramePerfSnapshot::default(),
                layout_menu_open: false,
                taskbar_button_rects: HashMap::new(),
                window_transitions: HashMap::new(),
                consecutive_update_panics: 0,
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
                search_open: false,
                search_buf: String::new(),
                search_panel_id: None,
                code_review: None,
                diff_loader: crate::orchestration::DiffLoader::default(),
                quick_open: None,
                quick_open_rx: None,
                file_viewer: None,
                settings_open: false,
                settings_draft: None,
                broadcast: None,
                file_tree: Default::default(),
                scrollback_restored: HashSet::new(),
                highlighter: code_highlight::Highlighter::new(),
                toasts: Default::default(),
                agent_status_seen: HashMap::new(),
                brand_texture,
                sidebar: Sidebar::default(),
                update_checker,
                fullscreen: false,
                panel_gesture: None,
                global_sash_drag: None,
                autosave: AutosaveController::new(AUTOSAVE_INTERVAL),
                persisted_state: None,
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
                    submitting: false,
                },
                local_device_id: Uuid::new_v4().to_string(),
                trusted_devices: HashMap::new(),
                last_orchestration_refresh: Instant::now()
                    .checked_sub(ORCHESTRATION_REFRESH_INTERVAL)
                    .unwrap_or_else(Instant::now),
                last_orchestration_scan_duration: Duration::ZERO,
                last_perf_snapshot: FramePerfSnapshot::default(),
                layout_menu_open: false,
                taskbar_button_rects: HashMap::new(),
                window_transitions: HashMap::new(),
                consecutive_update_panics: 0,
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
                    saved.legacy_canvas.viewport_pan = [self.viewport.pan.x, self.viewport.pan.y];
                    saved.legacy_canvas.viewport_zoom = self.viewport.zoom;
                }
                saved
            })
            .collect();

        AppState {
            schema_version: 2,
            workspaces,
            active_ws: self.active_ws,
            sidebar_visible: self.sidebar_visible,
            legacy_canvas_ui: crate::state::persistence::LegacyCanvasUiState {
                show_grid: self.show_grid,
                show_minimap: self.show_minimap,
            },
            local_device_id: self.local_device_id.clone(),
            trusted_devices: self.trusted_devices_snapshot(),
            orchestration: self.orchestrator.snapshot(),
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
                if let Some(canvas_rect) = canvas_rect {
                    let desktop_rect = desktop_canvas_rect(canvas_rect);
                    self.ws_mut()
                        .restore_panel_with_desktop(panel_id, desktop_rect);
                } else {
                    self.ws_mut().restore_panel(panel_id);
                }
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
                    | Command::ZoomIn
                    | Command::ZoomOut
                    | Command::ResetZoom
                    | Command::ToggleFullscreen
            )
        {
            return;
        }
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
            Command::SearchTerminal => self.open_search_bar(),
            Command::ReviewChanges => self.open_code_review(),
            Command::QuickOpen => self.open_quick_open(),
            Command::OpenSettings => self.open_settings(),
            Command::ExportScrollback => self.export_focused_scrollback(),
            Command::BroadcastCommand => self.open_broadcast(),
            Command::SharePanelPrivate => {
                self.set_focused_panel_share_scope(PanelShareScope::Private)
            }
            Command::SharePanelVisibleOnly => {
                self.set_focused_panel_share_scope(PanelShareScope::VisibleOnly)
            }
            Command::SharePanelVisibleAndHistory => {
                self.set_focused_panel_share_scope(PanelShareScope::VisibleAndHistory)
            }
            Command::SharePanelControllable => {
                self.set_focused_panel_share_scope(PanelShareScope::Controllable)
            }
            Command::FocusNext => self.focus_relative(1),
            Command::FocusPrev => self.focus_relative(-1),
            Command::ZoomToFitAll => self.zoom_to_fit_all(canvas_rect),
            Command::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
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
                SidebarResponse::ReviewPanelChanges(panel_id) => {
                    self.focus_panel_across_workspaces(panel_id, Some(ctx.available_rect()));
                    self.open_code_review();
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
                SidebarResponse::OpenSettings => self.open_settings(),
                SidebarResponse::OpenBroadcast => self.open_broadcast(),
                SidebarResponse::ExportScrollback => self.export_focused_scrollback(),
                SidebarResponse::OpenFileInViewer(path) => self.open_file_viewer(path),
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
                        // El scrollback va junto al layout: si guardamos uno sin
                        // el otro, al restaurar el historial no matchea.
                        self.persist_scrollbacks();
                    }
                    Err(err) => {
                        log::warn!("Autosave failed: {err}");
                        ctx.request_repaint_after(AUTOSAVE_INTERVAL);
                    }
                }
            }
        }
    }
}

impl TerminalApp {
    fn update_impl(&mut self, ctx: &egui::Context) {
        let frame_started_at = Instant::now();
        let mut perf_snapshot = FramePerfSnapshot::default();
        self.begin_frame(ctx);
        self.pump_runtime_updates(&mut perf_snapshot);
        self.forward_input_to_focused_panel(ctx);
        self.show_sidebar(ctx);
        self.show_taskbar(ctx);
        // El visor de código es un SidePanel: tiene que declararse antes del
        // CentralPanel para que el canvas se achique en vez de quedar tapado.
        self.show_file_viewer(ctx);

        CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(CANVAS_BG)
                    .inner_margin(egui::Margin::same(0.0))
                    .outer_margin(egui::Margin::same(0.0)),
            )
            .show(ctx, |ui| {
                let canvas_rect = ui.max_rect();
                ui.painter().rect_filled(canvas_rect, 0.0, CANVAS_BG);

                if matches!(self.collab.mode(), CollabMode::Guest) {
                    self.show_guest_canvas(ui, ctx, canvas_rect);
                } else {
                    self.show_desktop_canvas(ui, ctx, canvas_rect, &mut perf_snapshot);
                }
            });

        self.finish_frame(ctx, frame_started_at, perf_snapshot);
    }

    /// Fase 1: estado por frame — eventos de collab, transiciones de ventana,
    /// refresh de orquestación y atajos globales.
    fn begin_frame(&mut self, ctx: &egui::Context) {
        self.ctx = Some(ctx.clone());
        self.handle_collab_events();
        self.sync_window_transitions(ctx);
        self.maybe_refresh_orchestration();
        self.poll_diff_loader();
        self.poll_quick_open();
        if self.code_review.as_ref().is_some_and(|state| state.loading) {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }

        if let Some(command) = self.handle_shortcuts(ctx) {
            let canvas_rect = ctx.available_rect();
            self.execute_command(command, ctx, canvas_rect);
        }
    }

    /// Fase 2: drena la salida de los PTYs, alimenta la política de repintado
    /// y registra los contadores de sesiones del frame.
    fn pump_runtime_updates(&mut self, perf_snapshot: &mut FramePerfSnapshot) {
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
    }

    /// Fase 3: teclado hacia la terminal enfocada, salvo que un diálogo, la
    /// paleta o el modo guest lo capturen.
    fn forward_input_to_focused_panel(&mut self, ctx: &egui::Context) {
        if !self.command_palette.open
            && self.renaming_panel.is_none()
            && !self.search_open
            && self.code_review.is_none()
            && self.quick_open.is_none()
            && self.file_viewer.is_none()
            && !self.settings_open
            && self.broadcast.is_none()
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
    }

    fn show_sidebar(&mut self, ctx: &egui::Context) {
        if self.sidebar_visible && !matches!(self.collab.mode(), CollabMode::Guest) {
            SidePanel::left("sidebar")
                .resizable(true)
                .default_width(220.0)
                .min_width(180.0)
                .max_width(320.0)
                .frame(
                    egui::Frame::none()
                        .fill(crate::theme::colors::INK)
                        .inner_margin(egui::Margin::same(0.0))
                        .outer_margin(egui::Margin::same(0.0)),
                )
                .show_separator_line(false)
                .show(ctx, |ui| {
                    let state = self.update_checker.snapshot();
                    let attention = self.attention_items();
                    // El explorador sigue la carpeta del workspace activo.
                    let ws_root = self.workspaces[self.active_ws].cwd.clone();
                    self.file_tree.set_root(ws_root);
                    let responses = self.sidebar.show(
                        ui,
                        self.brand_texture.as_ref(),
                        &self.workspaces,
                        self.active_ws,
                        &state,
                        self.collab.mode(),
                        self.collab.session_state(),
                        &attention,
                        &mut self.file_tree,
                    );
                    self.handle_sidebar_responses(responses, ctx);
                });
        }
    }

    /// Sesiones de agente del workspace activo que piden atención, para la
    /// sección "Atención" del sidebar.
    /// Guarda el scrollback de cada panel vivo de todos los workspaces y borra
    /// los archivos de paneles que ya no existen.
    fn persist_scrollbacks(&mut self) {
        let Some(dir) = crate::state::scrollback_store::scrollback_dir() else {
            return;
        };
        let mut live_ids = Vec::new();
        for workspace in &self.workspaces {
            for panel in &workspace.panels {
                live_ids.push(panel.id());
                // Un panel detached no tiene texto que leer; su archivo previo
                // se conserva tal cual (es justo el historial a restaurar).
                if let Some(text) = panel.scrollback_text() {
                    if let Err(err) =
                        crate::state::scrollback_store::save_scrollback(&dir, panel.id(), &text)
                    {
                        log::warn!("No se pudo guardar el scrollback del panel: {err}");
                    }
                }
            }
        }
        crate::state::scrollback_store::prune_scrollback(&dir, &live_ids);
    }

    /// Reinyecta el historial guardado en los paneles que acaban de conseguir
    /// terminal. Cada panel se restaura una sola vez por corrida.
    fn restore_pending_scrollbacks(&mut self) {
        if self.scrollback_restored.len() == self.total_panel_count() {
            return;
        }
        let Some(dir) = crate::state::scrollback_store::scrollback_dir() else {
            return;
        };
        let pending: Vec<Uuid> = self
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.panels.iter())
            .filter(|panel| !self.scrollback_restored.contains(&panel.id()))
            .map(|panel| panel.id())
            .collect();

        for panel_id in pending {
            let Some(text) = crate::state::scrollback_store::load_scrollback(&dir, panel_id) else {
                // Sin historial guardado: no hay nada que reintentar después.
                self.scrollback_restored.insert(panel_id);
                continue;
            };
            let restored = self
                .workspaces
                .iter_mut()
                .flat_map(|workspace| workspace.panels.iter_mut())
                .find(|panel| panel.id() == panel_id)
                .map(|panel| panel.restore_history(&text))
                .unwrap_or(true);
            // Si todavía está detached, se reintenta en un frame posterior.
            if restored {
                self.scrollback_restored.insert(panel_id);
            }
        }
    }

    fn total_panel_count(&self) -> usize {
        self.workspaces
            .iter()
            .map(|workspace| workspace.panels.len())
            .sum()
    }

    fn attention_items(&self) -> Vec<crate::sidebar::AttentionItem> {
        let workspace_id = self.ws().id;
        self.orchestrator
            .sessions()
            .iter()
            .filter(|session| session.workspace_id == workspace_id)
            .filter(|session| session.status.is_attention())
            .filter_map(|session| {
                let panel_id = session.panel_id?;
                Some(crate::sidebar::AttentionItem {
                    panel_id,
                    label: session.label.clone(),
                    provider: session.provider.label(),
                    status: session.status.label(),
                })
            })
            .collect()
    }

    /// Escritorio host: input de puntero, sash global, gestos de ventana,
    /// render de paneles y overlays.
    fn show_desktop_canvas(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        canvas_rect: Rect,
        perf_snapshot: &mut FramePerfSnapshot,
    ) {
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
        let desktop_rect = desktop_canvas_rect(canvas_rect);
        let desktop_screen = desktop_screen_rect(canvas_rect, desktop_rect);
        clamp_workspace_panels_to_desktop(self.ws_mut(), desktop_rect);
        ui.painter()
            .rect_filled(desktop_screen, 0.0, palette::SURFACE);
        ui.painter()
            .rect_stroke(desktop_screen, 0.0, Stroke::new(0.0, palette::LINE));
        let pointer_pos = gesture_pointer_pos(latest_pos, interact_pos, hover_pos);
        let split_hit: Option<desktop::SplitResizeHit> = None;
        let _ = split_resize_hit;
        let _ = split_resize_panel_ids;

        let sash_active = self.update_global_sash(
            ctx,
            desktop_screen,
            pointer_pos,
            primary_pressed,
            primary_released,
            primary_down,
        );

        let hovered_hit = pointer_pos
            .filter(|pos| desktop_screen.contains(*pos))
            .filter(|_| !sash_active)
            .and_then(|pos| top_panel_hit(self.ws(), pos, &self.viewport, canvas_rect));
        let scroll_target = pointer_pos
            .filter(|pos| desktop_screen.contains(*pos))
            .and_then(|pos| top_panel_scroll_hit(self.ws(), pos, &self.viewport, canvas_rect));
        let hovered_hit = hovered_hit.filter(|hit| {
            panel_id_for_hit(self.ws(), hit)
                .map(|panel_id| !self.is_panel_transitioning(panel_id))
                .unwrap_or(false)
        });
        let scroll_target = scroll_target.filter(|index| {
            panel_id_for_index(self.ws(), *index)
                .map(|panel_id| !self.is_panel_transitioning(panel_id))
                .unwrap_or(false)
        });
        let hovered_panel = split_hit.is_none() && hovered_hit.is_some();
        let scroll_capture_active =
            panel_scroll_capture_active(hovered_panel, smooth_scroll_delta, zoom_delta, modifiers);
        if let Some(split_hit) = split_hit {
            ctx.output_mut(|output| {
                output.cursor_icon = match split_hit.axis {
                    SplitResizeAxis::Vertical => egui::CursorIcon::ResizeHorizontal,
                    SplitResizeAxis::Horizontal => egui::CursorIcon::ResizeVertical,
                };
            });
        }

        if scroll_capture_active {
            if let (Some(index), scroll_y) = (scroll_target, smooth_scroll_delta.y) {
                if scroll_y != 0.0 {
                    if let Some(panel_id) = panel_id_for_index(self.ws(), index) {
                        let viewport = self.viewport;
                        if matches!(self.collab.mode(), CollabMode::Host)
                            && self.collab.controller_for(panel_id).is_some()
                        {
                            self.collab.revoke_control(panel_id, "Host took control");
                        }
                        if let Some(panel) = self
                            .ws_mut()
                            .panels
                            .iter_mut()
                            .find(|panel| panel.id() == panel_id)
                        {
                            panel.handle_scroll(scroll_y, pointer_pos, &viewport, canvas_rect, ctx);
                        }
                    }
                }
            }
        }

        let mut guides = Vec::new();
        let mut snap_preview_rect = None;
        let mut split_preview_rect = split_hit.map(|hit| hit.hit_rect);
        let fast_path_render = self.panel_gesture.is_some();
        let needs_interaction_repaint = scroll_capture_active || self.panel_gesture.is_some();
        if primary_pressed {
            self.begin_panel_gesture(split_hit, hovered_hit, desktop_rect, pointer_pos);
        }

        if primary_down {
            guides = self.drive_panel_gesture(
                canvas_rect,
                desktop_rect,
                pointer_pos,
                &mut snap_preview_rect,
                &mut split_preview_rect,
            );
        }

        if primary_released {
            self.finish_panel_gesture(canvas_rect, desktop_rect, pointer_pos);
        }

        self.handle_panel_clicks(
            ctx,
            canvas_rect,
            desktop_rect,
            hovered_hit,
            primary_clicked,
            primary_double_clicked,
        );

        let mut panel_order: Vec<_> = (0..self.ws().panels.len()).collect();
        panel_order.sort_by_key(|index| self.ws().panels[*index].z_index());

        for index in panel_order {
            if self.ws().panels[index].minimized() {
                continue;
            }
            if self.is_panel_transitioning(self.ws().panels[index].id()) {
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

        self.draw_desktop_overlays(
            ui,
            canvas_rect,
            snap_preview_rect,
            split_preview_rect,
            guides,
        );

        if needs_interaction_repaint {
            ui.ctx().request_repaint();
        }
    }

    /// Fase final: paleta de comandos, diálogos, autosave y programación del
    /// próximo repintado.
    fn finish_frame(
        &mut self,
        ctx: &egui::Context,
        frame_started_at: Instant,
        mut perf_snapshot: FramePerfSnapshot,
    ) {
        if let Some(command) = self.command_palette.show(ctx) {
            self.execute_command(command, ctx, ctx.available_rect());
        }

        self.publish_collab_snapshot();
        self.poll_join_session_result();
        self.poll_pending_launches(ctx);
        self.show_share_workspace_dialog(ctx);
        self.show_join_session_dialog(ctx);
        // Los eventos de collab (transporte y worker HTTP) llegan de hilos de
        // fondo: con una sesión activa o un join en vuelo hay que repintar
        // periódicamente para drenarlos aunque no haya input local.
        if self.collab.mode() != CollabMode::Inactive || self.collab.join_in_flight() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.show_launch_dialog(ctx);
        self.show_rename_dialog(ctx);
        self.show_search_bar(ctx);
        self.show_code_review(ctx);
        self.show_quick_open(ctx);
        self.restore_pending_scrollbacks();
        self.show_settings(ctx);
        self.show_broadcast(ctx);
        // Los toasts van último: se dibujan por encima de cualquier overlay.
        self.show_toasts(ctx);
        self.maybe_persist_state(ctx);

        if perf_snapshot.runtime_repaint {
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
}

const MAX_CONSECUTIVE_UPDATE_PANICS: u32 = 5;

impl eframe::App for TerminalApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Contain panics from a single frame so one rendering/logic bug does
        // not close the whole app; bail out only if every frame keeps
        // panicking, saving state first.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.update_impl(ctx);
        }));
        match outcome {
            Ok(()) => self.consecutive_update_panics = 0,
            Err(_) => {
                self.consecutive_update_panics = self.consecutive_update_panics.saturating_add(1);
                log::error!(
                    "update loop panicked ({} consecutive); attempting to continue",
                    self.consecutive_update_panics
                );
                if self.consecutive_update_panics >= MAX_CONSECUTIVE_UPDATE_PANICS {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        save_state(&self.snapshot_state());
                    }));
                    std::process::exit(1);
                }
                ctx.request_repaint();
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.collab.stop_session();
        save_state(&self.snapshot_state());
        // El autosave puede tener hasta AUTOSAVE_INTERVAL de atraso: al salir
        // guardamos el scrollback definitivo para no perder las últimas líneas.
        self.persist_scrollbacks();
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

pub(crate) fn gesture_pointer_pos(
    latest_pos: Option<Pos2>,
    interact_pos: Option<Pos2>,
    hover_pos: Option<Pos2>,
) -> Option<Pos2> {
    latest_pos.or(interact_pos).or(hover_pos)
}
