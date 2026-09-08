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
use crate::state::persistence::{AutosaveController, AutosaveDecision, PeriodicFlushController};
use crate::state::{save_state, AppState, Workspace};
use crate::theme::colors as palette;
use crate::theme::fonts::setup_fonts;
use crate::update::{RepaintPolicy, UpdateChecker};
use crate::utils::platform::home_dir;

mod broadcast_ui;
pub mod code_highlight;
mod code_review_ui;
#[allow(float_literal_f32_fallback)]
mod collab_ui;
mod desktop;
mod dialogs;
mod export_action;
mod file_viewer_ui;
mod memory_ui;
mod notify_policy;
mod onboarding;
mod orchestration_ui;
mod perf;
mod persistence_worker;
mod preferences_worker;
mod quick_open_ui;
mod resume_ui;
mod scrollback_restore_worker;
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
use self::desktop::{
    close_workspace_for_good, panel_scroll_capture_active, split_resize_hit, top_panel_hit,
    top_panel_scroll_hit, upsert_workspace_for_folder, SplitResizeAxis,
};
#[cfg(test)]
use self::desktop::{interpolate_viewport, overview_viewport_for_panels};
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
    closing_workspace: Option<Uuid>,
    rename_buf: String,
    search_open: bool,
    search_buf: String,
    search_panel_id: Option<Uuid>,
    code_review: Option<CodeReviewState>,
    preferences_worker: preferences_worker::PreferencesWorker,
    diff_loader: crate::orchestration::DiffLoader,
    worktree_ops: crate::orchestration::WorktreeOps,
    quick_open: Option<QuickOpenState>,
    quick_open_rx: Option<std::sync::mpsc::Receiver<Vec<String>>>,
    /// Resultado pendiente de la captura interactiva de pantalla (corre en un
    /// worker porque bloquea hasta que el usuario selecciona o cancela).
    screenshot_rx: Option<std::sync::mpsc::Receiver<anyhow::Result<PathBuf>>>,
    screenshot_target: Option<(Uuid, Uuid, Uuid)>,
    file_viewer: Option<file_viewer_ui::FileViewerState>,
    file_viewer_keyboard_active: bool,
    file_viewer_rx: Option<std::sync::mpsc::Receiver<file_viewer_ui::FileViewerState>>,
    settings_open: bool,
    settings_draft: Option<settings_ui::SettingsDraft>,
    broadcast: Option<broadcast_ui::BroadcastState>,
    resume_picker: Option<resume_ui::ResumeState>,
    file_tree: crate::sidebar::file_tree::FileTreeState,
    /// Paneles cuyo scrollback persistido ya se reinyectó en esta corrida.
    scrollback_restored: HashSet<Uuid>,
    scrollback_restore_worker: scrollback_restore_worker::ScrollbackRestoreWorker,
    scrollback_restore_ready: HashMap<Uuid, LeafHistories>,
    /// Identidades de panel/hoja observadas por esta instancia. La poda de
    /// scrollback queda limitada a este alcance para no borrar datos de otra
    /// ventana que use el mismo directorio durable.
    scrollback_known_leaves: HashMap<Uuid, HashSet<Uuid>>,
    highlighter: code_highlight::Highlighter,
    toasts: toast::Toasts,
    /// Último estado de agente por sesión que vimos, para notificar solo en la
    /// transición hacia un estado de atención (no repetirlo cada refresh).
    agent_status_seen: HashMap<Uuid, crate::orchestration::AgentStatus>,
    /// Gate de notificaciones del SO por workspace (cooldown, P1.8).
    notification_gate: notify_policy::NotificationGate,
    /// Servidor local de hooks de agentes (P2.12). `None` si no arrancó.
    hook_server: Option<crate::orchestration::HookServer>,
    /// Worker de la CLI `gh` y estado de la pestaña Tasks (P2.13).
    gh_client: crate::orchestration::GhClient,
    /// Worker de la API de Linear (P3.17).
    linear_client: crate::orchestration::LinearClient,
    /// Detección de agentes instalados y su resultado (Ship-it 7.2).
    agent_detector: crate::orchestration::AgentDetector,
    installed_agents: crate::orchestration::InstalledAgents,
    /// El overlay de primeros pasos ya se cerró (Ship-it 7.2).
    onboarding_dismissed: bool,
    /// Adopción del daemon de PTYs (P3.15, T3). Sin el feature `daemon` la app
    /// corre siempre in-process.
    #[cfg(all(unix, feature = "daemon"))]
    daemon: crate::daemon::backend::DaemonBackend,
    tasks_state: crate::sidebar::tasks::TasksState,
    /// Issues que esperan a que su worktree termine para pegarse al panel.
    pending_issue_links: HashMap<Uuid, u64>,
    /// ¿La ventana tiene foco del SO? (P1.8) Se refresca cada frame.
    window_focused: bool,
    brand_texture: Option<egui::TextureHandle>,
    sidebar: Sidebar,
    update_checker: UpdateChecker,
    fullscreen: bool,
    panel_gesture: Option<PanelGesture>,
    global_sash_drag: Option<GlobalSashDrag>,
    autosave: AutosaveController,
    /// La salida de los PTYs cambia aunque el layout permanezca idéntico.
    /// Este pulso mantiene durable el log incremental con cadencia propia.
    scrollback_flush: PeriodicFlushController,
    persisted_state: Option<AppState>,
    persistence_worker: persistence_worker::PersistenceWorker,
    repaint_policy: RepaintPolicy,
    launch_agent: Option<LaunchAgentDraft>,
    launch_memory_worker: orchestration_ui::LaunchMemoryWorker,
    memory_ui: Option<memory_ui::MemoryUiState>,
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
    /// Sólo una app real crea y elimina el marker global. El harness jamás
    /// debe borrar el marker perteneciente a una instancia viva del usuario.
    run_marker_active: bool,
    /// Un layout de schema futuro se abre sin restaurar, pero toda escritura
    /// durable queda bloqueada para no destruirlo ni podar sus scrollbacks.
    persistence_writes_enabled: bool,
}

impl TerminalApp {
    pub fn new(cc: &eframe::CreationContext<'_>, pending_join_invite: Option<String>) -> Self {
        setup_fonts(cc);
        let brand_texture = load_brand_texture(cc);
        let (loaded_state, incompatible_version) =
            match crate::state::persistence::load_state_result() {
                crate::state::persistence::StateLoadResult::Loaded(state) => (Some(state), None),
                crate::state::persistence::StateLoadResult::MissingOrUnreadable => (None, None),
                crate::state::persistence::StateLoadResult::IncompatibleFuture { version } => {
                    (None, Some(version))
                }
            };
        let mut app = Self::build(
            &cc.egui_ctx,
            brand_texture,
            loaded_state,
            pending_join_invite,
            true,
            incompatible_version.is_none(),
        );
        if let Some(version) = incompatible_version {
            app.toast_error(format!(
                "El layout usa el schema {version}, más nuevo que esta app; se abrió sin escribir para preservarlo"
            ));
        }
        app
    }

    /// Constructor sin `eframe::CreationContext` (Ship-it 7.4): el único
    /// acople con eframe eran las fuentes y la textura de marca, que acá no
    /// hacen falta. **No lee estado de disco**: el harness arranca limpio y
    /// nunca toca el layout real del usuario.
    #[cfg(test)]
    pub fn new_for_tests(ctx: &egui::Context) -> Self {
        // `side_effects: false`: el harness no levanta el hook server ni
        // instala hooks en el ~/.claude del usuario que corre los tests.
        Self::build(ctx, None, None, None, false, false)
    }

    fn build(
        egui_ctx: &egui::Context,
        brand_texture: Option<egui::TextureHandle>,
        loaded_state: Option<crate::state::persistence::AppState>,
        pending_join_invite: Option<String>,
        side_effects: bool,
        persistence_writes_enabled: bool,
    ) -> Self {
        let update_checker = if side_effects {
            UpdateChecker::new(egui_ctx)
        } else {
            UpdateChecker::disabled()
        };
        let has_saved_state = loaded_state.is_some();

        // El daemon se adopta **antes** de que exista cualquier workspace
        // (P3.15, T3): si se adoptara después, los PTYs ya estarían creados
        // in-process y el daemon no hostearía nada.
        #[cfg(all(unix, feature = "daemon"))]
        let daemon = if side_effects {
            crate::daemon::backend::DaemonBackend::adopt()
        } else {
            crate::daemon::backend::DaemonBackend::Fallback {
                reason: "modo test".to_owned(),
            }
        };
        #[cfg(all(unix, feature = "daemon"))]
        let daemon_endpoint = daemon.endpoint();

        let mut app = if let Some(mut saved) = loaded_state {
            crate::state::persistence::normalize_saved_state(&mut saved);
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
                let workspace = Workspace::from_saved(workspace, egui_ctx);
                // Las sesiones restauradas vienen detached: se atachean recién
                // al primer frame, así que instalar el spawner acá alcanza
                // para que se enganchen contra el daemon.
                #[cfg(all(unix, feature = "daemon"))]
                if let Some(endpoint) = daemon_endpoint.as_ref() {
                    install_daemon_spawner_on(&workspace, endpoint);
                }
                workspaces.push(workspace);
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
                ctx: Some(egui_ctx.clone()),
                command_palette: CommandPalette::default(),
                renaming_panel: None,
                closing_workspace: None,
                rename_buf: String::new(),
                search_open: false,
                search_buf: String::new(),
                search_panel_id: None,
                code_review: None,
                preferences_worker: Default::default(),
                diff_loader: crate::orchestration::DiffLoader::default(),
                worktree_ops: crate::orchestration::WorktreeOps::default(),
                quick_open: None,
                quick_open_rx: None,
                screenshot_rx: None,
                screenshot_target: None,
                file_viewer: None,
                file_viewer_keyboard_active: false,
                file_viewer_rx: None,
                settings_open: false,
                settings_draft: None,
                broadcast: None,
                resume_picker: None,
                file_tree: Default::default(),
                scrollback_restored: HashSet::new(),
                scrollback_restore_worker: scrollback_restore_worker::ScrollbackRestoreWorker::new(
                ),
                scrollback_restore_ready: HashMap::new(),
                scrollback_known_leaves: HashMap::new(),
                highlighter: code_highlight::Highlighter::new(),
                toasts: Default::default(),
                agent_status_seen: HashMap::new(),
                notification_gate: notify_policy::NotificationGate::new(
                    notify_policy::NOTIFICATION_COOLDOWN,
                ),
                hook_server: side_effects.then(start_hook_server).flatten(),
                gh_client: Default::default(),
                linear_client: Default::default(),
                agent_detector: crate::orchestration::AgentDetector::start(),
                installed_agents: Default::default(),
                onboarding_dismissed: crate::config::runtime_config().onboarding_dismissed,
                #[cfg(all(unix, feature = "daemon"))]
                daemon,
                tasks_state: Default::default(),
                pending_issue_links: HashMap::new(),
                window_focused: false,
                brand_texture,
                sidebar: Sidebar::default(),
                update_checker,
                fullscreen: false,
                panel_gesture: None,
                global_sash_drag: None,
                autosave: AutosaveController::new(AUTOSAVE_INTERVAL),
                scrollback_flush: PeriodicFlushController::new(AUTOSAVE_INTERVAL),
                persisted_state: None,
                persistence_worker: persistence_worker::PersistenceWorker::new(),
                repaint_policy: RepaintPolicy::new(RUNTIME_REPAINT_BATCH),
                launch_agent: None,
                launch_memory_worker: orchestration_ui::LaunchMemoryWorker::new(),
                memory_ui: None,
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
                run_marker_active: side_effects,
                persistence_writes_enabled,
            }
        } else {
            let collab = CollabManager::new();
            let broker_url = collab.broker_url().to_owned();
            let mut workspace = Workspace::new("Default", None);
            #[cfg(all(unix, feature = "daemon"))]
            if let Some(endpoint) = daemon_endpoint.as_ref() {
                install_daemon_spawner_on(&workspace, endpoint);
            }
            workspace.spawn_terminal(egui_ctx);
            Self {
                workspaces: vec![workspace],
                active_ws: 0,
                orchestrator: Orchestrator::new(),
                collab,
                viewport: Viewport::default(),
                sidebar_visible: true,
                show_grid: true,
                show_minimap: true,
                ctx: Some(egui_ctx.clone()),
                command_palette: CommandPalette::default(),
                renaming_panel: None,
                closing_workspace: None,
                rename_buf: String::new(),
                search_open: false,
                search_buf: String::new(),
                search_panel_id: None,
                code_review: None,
                preferences_worker: Default::default(),
                diff_loader: crate::orchestration::DiffLoader::default(),
                worktree_ops: crate::orchestration::WorktreeOps::default(),
                quick_open: None,
                quick_open_rx: None,
                screenshot_rx: None,
                screenshot_target: None,
                file_viewer: None,
                file_viewer_keyboard_active: false,
                file_viewer_rx: None,
                settings_open: false,
                settings_draft: None,
                broadcast: None,
                resume_picker: None,
                file_tree: Default::default(),
                scrollback_restored: HashSet::new(),
                scrollback_restore_worker: scrollback_restore_worker::ScrollbackRestoreWorker::new(
                ),
                scrollback_restore_ready: HashMap::new(),
                scrollback_known_leaves: HashMap::new(),
                highlighter: code_highlight::Highlighter::new(),
                toasts: Default::default(),
                agent_status_seen: HashMap::new(),
                notification_gate: notify_policy::NotificationGate::new(
                    notify_policy::NOTIFICATION_COOLDOWN,
                ),
                hook_server: side_effects.then(start_hook_server).flatten(),
                gh_client: Default::default(),
                linear_client: Default::default(),
                agent_detector: crate::orchestration::AgentDetector::start(),
                installed_agents: Default::default(),
                onboarding_dismissed: crate::config::runtime_config().onboarding_dismissed,
                #[cfg(all(unix, feature = "daemon"))]
                daemon,
                tasks_state: Default::default(),
                pending_issue_links: HashMap::new(),
                window_focused: false,
                brand_texture,
                sidebar: Sidebar::default(),
                update_checker,
                fullscreen: false,
                panel_gesture: None,
                global_sash_drag: None,
                autosave: AutosaveController::new(AUTOSAVE_INTERVAL),
                scrollback_flush: PeriodicFlushController::new(AUTOSAVE_INTERVAL),
                persisted_state: None,
                persistence_worker: persistence_worker::PersistenceWorker::new(),
                repaint_policy: RepaintPolicy::new(RUNTIME_REPAINT_BATCH),
                launch_agent: None,
                launch_memory_worker: orchestration_ui::LaunchMemoryWorker::new(),
                memory_ui: None,
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
                run_marker_active: side_effects,
                persistence_writes_enabled,
            }
        };

        if let Some(workspace) = app.workspaces.get(app.active_ws) {
            app.viewport.pan = workspace.viewport_pan;
            app.viewport.zoom = workspace.viewport_zoom.max(0.125);
        }
        app.remember_scrollback_layout();

        if has_saved_state {
            app.persisted_state = Some(app.snapshot_state());
        }

        app.reconcile_orchestration();
        app.refresh_orchestration();
        app.share_workspace_draft.broker_url = app.collab.broker_url().to_owned();
        // Marcador de corrida: si la anterior murió sin cierre limpio (kill,
        // crash nativo, OOM), avisamos que el estado igual se restauró. Sin
        // esto, una muerte súbita y un cierre normal eran indistinguibles.
        if side_effects && crate::state::run_marker::begin_run().is_some() {
            app.toast_error(
                "La sesión anterior terminó de golpe; se restauró el último estado guardado",
            );
        }
        if side_effects {
            if let Some(error) = crate::state::run_marker::persistence_claim_error() {
                app.toast_error(error);
            }
        }
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
            schema_version: crate::state::persistence::APP_STATE_SCHEMA_VERSION,
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
        if self.modal_input_is_active() {
            return None;
        }
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            {
                if modifiers.ctrl && modifiers.shift && key == Key::P {
                    consume_key_event(ctx, modifiers, key);
                    self.command_palette.toggle();
                    return None;
                }
                if let Some(command) = shortcut_command(&modifiers, key) {
                    if !matches!(self.collab.mode(), CollabMode::Guest)
                        && !command.available_on_desktop()
                    {
                        continue;
                    }
                    // El atajo es de la app. Quitarlo del stream evita que la
                    // misma tecla llegue después al PTY como byte de control.
                    consume_key_event(ctx, modifiers, key);
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
        self.remember_scrollback_layout();
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
            Command::ExportDiagnostics => self.export_diagnostics(),
            Command::AttachScreenshot => self.start_screenshot_capture(ctx),
            Command::BroadcastCommand => self.open_broadcast(),
            Command::ResumeConversation => self.open_resume_picker(),
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
            Command::SplitRight => {
                self.ws_mut()
                    .split_focused_panel(crate::terminal::split_tree::Axis::Horizontal);
            }
            Command::SplitDown => {
                self.ws_mut()
                    .split_focused_panel(crate::terminal::split_tree::Axis::Vertical);
            }
            Command::CloseLeaf => {
                self.ws_mut().close_focused_leaf();
                self.reconcile_orchestration();
            }
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
            Command::OpenMemory => self.open_memory_hub(),
            Command::RememberSelection => self.open_remember_selection(),
            Command::CreateHandoff => self.open_create_handoff(),
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

    /// Ejecuta el cierre que la persona ya confirmó en el diálogo. Los
    /// archivos del proyecto no se tocan: sólo se cierran sus sesiones y se
    /// quita el workspace del estado de TerminalCanvas.
    fn close_workspace_confirmed(&mut self, workspace_id: Uuid) {
        self.orchestrator.cancel_workspace_launches(workspace_id);
        self.remember_scrollback_layout();
        let Some(workspace) = self
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };
        let name = workspace.name.clone();
        let was_active = self.ws().id == workspace_id;
        let was_shared = self.collab.shared_workspace_id() == Some(workspace_id);
        if should_stop_collaboration_on_workspace_close(self.collab.mode(), was_active, was_shared)
        {
            self.collab.stop_session();
        }

        self.workspaces[self.active_ws].viewport_pan = self.viewport.pan;
        self.workspaces[self.active_ws].viewport_zoom = self.viewport.zoom;
        let Some(closed_terminals) =
            close_workspace_for_good(&mut self.workspaces, &mut self.active_ws, workspace_id)
        else {
            return;
        };

        #[cfg(all(unix, feature = "daemon"))]
        if let Some(endpoint) = self.daemon.endpoint() {
            install_daemon_spawner_on(self.ws(), &endpoint);
        }
        self.viewport.pan = self.ws().viewport_pan;
        self.viewport.zoom = self.ws().viewport_zoom.max(0.125);
        let can_persist = self.ensure_persistence_ownership();
        if can_persist {
            self.persist_scrollbacks(false);
        }
        self.reconcile_daemon_sessions();
        self.reconcile_orchestration();
        self.refresh_orchestration();

        // Un cierre explícito no debe reaparecer si la app termina antes del
        // próximo tick de autosave.
        if can_persist {
            let snapshot = self.snapshot_state();
            match crate::state::persistence::try_save_state(&snapshot) {
                Ok(()) => {
                    self.persisted_state = Some(snapshot);
                    self.autosave.mark_saved(Instant::now());
                }
                Err(err) => {
                    log::warn!("no se pudo persistir el cierre del workspace: {err}");
                    self.toast_error("El proyecto se cerró, pero no se pudo guardar el cambio");
                    return;
                }
            }
        }

        let terminal_summary = if closed_terminals == 1 {
            "1 terminal cerrado".to_owned()
        } else {
            format!("{closed_terminals} terminales cerrados")
        };
        self.toast_success(format!("{name} cerrado · {terminal_summary}"));
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
        let index = upsert_workspace_for_folder(&mut self.workspaces, path.clone());
        // Un workspace nuevo tiene su propio PtyManager: hay que decirle que
        // las sesiones van al daemon antes de que spawnee su terminal.
        #[cfg(all(unix, feature = "daemon"))]
        if let Some(endpoint) = self.daemon.endpoint() {
            if let Some(workspace) = self.workspaces.get(index) {
                install_daemon_spawner_on(workspace, &endpoint);
            }
        }
        self.switch_workspace(index);
        // Opening a project never deletes old worktree files. Archives and
        // legacy trash are retained for explicit recovery.
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
                SidebarResponse::RequestCloseWorkspace(workspace_id) => {
                    self.closing_workspace = Some(workspace_id);
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
                SidebarResponse::RefreshTasks => self.refresh_github_tasks(true),
                SidebarResponse::OpenTask(number) => self.open_github_task(number),
                SidebarResponse::StartWorkOnIssue(number) => self.start_work_on_issue(number),
                SidebarResponse::StartWorkOnLinearIssue(identifier) => {
                    self.start_work_on_linear_issue(&identifier)
                }
                SidebarResponse::ExportScrollback => self.export_focused_scrollback(),
                SidebarResponse::OpenUpdate(url) => {
                    if let Err(err) = crate::utils::platform::open_url_external(&url) {
                        self.toast_error(format!("No se pudo abrir la actualización: {err}"));
                    }
                }
                SidebarResponse::OpenFileInViewer(path) => self.open_file_viewer(path),
            }
        }
        self.reconcile_orchestration();
    }

    fn maybe_persist_state(&mut self, ctx: &egui::Context) {
        if !self.persistence_writes_enabled {
            return;
        }
        let snapshot = self.snapshot_state();
        let now = Instant::now();
        let state_decision =
            self.autosave
                .should_persist(&snapshot, self.persisted_state.as_ref(), now);
        let scrollback_decision = self.scrollback_flush.decision(now);
        if (matches!(state_decision, AutosaveDecision::SaveNow)
            || matches!(scrollback_decision, AutosaveDecision::SaveNow))
            && !self.ensure_persistence_ownership()
        {
            return;
        }

        match state_decision {
            AutosaveDecision::Idle => {}
            AutosaveDecision::ScheduleAfter(delay) => ctx.request_repaint_after(delay),
            AutosaveDecision::SaveNow => {
                if self.persistence_worker.state_in_flight()
                    || self.persistence_worker.submit_state(snapshot)
                {
                    ctx.request_repaint_after(Duration::from_millis(50));
                } else {
                    ctx.request_repaint_after(AUTOSAVE_INTERVAL);
                }
            }
        }

        // El scrollback no forma parte de `AppState`; condicionarlo a que el
        // layout cambie deja la salida estable sólo en RAM hasta `on_exit`.
        match scrollback_decision {
            AutosaveDecision::Idle => {}
            AutosaveDecision::ScheduleAfter(delay) => ctx.request_repaint_after(delay),
            AutosaveDecision::SaveNow => {
                if self.persist_scrollbacks(false) {
                    self.reconcile_daemon_sessions();
                    self.scrollback_flush.mark_flushed(now);
                    ctx.request_repaint_after(AUTOSAVE_INTERVAL);
                } else {
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
            }
        }
    }

    fn ensure_persistence_ownership(&mut self) -> bool {
        if !self.persistence_writes_enabled {
            return false;
        }
        if !self.run_marker_active || crate::state::run_marker::current_process_may_write() {
            return true;
        }
        self.persistence_writes_enabled = false;
        self.toast_error(
            "Otra instancia de TerminalCanvas tomó el guardado; esta ventana dejó de escribir para proteger tus proyectos",
        );
        false
    }

    fn poll_persistence_worker(&mut self, ctx: &egui::Context) {
        for completion in self.persistence_worker.poll() {
            match completion {
                persistence_worker::Completion::State { snapshot, result } => match result {
                    Ok(()) => {
                        self.persisted_state = Some(snapshot);
                        self.autosave.mark_saved(Instant::now());
                    }
                    Err(err) => {
                        log::warn!("Autosave failed: {err}");
                        ctx.request_repaint_after(AUTOSAVE_INTERVAL);
                    }
                },
                persistence_worker::Completion::Incremental {
                    rollover_panels,
                    acknowledgements,
                } => {
                    self.acknowledge_persisted_logs(acknowledgements);
                    if rollover_panels.is_empty() {
                        continue;
                    }
                    let Some(dir) = crate::state::scrollback_store::scrollback_dir() else {
                        continue;
                    };
                    let wanted = rollover_panels.into_iter().collect::<HashSet<_>>();
                    let entries = self.full_scrollback_entries(&dir, Some(&wanted));
                    if !entries.is_empty() && !self.persistence_worker.submit_full(entries) {
                        log::warn!("no se pudo encolar el rollover de scrollback");
                    }
                }
                persistence_worker::Completion::Full { acknowledgements } => {
                    self.acknowledge_persisted_logs(acknowledgements);
                }
            }
        }
    }

    fn acknowledge_persisted_logs(
        &self,
        acknowledgements: Vec<persistence_worker::IncrementalAck>,
    ) {
        for acknowledgement in acknowledgements {
            let Some(leaf_id) = acknowledgement.leaf_id else {
                continue;
            };
            if let Some(panel) = self
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.panels.iter())
                .find(|panel| panel.id() == acknowledgement.panel_id)
            {
                panel.acknowledge_leaf_log(leaf_id, acknowledgement.written_bytes);
            }
        }
    }
}

fn should_stop_collaboration_on_workspace_close(
    mode: CollabMode,
    was_active: bool,
    was_shared: bool,
) -> bool {
    !matches!(mode, CollabMode::Inactive) && (was_active || was_shared)
}

impl TerminalApp {
    /// Un frame de la app, para el harness E2E (Ship-it 7.4).
    #[cfg(test)]
    pub fn update_for_tests(&mut self, ctx: &egui::Context) {
        self.update_impl(ctx);
    }

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
                egui::Frame::NONE
                    .fill(CANVAS_BG)
                    .inner_margin(egui::Margin::same(0))
                    .outer_margin(egui::Margin::same(0)),
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
        self.command_palette.desktop_mode = !matches!(self.collab.mode(), CollabMode::Guest);
        self.window_focused = ctx.input(|input| input.focused);
        self.poll_persistence_worker(ctx);
        self.handle_collab_events();
        self.sync_window_transitions(ctx);
        self.maybe_refresh_orchestration();
        self.poll_diff_loader();
        self.poll_preferences_worker();
        if self.preferences_worker.busy() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        self.poll_worktree_ops();
        self.poll_quick_open();
        self.poll_hook_events();
        self.poll_gh_client();
        self.poll_design_captures();
        if let Some(installed) = self.agent_detector.poll() {
            self.installed_agents = installed;
        }
        self.poll_screenshot_capture(ctx);
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
        #[cfg(all(unix, feature = "daemon"))]
        let focused_session = self
            .ws()
            .focused_panel()
            .and_then(|panel| panel.focused_runtime_session_id());
        #[cfg(all(unix, feature = "daemon"))]
        self.daemon.set_priority_session(focused_session);
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
                .and_then(|panel| panel.focused_runtime_session_id())
                .map(|session_id| dirty_sessions.contains(&session_id))
                .unwrap_or(false);
            if focused_dirty {
                self.repaint_policy.note_focused_runtime_event();
            } else {
                self.repaint_policy.note_runtime_event();
            }
            for panel in &mut self.ws_mut().panels {
                if panel
                    .all_runtime_session_ids()
                    .iter()
                    .any(|session_id| dirty_sessions.contains(session_id))
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
        if self.terminal_input_is_routable() {
            let focused_panel_id = self.ws().focused_panel().map(|panel| panel.id());
            if let Some(panel_id) = focused_panel_id {
                if matches!(self.collab.mode(), CollabMode::Host)
                    && host_terminal_input_pending(ctx)
                    && self.collab.controller_for(panel_id).is_some()
                {
                    self.collab.revoke_control(panel_id, "Host took control");
                }
            }
            // ¿Hubo un keystroke/click real este frame? (P1.8) La mera
            // selección no limpia el unread; la interacción sí.
            let interacted = ctx.input(|input| {
                input.events.iter().any(|event| {
                    matches!(
                        event,
                        egui::Event::Key { pressed: true, .. }
                            | egui::Event::PointerButton { pressed: true, .. }
                            | egui::Event::Text(_)
                            | egui::Event::Paste(_)
                    )
                })
            });
            if let Some(panel) = self.ws_mut().focused_panel_mut() {
                panel.handle_input(ctx);
                if interacted && panel.unread() {
                    panel.set_unread(false);
                }
            }
        }
    }

    /// Única interfaz que decide si los eventos crudos del frame pertenecen
    /// al PTY. Toda superficie modal vive acá para que briefs, invites y
    /// passphrases nunca se escriban también en el shell detrás del diálogo.
    fn terminal_input_is_routable(&self) -> bool {
        !(self.modal_input_is_active()
            || (self.file_viewer.is_some() && self.file_viewer_keyboard_active)
            || matches!(self.collab.mode(), CollabMode::Guest))
    }

    fn modal_input_is_active(&self) -> bool {
        self.command_palette.open
            || self.renaming_panel.is_some()
            || self.closing_workspace.is_some()
            || self.search_open
            || self.code_review.is_some()
            || self.quick_open.is_some()
            || self.settings_open
            || self.broadcast.is_some()
            || self.resume_picker.is_some()
            || self.launch_agent.is_some()
            || self.memory_ui.is_some()
            || self.share_workspace_open
            || self.join_session_open
    }

    fn show_sidebar(&mut self, ctx: &egui::Context) {
        if self.sidebar_visible && !matches!(self.collab.mode(), CollabMode::Guest) {
            SidePanel::left("sidebar")
                // Un ancho estable evita saltos de layout al pasar entre
                // Workspaces, Files, Tasks y Online. El contenido largo se
                // trunca o envuelve dentro del panel en vez de quitarle lugar
                // de golpe a las terminales.
                .resizable(false)
                .exact_width(228.0)
                .frame(
                    egui::Frame::NONE
                        .fill(crate::theme::colors::INK)
                        .inner_margin(egui::Margin::same(0))
                        .outer_margin(egui::Margin::same(0)),
                )
                .show_separator_line(false)
                .show(ctx, |ui| {
                    let state = self.update_checker.snapshot();
                    let attention = self.attention_items();
                    // El explorador sigue la carpeta del workspace activo.
                    let ws_root = self.workspaces[self.active_ws].cwd.clone();
                    self.file_tree.set_root(ws_root);
                    // Con la pestaña Tasks abierta se refresca respetando el
                    // cache de 60 s (P2.13).
                    if self.sidebar.active_tab == crate::sidebar::SidebarTab::Tasks {
                        self.refresh_github_tasks(false);
                    }
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
                        &self.tasks_state,
                    );
                    self.handle_sidebar_responses(responses, ctx);
                });
        }
    }

    /// Sesiones de agente del workspace activo que piden atención, para la
    /// sección "Atención" del sidebar.
    /// Cuándo hay que repintar para animar el cursor, o `None` si no hace falta
    /// repintar por su cuenta.
    ///
    /// Antes esto era un repaint fijo cada 120 ms mientras hubiera un panel
    /// enfocado y vivo: ~8 frames por segundo para siempre, aunque no cambiara
    /// nada. Dos recortes:
    ///
    /// - Con la ventana del SO sin foco no se dibuja cursor, así que la app en
    ///   segundo plano no repinta nada.
    /// - Con foco se pide el repaint en el instante exacto del próximo cambio
    ///   de fase (2 por segundo) en vez de sondear.
    fn cursor_blink_repaint_delay(&self, ctx: &egui::Context) -> Option<Duration> {
        let window_focused = ctx.input(|input| input.focused);
        if !window_focused {
            return None;
        }
        let has_live_focused_panel = self
            .ws()
            .panels
            .iter()
            .any(|panel| panel.focused() && panel.is_alive() && !panel.minimized());
        if !has_live_focused_panel {
            return None;
        }
        let now = ctx.input(|input| input.time);
        Some(Duration::from_secs_f64(
            crate::terminal::renderer::time_until_blink_change(now),
        ))
    }

    /// Le dice al daemon qué sesiones siguen vivas, para que mate las
    /// huérfanas (P3.15, T5). Sin el feature `daemon` es un no-op.
    fn reconcile_daemon_sessions(&mut self) {
        #[cfg(all(unix, feature = "daemon"))]
        {
            if !self.daemon.is_connected() {
                return;
            }
            let live: Vec<Uuid> = self
                .workspaces
                .iter()
                .flat_map(crate::state::Workspace::all_runtime_session_ids)
                .collect();
            let killed = self.daemon.reconcile_live(live);
            if !killed.is_empty() {
                log::info!("el daemon mató {} sesiones huérfanas", killed.len());
            }
        }
    }

    /// Exporta el diagnóstico a Descargas (Ship-it 7.5). Sin secretos: los
    /// tokens se redactan y los títulos/paths se hashean.
    fn export_diagnostics(&mut self) {
        match crate::utils::diagnostics::export(env!("CARGO_PKG_VERSION")) {
            Ok(path) => {
                self.toast_success(format!("Diagnóstico en {}", path.display()));
            }
            Err(err) => self.toast_error(format!("No se pudo exportar el diagnóstico: {err}")),
        }
    }

    /// Guarda el scrollback de cada panel vivo de todos los workspaces y borra
    /// los archivos de paneles conocidos por esta instancia que ya no existen.
    ///
    /// `full` reescribe el checkpoint completo (cierre limpio / panic /
    /// rollover); `false` appendea solo el log incremental (autosave de 2 s).
    fn persist_scrollbacks(&mut self, full: bool) -> bool {
        let Some(dir) = crate::state::scrollback_store::scrollback_dir() else {
            return false;
        };
        self.remember_scrollback_layout();
        if full {
            self.persistence_worker.wait_until_idle();
            let entries = self.full_scrollback_entries(&dir, None);
            let _ = persistence_worker::persist_full_entries(entries);
            self.prune_scrollback_files(&dir);
            return true;
        }
        if self.persistence_worker.scrollback_in_flight() {
            return false;
        }

        let mut entries = Vec::new();
        let mut live_panels = Vec::new();
        for workspace in &self.workspaces {
            for panel in &workspace.panels {
                let panel_id = panel.id();
                live_panels.push((panel_id, panel.leaf_ids()));
                entries.extend(
                    panel
                        .pending_leaf_logs()
                        .into_iter()
                        .filter(|(_, frames)| !frames.is_empty())
                        .map(|(leaf, frames)| persistence_worker::IncrementalEntry {
                            dir: dir.clone(),
                            panel_id,
                            leaf_id: Some(leaf),
                            frames,
                        }),
                );
            }
        }
        let known_panels = self
            .scrollback_known_leaves
            .iter()
            .map(|(panel, leaves)| (*panel, leaves.iter().copied().collect()))
            .collect();
        let submitted =
            self.persistence_worker
                .submit_incremental(persistence_worker::IncrementalBatch {
                    dir,
                    entries,
                    live_panels,
                    known_panels,
                });
        if submitted {
            // El batch ya conserva el alcance de poda que necesita. Retener
            // después sólo el layout vivo evita que una sesión larga acumule
            // un UUID por cada terminal y split cerrados.
            self.scrollback_known_leaves.clear();
            self.remember_scrollback_layout();
        }
        submitted
    }

    fn remember_scrollback_layout(&mut self) {
        let live = self
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace
                    .panels
                    .iter()
                    .map(|panel| (panel.id(), panel.leaf_ids()))
            })
            .collect::<Vec<_>>();
        for (panel, leaves) in live {
            self.scrollback_known_leaves
                .entry(panel)
                .or_default()
                .extend(leaves);
        }
    }

    fn prune_scrollback_files(&mut self, dir: &std::path::Path) {
        self.remember_scrollback_layout();
        let mut live_ids = Vec::new();
        for workspace in &self.workspaces {
            for panel in &workspace.panels {
                live_ids.push(panel.id());
                let known_leaves = self
                    .scrollback_known_leaves
                    .get(&panel.id())
                    .map(|leaves| leaves.iter().copied().collect::<Vec<_>>())
                    .unwrap_or_default();
                crate::state::scrollback_store::prune_panel_leaf_scrollback(
                    dir,
                    panel.id(),
                    &panel.leaf_ids(),
                    &known_leaves,
                );
            }
        }
        let known_ids = self
            .scrollback_known_leaves
            .keys()
            .copied()
            .collect::<Vec<_>>();
        crate::state::scrollback_store::prune_scrollback(dir, &live_ids, &known_ids);
    }

    fn full_scrollback_entries(
        &self,
        dir: &std::path::Path,
        only_panels: Option<&HashSet<Uuid>>,
    ) -> Vec<persistence_worker::FullEntry> {
        let mut entries = Vec::new();
        for workspace in &self.workspaces {
            for panel in &workspace.panels {
                let panel_id = panel.id();
                if only_panels.is_some_and(|wanted| !wanted.contains(&panel_id)) {
                    continue;
                }
                entries.extend(panel.leaf_scrollbacks().into_iter().map(
                    |(leaf, text, pending_bytes)| persistence_worker::FullEntry {
                        dir: dir.to_path_buf(),
                        panel_id,
                        leaf_id: leaf,
                        text,
                        pending_bytes,
                    },
                ));
            }
        }
        entries
    }

    /// Reinyecta el historial guardado en los paneles que acaban de conseguir
    /// terminal. Cada panel se restaura una sola vez por corrida.
    fn restore_pending_scrollbacks(&mut self) {
        let live_panels = self
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.panels.iter())
            .map(crate::panel::CanvasPanel::id)
            .collect::<HashSet<_>>();
        self.scrollback_restored
            .retain(|panel_id| live_panels.contains(panel_id));

        for completion in self.scrollback_restore_worker.poll() {
            if live_panels.contains(&completion.panel_id) {
                self.scrollback_restore_ready
                    .insert(completion.panel_id, completion.histories);
            }
        }

        let ready_panels = self
            .scrollback_restore_ready
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for panel_id in ready_panels {
            let Some(leaf_histories) = self.scrollback_restore_ready.remove(&panel_id) else {
                continue;
            };
            if leaf_histories.is_empty() {
                self.scrollback_restored.insert(panel_id);
                continue;
            }
            let active_leaves: HashSet<_> = self
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.panels.iter())
                .find(|panel| panel.id() == panel_id)
                .map(crate::panel::CanvasPanel::leaf_ids)
                .unwrap_or_default()
                .into_iter()
                .collect();
            if !leaf_histories
                .iter()
                .any(|(leaf, _, _)| leaf.is_none_or(|leaf| active_leaves.contains(&leaf)))
            {
                self.scrollback_restored.insert(panel_id);
                continue;
            }
            let restored = self
                .workspaces
                .iter_mut()
                .flat_map(|workspace| workspace.panels.iter_mut())
                .find(|panel| panel.id() == panel_id)
                .map(|panel| panel.restore_leaf_histories(&leaf_histories))
                .unwrap_or(true);
            if restored {
                self.scrollback_restored.insert(panel_id);
            } else {
                self.scrollback_restore_ready
                    .insert(panel_id, leaf_histories);
            }
        }

        if self.scrollback_restored.len() == live_panels.len() {
            return;
        }
        let Some(dir) = crate::state::scrollback_store::scrollback_dir() else {
            return;
        };
        if !self.scrollback_restore_worker.in_flight() {
            let next = live_panels.iter().copied().find(|panel_id| {
                !self.scrollback_restored.contains(panel_id)
                    && !self.scrollback_restore_ready.contains_key(panel_id)
            });
            if let Some(panel_id) = next {
                self.scrollback_restore_worker.submit(dir, panel_id);
            }
        }
        if self.scrollback_restore_worker.in_flight() {
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(Duration::from_millis(16));
            }
        }
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
        ui.painter().rect_stroke(
            desktop_screen,
            0.0,
            Stroke::new(0.0_f32, palette::LINE),
            egui::StrokeKind::Middle,
        );
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

        // Drag & drop de archivos del SO: mientras el puntero está sobre un
        // panel se resalta el destino; al soltar se tipea el path
        // shell-escapado (como Terminal.app).
        let drop_highlight_index = self.handle_file_drop(ctx, pointer_pos, canvas_rect);

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
            if let Some(issue) = interaction.open_issue {
                self.open_github_task(issue);
            }
            guides.extend(interaction.guides);
        }

        // Ring sobre el panel que recibiría los archivos soltados.
        if let Some(index) = drop_highlight_index {
            if let Some(panel) = self.ws().panels.get(index) {
                let rect = panel.rect();
                let screen_rect = Rect::from_min_max(
                    self.viewport.canvas_to_screen(rect.min, canvas_rect),
                    self.viewport.canvas_to_screen(rect.max, canvas_rect),
                );
                ui.painter().rect_stroke(
                    screen_rect.expand(2.0),
                    10.0,
                    Stroke::new(2.0_f32, palette::TEXT_STRONG),
                    egui::StrokeKind::Middle,
                );
            }
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

    /// Drag & drop de archivos del SO sobre el canvas.
    ///
    /// Mientras hay archivos en vuelo (`hovered_files`) se pide repaint y se
    /// devuelve el índice del panel bajo el puntero para resaltarlo. Al soltar
    /// (`dropped_files`) se tipean los paths shell-escapados en ese panel,
    /// precedidos de espacio, igual que Terminal.app: varios archivos quedan
    /// en una sola línea listos para Enter.
    /// Lanza la captura interactiva de pantalla en un worker (bloquea hasta
    /// que el usuario selecciona el área o cancela) y deja el receiver para
    /// mandarle el path al agente enfocado cuando llegue.
    fn start_screenshot_capture(&mut self, ctx: &egui::Context) {
        if self.screenshot_rx.is_some() {
            self.toast_error("Ya hay una captura en curso");
            return;
        }
        let Some(target) = self
            .ws()
            .focused_panel()
            .filter(|panel| panel.is_alive())
            .map(|panel| (self.ws().id, panel.id(), panel.focused_leaf_id()))
        else {
            self.toast_error("Elegí una terminal activa para adjuntar la captura");
            return;
        };
        let Some(dir) = crate::utils::platform::screenshots_dir() else {
            self.toast_error("No pude resolver el directorio de capturas");
            return;
        };
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.toast_error(format!("No pude crear el directorio de capturas: {err}"));
            return;
        }
        let dest = dir.join(format!(
            "captura-{}.png",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        let (tx, rx) = std::sync::mpsc::channel();
        // Si el hilo no arranca, el receiver se desconecta y el poll lo
        // descarta: nunca queda colgado.
        let _ = std::thread::Builder::new()
            .name("screenshot".to_owned())
            .spawn(move || {
                let result = crate::utils::platform::capture_interactive(&dest).map(|()| dest);
                let _ = tx.send(result);
            });
        self.screenshot_rx = Some(rx);
        self.screenshot_target = Some(target);
        self.toast_success("Seleccioná el área de la pantalla a capturar");
        ctx.request_repaint();
    }

    fn poll_screenshot_capture(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.screenshot_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(path)) => {
                self.screenshot_rx = None;
                self.deliver_screenshot(path);
            }
            Ok(Err(err)) => {
                self.screenshot_rx = None;
                self.screenshot_target = None;
                self.toast_error(err.to_string());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // La captura sigue abierta: volver a preguntar más tarde.
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.screenshot_rx = None;
                self.screenshot_target = None;
            }
        }
    }

    fn deliver_screenshot(&mut self, path: PathBuf) {
        let Some((workspace_id, panel_id, leaf_id)) = self.screenshot_target.take() else {
            return;
        };
        let target_index = self.workspace_index_by_id(workspace_id).filter(|index| {
            self.workspaces[*index]
                .panel(panel_id)
                .is_some_and(|panel| panel.is_alive() && panel.focused_leaf_id() == leaf_id)
        });
        let Some(index) = target_index else {
            self.toast_error(format!(
                "El destino cambió. Captura guardada en {}",
                path.display()
            ));
            return;
        };
        // El path va quoteado: los agentes reciben el literal exacto aunque
        // tenga espacios o caracteres raros.
        let prompt = format!(
            "Mirá esta captura: {}",
            crate::terminal::shell_quote::quote_path(&path.to_string_lossy())
        );
        if self.workspaces[index].send_prompt_to_panel(panel_id, &prompt) {
            self.toast_success("Captura enviada al agente");
        } else {
            self.toast_error("No se pudo escribir en ese terminal");
        }
    }

    fn handle_file_drop(
        &mut self,
        ctx: &egui::Context,
        pointer_pos: Option<Pos2>,
        canvas_rect: Rect,
    ) -> Option<usize> {
        let (hovering, dropped_files) = ctx.input(|input| {
            (
                !input.raw.hovered_files.is_empty(),
                input.raw.dropped_files.clone(),
            )
        });
        if !hovering && dropped_files.is_empty() {
            return None;
        }
        // Mantener el highlight mientras el drag sigue vivo.
        ctx.request_repaint();
        let hit = pointer_pos
            .and_then(|pos| top_panel_hit(self.ws(), pos, &self.viewport, canvas_rect))?;
        let panel_id = self.ws().panels[hit.index].id();

        // Los dropped_files sin path local (drag entre apps sin archivo real)
        // no tienen nada que tipear.
        let paths: Vec<_> = dropped_files
            .iter()
            .filter_map(|file| file.path.clone())
            .collect();
        if !paths.is_empty() {
            let mut text = String::new();
            for path in &paths {
                text.push(' ');
                text.push_str(&crate::terminal::shell_quote::quote_path(
                    &path.to_string_lossy(),
                ));
            }
            if let Some(panel) = self.ws_mut().panel_mut(panel_id) {
                panel.insert_text(&text);
            }
            let count_label = if paths.len() == 1 { "" } else { "s" };
            self.toast_success(format!(
                "{}: path{count_label} pegado{count_label} en el terminal",
                paths
                    .first()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "archivo".to_owned()),
            ));
        }
        Some(hit.index)
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
        self.show_close_workspace_dialog(ctx);
        self.show_search_bar(ctx);
        self.show_code_review(ctx);
        self.show_quick_open(ctx);
        self.show_onboarding(ctx);
        self.restore_pending_scrollbacks();
        self.show_settings(ctx);
        self.show_memory_hub(ctx);
        self.show_broadcast(ctx);
        self.show_resume_picker(ctx);
        // Los toasts van último: se dibujan por encima de cualquier overlay.
        self.show_toasts(ctx);
        self.maybe_persist_state(ctx);

        if perf_snapshot.runtime_repaint {
            ctx.request_repaint();
        }

        if let Some(delay) = self.repaint_policy.next_repaint_delay(Instant::now()) {
            ctx.request_repaint_after(delay.max(Duration::from_millis(1)));
        } else if let Some(delay) = self.cursor_blink_repaint_delay(ctx) {
            ctx.request_repaint_after(delay);
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
                    // Camino catastrófico: acá se pierde todo lo que no esté en
                    // disco, así que se guarda lo mismo que en una salida
                    // limpia. Antes sólo se guardaba el layout y el scrollback
                    // de la sesión se perdía entero.
                    if self.persistence_writes_enabled
                        && (!self.run_marker_active
                            || crate::state::run_marker::current_process_may_write())
                    {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            save_state(&self.snapshot_state());
                            self.persist_scrollbacks(true);
                        }));
                    }
                    std::process::exit(1);
                }
                ctx.request_repaint();
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.collab.stop_session();
        if self.persistence_writes_enabled
            && (!self.run_marker_active || crate::state::run_marker::current_process_may_write())
        {
            save_state(&self.snapshot_state());
            // El autosave puede tener hasta AUTOSAVE_INTERVAL de atraso: al salir
            // guardamos el scrollback definitivo para no perder las últimas líneas.
            self.persist_scrollbacks(true);
        }
        // El daemon se apaga solo si no le quedan sesiones (P3.15, T5).
        #[cfg(all(unix, feature = "daemon"))]
        self.daemon.shutdown_if_idle();
        if self.run_marker_active {
            crate::state::run_marker::end_run_clean();
        }
    }
}

fn consume_key_event(ctx: &egui::Context, modifiers: egui::Modifiers, key: Key) {
    ctx.input_mut(|input| {
        input.events.retain(|event| {
            !matches!(
                event,
                egui::Event::Key {
                    key: event_key,
                    pressed: true,
                    modifiers: event_modifiers,
                    ..
                } if *event_key == key && *event_modifiers == modifiers
            )
        });
    });
}

/// Hace que las sesiones nuevas de este workspace se creen en el daemon
/// (P3.15, T3). Se aplica al crear el workspace, antes de que exista cualquier
/// terminal: instalarlo después no sirve, porque los PTYs ya estarían locales.
#[cfg(all(unix, feature = "daemon"))]
fn install_daemon_spawner_on(
    workspace: &Workspace,
    endpoint: &crate::daemon::sessions::DaemonEndpoint,
) {
    let endpoint = endpoint.clone();
    if let Ok(mut manager) = workspace.pty_manager().lock() {
        manager.set_remote_spawner(Box::new(move |spec, cols, rows, scheduler, desired_id| {
            crate::daemon::sessions::spawn_remote(
                &endpoint, spec, cols, rows, scheduler, desired_id,
            )
        }));
    }
}

/// Arranca el servidor de hooks e instala los hooks de Claude (P2.12).
/// Best-effort: si algo falla, la app sigue andando con OSC 9999 y heurística.
fn start_hook_server() -> Option<crate::orchestration::HookServer> {
    match crate::orchestration::HookServer::start() {
        Ok(server) => {
            log::info!("hook server escuchando en {}", server.url());
            if let Err(err) = crate::orchestration::install_claude_hooks() {
                log::warn!("no se pudieron instalar los hooks de Claude: {err}");
            }
            Some(server)
        }
        Err(err) => {
            log::warn!("no se pudo arrancar el hook server: {err}");
            None
        }
    }
}

fn read_leaf_generation(dir: &std::path::Path, panel_id: Uuid, leaf_id: Option<Uuid>) -> u32 {
    if let Some((Some(generation), _)) =
        crate::state::scrollback_store::load_leaf_scrollback_checkpoint(dir, panel_id, leaf_id)
    {
        return generation;
    }
    let path =
        dir.join(crate::state::scrollback_store::scrollback_leaf_gen_file_name(panel_id, leaf_id));
    std::fs::read(path)
        .ok()
        .filter(|bytes| bytes.len() >= 4)
        .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .unwrap_or(0)
}

#[cfg(test)]
fn write_generation(dir: &std::path::Path, panel_id: Uuid, generation: u32) -> std::io::Result<()> {
    write_leaf_generation(dir, panel_id, None, generation)
}

#[cfg(test)]
fn write_leaf_generation(
    dir: &std::path::Path,
    panel_id: Uuid,
    leaf_id: Option<Uuid>,
    generation: u32,
) -> std::io::Result<()> {
    let path =
        dir.join(crate::state::scrollback_store::scrollback_leaf_gen_file_name(panel_id, leaf_id));
    crate::state::durable_write::write_atomic(&path, &generation.to_le_bytes()).map(|_| ())
}

/// Appendea un lote ya encodeado al log de una sesión. Devuelve `true` si el
/// log alcanzó el umbral de rollover.
fn persist_incremental_frames(
    dir: &std::path::Path,
    panel_id: Uuid,
    leaf_id: Option<Uuid>,
    frames: &[u8],
) -> Option<bool> {
    let log_path =
        dir.join(crate::state::scrollback_store::scrollback_leaf_log_file_name(panel_id, leaf_id));
    let generation = read_leaf_generation(dir, panel_id, leaf_id);
    let log_is_current = std::fs::read(&log_path)
        .ok()
        .and_then(|bytes| crate::state::scrollback_log::read_frames(&bytes))
        .is_some_and(|(log_generation, _)| log_generation == generation);
    if !log_is_current {
        if let Err(err) = crate::state::scrollback_log::reset_log(&log_path, generation) {
            log::warn!("No se pudo inicializar el log del panel: {err}");
            return None;
        }
    }
    if let Err(err) = crate::state::scrollback_log::append_frames(&log_path, frames) {
        log::warn!("No se pudo appendear al log del panel: {err}");
        return None;
    }
    Some(
        std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0)
            > crate::state::scrollback_log::MAX_LOG_BYTES,
    )
}

/// Historiales por hoja de un panel con splits (P2.11, T4). Descubre tanto
/// checkpoints como logs: un crash puede ocurrir antes del primer checkpoint.
type LeafHistories = Vec<(
    Option<Uuid>,
    String,
    Vec<crate::state::scrollback_log::Frame>,
)>;

fn collect_leaf_histories(dir: &std::path::Path, panel_id: Uuid) -> LeafHistories {
    use std::collections::BTreeSet;

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let prefix = format!("{}-", panel_id.simple());
    let mut leaves = BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix(&prefix).and_then(|rest| {
            rest.strip_suffix(".txt")
                .or_else(|| rest.strip_suffix(".mtlg"))
                .or_else(|| rest.strip_suffix(".gen"))
        }) else {
            continue;
        };
        let Ok(leaf) = Uuid::parse_str(rest) else {
            continue;
        };
        leaves.insert(leaf);
    }
    let mut out = Vec::new();
    for leaf in leaves {
        let text = crate::state::scrollback_store::load_leaf_scrollback(dir, panel_id, Some(leaf))
            .unwrap_or_default();
        let frames = load_leaf_session_frames(dir, panel_id, Some(leaf));
        let has_generation_marker = dir
            .join(
                crate::state::scrollback_store::scrollback_leaf_gen_file_name(panel_id, Some(leaf)),
            )
            .exists()
            || dir
                .join(crate::state::scrollback_store::scrollback_leaf_file_name(
                    panel_id,
                    Some(leaf),
                ))
                .exists();
        // Un marker sin contenido representa de forma durable una hoja vacía.
        // Es importante durante la migración: evita que un checkpoint legado
        // de raíz reaparezca después de que el usuario limpió la terminal.
        if !text.is_empty() || !frames.is_empty() || has_generation_marker {
            out.push((Some(leaf), text, frames));
        }
    }
    // Alias legado de la raíz. La clave estable `Some(root_leaf)` gana dentro
    // del panel si ambos formatos coexisten durante la migración.
    let legacy_root = crate::state::scrollback_store::load_leaf_scrollback(dir, panel_id, None)
        .unwrap_or_default();
    let legacy_frames = load_leaf_session_frames(dir, panel_id, None);
    let legacy_checkpoint_exists = dir
        .join(crate::state::scrollback_store::scrollback_leaf_file_name(
            panel_id, None,
        ))
        .exists();
    if !legacy_root.is_empty() || !legacy_frames.is_empty() || legacy_checkpoint_exists {
        out.insert(0, (None, legacy_root, legacy_frames));
    }
    out
}

/// Frames del log incremental que corresponden al checkpoint actual (P1.7).
/// Devuelve vacío si no hay log o si la generation no matchea.
#[cfg(test)]
fn load_session_frames(
    dir: &std::path::Path,
    panel_id: Uuid,
) -> Vec<crate::state::scrollback_log::Frame> {
    load_leaf_session_frames(dir, panel_id, None)
}

fn load_leaf_session_frames(
    dir: &std::path::Path,
    panel_id: Uuid,
    leaf_id: Option<Uuid>,
) -> Vec<crate::state::scrollback_log::Frame> {
    let log_path =
        dir.join(crate::state::scrollback_store::scrollback_leaf_log_file_name(panel_id, leaf_id));
    let Ok(bytes) = std::fs::read(log_path) else {
        return Vec::new();
    };
    let Some((log_gen, frames)) = crate::state::scrollback_log::read_frames(&bytes) else {
        return Vec::new();
    };
    if log_gen != read_leaf_generation(dir, panel_id, leaf_id) {
        return Vec::new();
    }
    frames
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

/// Smoke E2E de la app real montada sobre egui_kittest (Ship-it 7.4).
///
/// No es un test de píxeles: monta `TerminalApp` entero, lo corre por frames y
/// verifica el árbol de accesibilidad, que es lo que un usuario "ve". Cubre el
/// camino que ningún test unitario toca: construcción + update + render.
#[cfg(test)]
mod smoke_e2e {
    use egui_kittest::kittest::NodeT;

    use super::TerminalApp;

    /// Corre la app dentro del harness, opcionalmente inyectando eventos antes
    /// de los últimos frames, y devuelve las etiquetas de accesibilidad.
    fn run_app(frames: usize, events: Vec<egui::Event>) -> Vec<String> {
        let mut app: Option<TerminalApp> = None;
        let mut harness = egui_kittest::Harness::new(|ctx| {
            let app = app.get_or_insert_with(|| TerminalApp::new_for_tests(ctx));
            app.update_for_tests(ctx);
        });
        // La app tiene repaints periódicos legítimos (cursor y polling de los
        // workers). `Harness::run` exige que la UI llegue a reposo y por eso
        // se vuelve flaky bajo carga paralela; acá queremos exactamente N
        // frames, no esperar una quietud que una terminal viva no promete.
        harness.run_steps(frames);
        if !events.is_empty() {
            harness.input_mut().events.extend(events);
            harness.run_steps(2);
        }
        let root = harness.root();
        std::iter::once(root)
            .chain(root.children_recursive())
            .flat_map(|node| {
                let node = node.accesskit_node();
                [node.label(), node.value()].into_iter().flatten()
            })
            .collect()
    }

    fn contains_label(labels: &[String], expected: &str) -> bool {
        labels.iter().any(|label| label.contains(expected))
    }

    fn ctrl(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        }
    }

    #[test]
    fn a_fresh_app_with_its_default_terminal_does_not_show_an_empty_state() {
        // Sin estado en disco la app crea un shell en HOME. Aunque todavía no
        // haya una carpeta elegida, el canvas ya no está vacío y la invitación
        // a abrir una carpeta no debe dibujarse encima del terminal.
        let labels = run_app(3, Vec::new());
        assert!(
            !contains_label(&labels, "Todavía no hay carpeta abierta"),
            "el empty state quedó visible sobre el terminal:\n{labels:#?}"
        );
        let open_folder_actions = labels
            .iter()
            .filter(|label| label.as_str() == "Abrir carpeta")
            .count();
        assert_eq!(
            open_folder_actions, 1,
            "debe existir sólo la acción accesible del sidebar, no otra superpuesta:\n{labels:#?}"
        );
    }

    #[test]
    fn the_onboarding_overlay_shows_its_three_steps() {
        let labels = run_app(3, Vec::new());
        assert!(
            contains_label(&labels, "Primeros pasos"),
            "falta el overlay de onboarding:\n{labels:#?}"
        );
        assert!(contains_label(&labels, "1. Abrí una carpeta"));
        assert!(contains_label(&labels, "2. Abrí un terminal"));
        assert!(contains_label(&labels, "3. Revisá los cambios"));
        assert!(
            contains_label(&labels, "Entendido"),
            "falta el botón de cierre"
        );
    }

    #[test]
    fn ctrl_comma_opens_settings() {
        let before = run_app(3, Vec::new());
        assert!(
            !contains_label(&before, "Configuración"),
            "settings no debería arrancar abierto"
        );

        let after = run_app(3, vec![ctrl(egui::Key::Comma)]);
        assert!(
            contains_label(&after, "Configuración"),
            "Ctrl+, no abrió settings:\n{after:#?}"
        );
    }

    #[test]
    fn running_many_frames_never_panics_or_deadlocks() {
        // La regresión que esto caza: un frame que se cuelga tomando dos
        // locks, o que paniquea en el frame N por estado acumulado.
        let labels = run_app(20, Vec::new());
        assert!(!labels.is_empty());
    }
}
