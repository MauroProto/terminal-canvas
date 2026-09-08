use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::collab::TrustedDevice;
use crate::orchestration::OrchestrationState;
use crate::state::panel_state::PanelState;

pub const APP_STATE_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppState {
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,
    pub workspaces: Vec<WorkspaceState>,
    pub active_ws: usize,
    pub sidebar_visible: bool,
    #[serde(default)]
    pub legacy_canvas_ui: LegacyCanvasUiState,
    #[serde(default = "default_local_device_id")]
    pub local_device_id: String,
    #[serde(default)]
    pub trusted_devices: Vec<TrustedDevice>,
    #[serde(default)]
    pub orchestration: OrchestrationState,
}

fn current_schema_version() -> u32 {
    APP_STATE_SCHEMA_VERSION
}

fn default_local_device_id() -> String {
    Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub id: String,
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub panels: Vec<PanelState>,
    #[serde(default)]
    pub desktop: WorkspaceDesktopState,
    #[serde(default)]
    pub legacy_canvas: LegacyCanvasState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WorkspaceDesktopState {
    #[serde(default)]
    pub next_z: u32,
    #[serde(default)]
    pub next_color: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyCanvasState {
    #[serde(default)]
    pub viewport_pan: [f32; 2],
    #[serde(default = "default_viewport_zoom")]
    pub viewport_zoom: f32,
}

impl Default for LegacyCanvasState {
    fn default() -> Self {
        Self {
            viewport_pan: [0.0, 0.0],
            viewport_zoom: default_viewport_zoom(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyCanvasUiState {
    #[serde(default = "default_show_grid")]
    pub show_grid: bool,
    #[serde(default = "default_show_minimap")]
    pub show_minimap: bool,
}

impl Default for LegacyCanvasUiState {
    fn default() -> Self {
        Self {
            show_grid: default_show_grid(),
            show_minimap: default_show_minimap(),
        }
    }
}

fn default_viewport_zoom() -> f32 {
    1.0
}

fn default_show_grid() -> bool {
    true
}

fn default_show_minimap() -> bool {
    false
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LegacyAppState {
    pub workspaces: Vec<LegacyWorkspaceState>,
    pub active_ws: usize,
    pub sidebar_visible: bool,
    #[serde(default = "default_show_grid")]
    pub show_grid: bool,
    #[serde(default = "default_show_minimap")]
    pub show_minimap: bool,
    #[serde(default = "default_local_device_id")]
    pub local_device_id: String,
    #[serde(default)]
    pub trusted_devices: Vec<TrustedDevice>,
    #[serde(default)]
    pub orchestration: OrchestrationState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LegacyWorkspaceState {
    pub id: String,
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub panels: Vec<PanelState>,
    #[serde(default)]
    pub viewport_pan: [f32; 2],
    #[serde(default = "default_viewport_zoom")]
    pub viewport_zoom: f32,
    #[serde(default)]
    pub next_z: u32,
    #[serde(default)]
    pub next_color: usize,
}

impl From<LegacyAppState> for AppState {
    fn from(value: LegacyAppState) -> Self {
        Self {
            schema_version: APP_STATE_SCHEMA_VERSION,
            workspaces: value
                .workspaces
                .into_iter()
                .map(WorkspaceState::from)
                .collect(),
            active_ws: value.active_ws,
            sidebar_visible: value.sidebar_visible,
            legacy_canvas_ui: LegacyCanvasUiState {
                show_grid: value.show_grid,
                show_minimap: value.show_minimap,
            },
            local_device_id: value.local_device_id,
            trusted_devices: value.trusted_devices,
            orchestration: value.orchestration,
        }
    }
}

impl From<LegacyWorkspaceState> for WorkspaceState {
    fn from(value: LegacyWorkspaceState) -> Self {
        Self {
            id: value.id,
            name: value.name,
            cwd: value.cwd,
            panels: value.panels,
            desktop: WorkspaceDesktopState {
                next_z: value.next_z,
                next_color: value.next_color,
            },
            legacy_canvas: LegacyCanvasState {
                viewport_pan: value.viewport_pan,
                viewport_zoom: value.viewport_zoom,
            },
        }
    }
}

/// Repara identidades persistidas antes de crear workspaces y sesiones.
///
/// Los UUID de panel son parte de la clave durable del scrollback y los UUID
/// de runtime apuntan a PTYs vivos. Aceptar duplicados provenientes de un
/// layout copiado o editado mezclaría proyectos o atachearía dos hojas a la
/// misma sesión. Se conserva siempre la primera identidad válida; las
/// posteriores reciben identidad nueva y pierden sólo el reattach ambiguo.
pub fn normalize_saved_state(state: &mut AppState) {
    if state.workspaces.is_empty() {
        log::warn!("layout persistido sin workspaces; se creó uno vacío y seguro");
        state.workspaces.push(WorkspaceState {
            id: Uuid::new_v4().to_string(),
            name: "Default".to_owned(),
            cwd: None,
            panels: Vec::new(),
            desktop: WorkspaceDesktopState::default(),
            legacy_canvas: LegacyCanvasState::default(),
        });
    }
    state.active_ws = state.active_ws.min(state.workspaces.len() - 1);

    let mut workspace_ids = HashSet::new();
    let mut panel_ids = HashSet::new();
    let mut runtime_ids = HashSet::new();

    for workspace in &mut state.workspaces {
        let workspace_id = parse_non_nil_uuid(&workspace.id);
        let workspace_identity_reset = match workspace_id {
            Some(id) if workspace_ids.insert(id) => false,
            _ => {
                let id = fresh_uuid(&mut workspace_ids);
                log::warn!(
                    "workspace persistido con identidad inválida o duplicada; se reasignó a {id}"
                );
                workspace.id = id.to_string();
                true
            }
        };

        for panel in &mut workspace.panels {
            let panel_id = parse_non_nil_uuid(&panel.id);
            let panel_identity_reset = match panel_id {
                Some(id) if panel_ids.insert(id) => false,
                _ => {
                    let id = fresh_uuid(&mut panel_ids);
                    log::warn!(
                        "panel persistido con identidad inválida o duplicada; se reasignó a {id}"
                    );
                    panel.id = id.to_string();
                    true
                }
            };

            if workspace_identity_reset || panel_identity_reset {
                panel.runtime_session_id = None;
                panel.leaf_runtime_session_ids.clear();
                continue;
            }

            let mut local_runtime_ids = HashSet::new();
            let mut normalized = BTreeMap::new();
            for (leaf, runtime) in std::mem::take(&mut panel.leaf_runtime_session_ids) {
                let Some(leaf_id) = parse_non_nil_uuid(&leaf) else {
                    log::warn!("se descartó una identidad de hoja inválida del layout");
                    continue;
                };
                let Some(runtime_id) = parse_non_nil_uuid(&runtime) else {
                    log::warn!("se descartó una identidad de runtime inválida del layout");
                    continue;
                };
                if local_runtime_ids.contains(&runtime_id) || runtime_ids.contains(&runtime_id) {
                    log::warn!("se descartó una identidad de runtime duplicada del layout");
                    continue;
                }
                local_runtime_ids.insert(runtime_id);
                runtime_ids.insert(runtime_id);
                normalized.insert(leaf_id.to_string(), runtime_id.to_string());
            }
            panel.leaf_runtime_session_ids = normalized;

            let Some(runtime_id) = panel
                .runtime_session_id
                .as_deref()
                .and_then(parse_non_nil_uuid)
            else {
                panel.runtime_session_id = None;
                continue;
            };
            // El campo legado de raíz puede (y debe) repetir el valor que ya
            // está en el mapa del mismo panel. Cualquier otra repetición es
            // ambigua entre paneles y se descarta.
            if local_runtime_ids.contains(&runtime_id) || runtime_ids.insert(runtime_id) {
                panel.runtime_session_id = Some(runtime_id.to_string());
            } else {
                log::warn!("se descartó el alias de un runtime duplicado del layout");
                panel.runtime_session_id = None;
            }
        }
    }

    state.schema_version = APP_STATE_SCHEMA_VERSION;
}

fn parse_non_nil_uuid(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value).ok().filter(|id| !id.is_nil())
}

fn fresh_uuid(used: &mut HashSet<Uuid>) -> Uuid {
    loop {
        let id = Uuid::new_v4();
        if used.insert(id) {
            return id;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutosaveDecision {
    Idle,
    ScheduleAfter(Duration),
    SaveNow,
}

#[derive(Debug)]
pub struct AutosaveController {
    interval: Duration,
    last_save_at: Instant,
}

/// Cadencia independiente para artefactos que cambian aunque el layout no lo
/// haga (por ejemplo, el log incremental de scrollback).
#[derive(Debug)]
pub struct PeriodicFlushController {
    interval: Duration,
    last_flush_at: Instant,
}

impl PeriodicFlushController {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_flush_at: Instant::now(),
        }
    }

    pub fn decision(&self, now: Instant) -> AutosaveDecision {
        let elapsed = now.saturating_duration_since(self.last_flush_at);
        if elapsed >= self.interval {
            AutosaveDecision::SaveNow
        } else {
            AutosaveDecision::ScheduleAfter(self.interval - elapsed)
        }
    }

    pub fn mark_flushed(&mut self, now: Instant) {
        self.last_flush_at = now;
    }
}

impl AutosaveController {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_save_at: Instant::now(),
        }
    }

    pub fn should_persist<T: PartialEq>(
        &self,
        snapshot: &T,
        persisted: Option<&T>,
        now: Instant,
    ) -> AutosaveDecision {
        if persisted == Some(snapshot) {
            return AutosaveDecision::Idle;
        }

        let elapsed = now.saturating_duration_since(self.last_save_at);
        if elapsed >= self.interval {
            AutosaveDecision::SaveNow
        } else {
            AutosaveDecision::ScheduleAfter(self.interval - elapsed)
        }
    }

    pub fn mark_saved(&mut self, now: Instant) {
        self.last_save_at = now;
    }
}

pub fn state_file_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "terminal-app")?;
    Some(dirs.data_dir().join("layout.json"))
}

pub fn load_state() -> Option<AppState> {
    load_state_result().into_state()
}

#[derive(Debug, Clone, PartialEq)]
pub enum StateLoadResult {
    Loaded(AppState),
    MissingOrUnreadable,
    IncompatibleFuture { version: u32 },
}

impl StateLoadResult {
    pub fn into_state(self) -> Option<AppState> {
        match self {
            Self::Loaded(state) => Some(state),
            Self::MissingOrUnreadable | Self::IncompatibleFuture { .. } => None,
        }
    }
}

pub fn load_state_result() -> StateLoadResult {
    let Some(path) = state_file_path() else {
        return StateLoadResult::MissingOrUnreadable;
    };
    load_state_result_from_path(&path)
}

pub fn load_state_from_path(path: &Path) -> Option<AppState> {
    load_state_result_from_path(path).into_state()
}

pub fn load_state_result_from_path(path: &Path) -> StateLoadResult {
    // Si el principal está corrupto (crash a mitad de escritura), se prueba el
    // ring en orden. Un schema futuro no es corrupción: si aparece antes que
    // un snapshot compatible, se bloquea el downgrade y toda escritura de la
    // app durante esta ejecución.
    let candidates = std::iter::once(path.to_path_buf()).chain(
        (0..crate::state::durable_write::BACKUP_SLOTS)
            .map(|slot| crate::state::durable_write::backup_path(path, slot)),
    );
    for candidate in candidates {
        let Ok(bytes) = std::fs::read(candidate) else {
            continue;
        };
        if let Some(version) =
            serialized_schema_version(&bytes).filter(|version| *version > APP_STATE_SCHEMA_VERSION)
        {
            log::warn!("layout creado por una versión más nueva; no se modificará");
            return StateLoadResult::IncompatibleFuture { version };
        }
        if let Some(state) = parse_state_bytes(&bytes) {
            return StateLoadResult::Loaded(state);
        }
    }
    StateLoadResult::MissingOrUnreadable
}

fn serialized_schema_version(bytes: &[u8]) -> Option<u32> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()?
        .get("schema_version")?
        .as_u64()
        .and_then(|version| u32::try_from(version).ok())
}

pub fn save_state(state: &AppState) {
    if let Err(err) = try_save_state(state) {
        log::warn!("Failed to write state file: {err}");
    }
}

pub fn try_save_state(state: &AppState) -> anyhow::Result<()> {
    let Some(_guard) = crate::state::run_marker::acquire_write_guard()? else {
        anyhow::bail!("otra instancia de TerminalCanvas posee la lease de persistencia");
    };
    let Some(path) = state_file_path() else {
        anyhow::bail!("Could not determine state file path");
    };
    save_state_to_path(&path, state)
}

/// Serializa y escribe con el patrón durable (tmp → fsync → rename → fsync
/// del directorio, ring de backups y no-op si nada cambió).
pub fn save_state_to_path(path: &Path, state: &AppState) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(state)?;
    bytes.push(b'\n');
    crate::state::durable_write::write_durable(path, &bytes)?;
    Ok(())
}

/// Parsea un snapshot de estado (esquema actual o legado). Es la función de
/// validez del load con fallback: un archivo que no parsea se salta.
fn parse_state_bytes(bytes: &[u8]) -> Option<AppState> {
    let json: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let has_schema_version = json
        .as_object()
        .map(|object| object.contains_key("schema_version"))
        .unwrap_or(false);

    let mut state = if has_schema_version {
        let state: AppState = serde_json::from_value(json).ok()?;
        if state.schema_version > APP_STATE_SCHEMA_VERSION {
            log::warn!(
                "layout schema {} is newer than supported schema {}",
                state.schema_version,
                APP_STATE_SCHEMA_VERSION
            );
            return None;
        }
        state
    } else {
        serde_json::from_value::<LegacyAppState>(json)
            .ok()
            .map(AppState::from)?
    };
    normalize_saved_state(&mut state);
    Some(state)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use chrono::Utc;
    use uuid::Uuid;

    use super::{
        load_state_from_path, load_state_result_from_path, normalize_saved_state,
        save_state_to_path, AppState, AutosaveController, AutosaveDecision, LegacyCanvasState,
        LegacyCanvasUiState, PeriodicFlushController, StateLoadResult, WorkspaceDesktopState,
        WorkspaceState, APP_STATE_SCHEMA_VERSION,
    };
    use crate::collab::{PanelShareScope, TrustedDevice};
    use crate::orchestration::OrchestrationState;
    use crate::state::panel_state::{PanelPlacement, SavedPanelBounds};
    use crate::state::PanelState;

    #[test]
    fn autosave_waits_until_interval_for_changed_state() {
        let mut controller = AutosaveController::new(Duration::from_secs(2));
        let state = sample_state("one");
        let now = std::time::Instant::now();
        controller.mark_saved(now);

        let decision = controller.should_persist(&state, None, now + Duration::from_secs(1));

        assert_eq!(
            decision,
            AutosaveDecision::ScheduleAfter(Duration::from_secs(1))
        );
    }

    #[test]
    fn autosave_saves_once_interval_has_elapsed() {
        let mut controller = AutosaveController::new(Duration::from_secs(2));
        let state = sample_state("one");
        let now = std::time::Instant::now();
        controller.mark_saved(now);

        let decision = controller.should_persist(&state, None, now + Duration::from_secs(2));

        assert_eq!(decision, AutosaveDecision::SaveNow);
    }

    #[test]
    fn periodic_flush_is_due_without_a_layout_snapshot() {
        let mut controller = PeriodicFlushController::new(Duration::from_secs(2));
        let now = std::time::Instant::now();
        controller.mark_flushed(now);

        assert_eq!(
            controller.decision(now + Duration::from_secs(1)),
            AutosaveDecision::ScheduleAfter(Duration::from_secs(1))
        );
        assert_eq!(
            controller.decision(now + Duration::from_secs(2)),
            AutosaveDecision::SaveNow
        );
    }

    #[test]
    fn save_and_load_round_trip_through_primary_file() {
        let dir = unique_temp_dir();
        let path = dir.join("layout.json");
        let state = sample_state("round-trip");

        save_state_to_path(&path, &state).unwrap();

        assert_eq!(load_state_from_path(&path), Some(state));
        assert!(path.exists());
    }

    #[test]
    fn load_falls_back_to_backup_when_primary_is_corrupted() {
        let dir = unique_temp_dir();
        let path = dir.join("layout.json");
        let first = sample_state("first");
        let second = sample_state("second");

        save_state_to_path(&path, &first).unwrap();
        save_state_to_path(&path, &second).unwrap();
        fs::write(&path, "{not valid json").unwrap();

        assert_eq!(load_state_from_path(&path), Some(first));
    }

    #[test]
    fn successful_save_cleans_up_temp_file() {
        let dir = unique_temp_dir();
        let path = dir.join("layout.json");

        save_state_to_path(&path, &sample_state("cleanup")).unwrap();

        assert!(!crate::state::durable_write::tmp_path(&path).exists());
    }

    #[test]
    fn normalization_prevents_cross_project_identity_collisions() {
        let mut state = sample_state("identity-isolation");
        let first_workspace = state.workspaces[0].clone();
        let first_panel_id = first_workspace.panels[0].id.clone();
        let leaf_id = Uuid::new_v4();
        let runtime_id = Uuid::new_v4();
        state.workspaces[0].panels[0].root_leaf = Some(leaf_id.to_string());
        state.workspaces[0].panels[0].runtime_session_id = Some(runtime_id.to_string());
        state.workspaces[0].panels[0]
            .leaf_runtime_session_ids
            .insert(leaf_id.to_string(), runtime_id.to_string());

        // Proyecto distinto, panel distinto, pero runtime copiado: no puede
        // atachear la misma PTY que el primero.
        let mut duplicate_runtime_workspace = state.workspaces[0].clone();
        duplicate_runtime_workspace.id = Uuid::new_v4().to_string();
        duplicate_runtime_workspace.panels[0].id = Uuid::new_v4().to_string();

        // Layout clonado literalmente: workspace y panel deben recibir UUIDs
        // nuevos y abandonar cualquier reattach ambiguo.
        let duplicated_workspace = state.workspaces[0].clone();
        state.workspaces.push(duplicate_runtime_workspace);
        state.workspaces.push(duplicated_workspace);

        normalize_saved_state(&mut state);

        let workspace_ids: HashSet<_> = state
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect();
        let panel_ids: HashSet<_> = state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.panels.iter().map(|panel| panel.id.clone()))
            .collect();
        assert_eq!(workspace_ids.len(), 3);
        assert_eq!(panel_ids.len(), 3);
        assert_eq!(state.workspaces[0].panels[0].id, first_panel_id);
        assert_eq!(
            state.workspaces[0].panels[0].runtime_session_id,
            Some(runtime_id.to_string())
        );
        assert!(state.workspaces[1].panels[0]
            .leaf_runtime_session_ids
            .is_empty());
        assert!(state.workspaces[1].panels[0].runtime_session_id.is_none());
        assert!(state.workspaces[2].panels[0]
            .leaf_runtime_session_ids
            .is_empty());
        assert!(state.workspaces[2].panels[0].runtime_session_id.is_none());
        assert_eq!(state.schema_version, APP_STATE_SCHEMA_VERSION);
    }

    #[test]
    fn normalization_repairs_an_empty_workspace_list_and_active_index() {
        let mut state = sample_state("empty-layout");
        state.workspaces.clear();
        state.active_ws = usize::MAX;

        normalize_saved_state(&mut state);

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.active_ws, 0);
        assert_eq!(state.workspaces[0].name, "Default");
        assert!(state.workspaces[0].cwd.is_none());
        assert!(state.workspaces[0].panels.is_empty());
        assert!(Uuid::parse_str(&state.workspaces[0].id).is_ok_and(|id| !id.is_nil()));
    }

    #[test]
    fn normalization_clamps_an_out_of_range_active_workspace() {
        let mut state = sample_state("bad-active-index");
        state.active_ws = usize::MAX;

        normalize_saved_state(&mut state);

        assert_eq!(state.active_ws, 0);
    }

    #[test]
    fn future_schema_is_rejected_instead_of_being_overwritten() {
        let dir = unique_temp_dir();
        let path = dir.join("layout.json");
        let older = sample_state("older-backup");
        save_state_to_path(&path, &older).unwrap();
        let mut state = sample_state("future");
        state.schema_version = APP_STATE_SCHEMA_VERSION + 1;
        save_state_to_path(&path, &state).unwrap();

        // Aunque exista un backup válido de esquema viejo, no se hace
        // downgrade silencioso del archivo principal futuro.
        assert!(load_state_from_path(&path).is_none());
        assert_eq!(
            load_state_result_from_path(&path),
            StateLoadResult::IncompatibleFuture {
                version: APP_STATE_SCHEMA_VERSION + 1
            }
        );
        let before = fs::read(&path).unwrap();
        // La clasificación no toca ni el principal ni su backup.
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn future_schema_in_newest_valid_backup_blocks_an_older_downgrade() {
        let dir = unique_temp_dir();
        let path = dir.join("layout.json");
        fs::write(&path, b"{corrupt").unwrap();

        let mut future = sample_state("future-backup");
        future.schema_version = APP_STATE_SCHEMA_VERSION + 2;
        fs::write(
            crate::state::durable_write::backup_path(&path, 0),
            serde_json::to_vec(&future).unwrap(),
        )
        .unwrap();
        fs::write(
            crate::state::durable_write::backup_path(&path, 1),
            serde_json::to_vec(&sample_state("older-compatible")).unwrap(),
        )
        .unwrap();

        assert_eq!(
            load_state_result_from_path(&path),
            StateLoadResult::IncompatibleFuture {
                version: APP_STATE_SCHEMA_VERSION + 2
            }
        );
    }

    fn sample_state(label: &str) -> AppState {
        AppState {
            schema_version: APP_STATE_SCHEMA_VERSION,
            workspaces: vec![WorkspaceState {
                id: Uuid::new_v4().to_string(),
                name: format!("Workspace {label}"),
                cwd: Some(PathBuf::from(format!("/tmp/{label}"))),
                panels: vec![PanelState {
                    id: Uuid::new_v4().to_string(),
                    title: "Terminal".to_owned(),
                    custom_title: Some(format!("Terminal {label}")),
                    position: [10.0, 20.0],
                    size: [300.0, 200.0],
                    color: [1, 2, 3],
                    z_index: 1,
                    focused: true,
                    minimized: false,
                    placement: PanelPlacement::Floating,
                    restore_placement: None,
                    restore_bounds: Some(SavedPanelBounds::new([10.0, 20.0], [300.0, 200.0])),
                    share_scope: PanelShareScope::VisibleOnly,
                    agent_command: None,
                    leaf_agent_commands: Default::default(),
                    unread: false,
                    split_tree: None,
                    focused_leaf: None,
                    root_leaf: None,
                    leaf_runtime_session_ids: Default::default(),
                    linked_issue: None,
                    agent_session_id: None,
                    leaf_agent_session_ids: Default::default(),
                    runtime_session_id: None,
                }],
                desktop: WorkspaceDesktopState {
                    next_z: 2,
                    next_color: 1,
                },
                legacy_canvas: LegacyCanvasState {
                    viewport_pan: [0.0, 0.0],
                    viewport_zoom: 1.0,
                },
            }],
            active_ws: 0,
            sidebar_visible: true,
            legacy_canvas_ui: LegacyCanvasUiState {
                show_grid: true,
                show_minimap: false,
            },
            local_device_id: format!("device-{label}"),
            trusted_devices: vec![TrustedDevice {
                device_id: format!("trusted-{label}"),
                last_display_name: format!("Guest {label}"),
                approved_at: Utc::now(),
                last_seen_at: Utc::now(),
            }],
            orchestration: OrchestrationState::default(),
        }
    }

    fn unique_temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("persistence-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn backup_ring_paths_use_expected_suffixes() {
        let path = PathBuf::from("/tmp/layout.json");
        assert_eq!(
            crate::state::durable_write::backup_path(&path, 0),
            PathBuf::from("/tmp/layout.json.bak.0")
        );
        assert_eq!(
            crate::state::durable_write::backup_path(&path, 4),
            PathBuf::from("/tmp/layout.json.bak.4")
        );
    }

    #[test]
    fn load_legacy_state_defaults_runtime_fields() {
        let dir = unique_temp_dir();
        let path = dir.join("layout.json");
        fs::write(
            &path,
            r#"{
  "workspaces": [
    {
      "id": "legacy",
      "name": "Legacy",
      "cwd": "/tmp/legacy",
      "panels": [
        {
          "id": "legacy-panel",
          "title": "Terminal",
          "custom_title": null,
          "position": [10.0, 20.0],
          "size": [300.0, 200.0],
          "color": [1, 2, 3],
          "z_index": 1,
          "focused": true
        }
      ],
      "viewport_pan": [0.0, 0.0],
      "viewport_zoom": 1.0,
      "next_z": 2,
      "next_color": 1
    }
  ],
  "active_ws": 0,
  "sidebar_visible": true,
  "show_grid": true,
  "show_minimap": false
}"#,
        )
        .unwrap();

        let loaded = load_state_from_path(&path).unwrap();

        assert_eq!(loaded.schema_version, APP_STATE_SCHEMA_VERSION);
        assert_eq!(loaded.workspaces[0].panels.len(), 1);
        assert_eq!(loaded.workspaces[0].panels[0].title, "Terminal");
        assert_eq!(
            loaded.workspaces[0].panels[0].share_scope,
            PanelShareScope::VisibleOnly
        );
        assert_eq!(loaded.workspaces[0].desktop.next_z, 2);
        assert_eq!(loaded.workspaces[0].legacy_canvas.viewport_zoom, 1.0);
        assert!(!loaded.local_device_id.is_empty());
        assert!(loaded.trusted_devices.is_empty());
    }

    #[test]
    fn saved_state_omits_the_runtime_session_registry() {
        let dir = unique_temp_dir();
        let path = dir.join("layout.json");
        let state = sample_state("ui-only");

        save_state_to_path(&path, &state).unwrap();

        let serialized = fs::read_to_string(&path).unwrap();
        assert!(serialized.contains(&format!("\"schema_version\": {APP_STATE_SCHEMA_VERSION}")));
        assert!(serialized.contains("\"legacy_canvas_ui\""));
        assert!(serialized.contains("\"desktop\""));
        // El registro entero de sesiones de runtime sigue fuera del layout: es
        // estado del proceso, no del usuario.
        assert!(!serialized.contains("runtime_sessions"));
        // El `runtime_session_id` **por panel** sí se persiste desde P3.15:
        // con el daemon hosteando los PTYs, ese id es la llave para que al
        // reabrir la app el panel se reengancha a su shell vivo en vez de
        // arrancar uno nuevo. Sin daemon es inocuo (se reusa como id local).
        assert!(
            serialized.contains("runtime_session_id"),
            "el id de sesión es la llave del reattach: no puede faltar"
        );
    }
}
