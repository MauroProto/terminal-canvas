use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::collab::TrustedDevice;
use crate::orchestration::OrchestrationState;
use crate::state::panel_state::PanelState;

pub const APP_STATE_SCHEMA_VERSION: u32 = 3;

// Layout metadata has a separate symmetric serialized-size limit. Terminal
// history and repository diff-note files are persisted separately.
const MAX_LAYOUT_FILE_BYTES: usize = 16 * 1024 * 1024;

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
    Some(crate::utils::app_paths::data_dir()?.join("layout.json"))
}

pub fn load_state() -> Option<AppState> {
    load_state_result().into_state()
}

#[derive(Debug, Clone, PartialEq)]
pub enum StateLoadResult {
    Loaded(AppState),
    /// No compatible snapshot was found among missing or readable-invalid files.
    MissingOrUnreadable,
    /// An earlier snapshot could not be read. Its contents and schema are unknown.
    ReadError {
        path: PathBuf,
        error: String,
    },
    IncompatibleFuture {
        version: u32,
    },
}

impl StateLoadResult {
    pub fn into_state(self) -> Option<AppState> {
        match self {
            Self::Loaded(state) => Some(state),
            Self::MissingOrUnreadable
            | Self::ReadError { .. }
            | Self::IncompatibleFuture { .. } => None,
        }
    }
}

pub fn load_state_result() -> StateLoadResult {
    let Some(path) = state_file_path() else {
        return StateLoadResult::ReadError {
            path: PathBuf::from("layout.json"),
            error: "Could not determine the saved profile directory".to_owned(),
        };
    };
    load_state_result_from_path(&path)
}

pub fn load_state_from_path(path: &Path) -> Option<AppState> {
    load_state_result_from_path(path).into_state()
}

pub fn load_state_result_from_path(path: &Path) -> StateLoadResult {
    load_state_result_from_path_with_limit(path, MAX_LAYOUT_FILE_BYTES)
}

fn load_state_result_from_path_with_limit(path: &Path, limit: usize) -> StateLoadResult {
    load_state_result_with_reader(path, |candidate| read_state_bytes(candidate, limit))
}

fn load_state_result_with_reader(
    path: &Path,
    mut read: impl FnMut(&Path) -> std::io::Result<Option<Vec<u8>>>,
) -> StateLoadResult {
    // Readable-invalid snapshots may fall back in order. Missing files may be
    // skipped, but an unknown earlier snapshot must never authorize replacing
    // it from an older backup, including when its schema cannot be inspected.
    let candidates = std::iter::once(path.to_path_buf()).chain(
        (0..crate::state::durable_write::BACKUP_SLOTS)
            .map(|slot| crate::state::durable_write::backup_path(path, slot)),
    );
    for candidate in candidates {
        let bytes = match read(&candidate) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => continue,
            Err(error) => {
                return StateLoadResult::ReadError {
                    path: candidate,
                    error: error.to_string(),
                };
            }
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

fn layout_size_error(limit: usize) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("El layout supera el límite de {limit} bytes"),
    )
}

fn retry_layout_interrupted<T>(
    mut operation: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    loop {
        match operation() {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

fn read_state_bytes(path: &Path, limit: usize) -> std::io::Result<Option<Vec<u8>>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Inspect FIFO/device descriptor metadata without waiting for a peer.
        // Regular-file links remain compatible with historical layouts.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match retry_layout_interrupted(|| options.open(path)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A dangling link exists even though open returns NotFound. Only
            // an absent directory entry permits falling back to older files.
            return match retry_layout_interrupted(|| std::fs::symlink_metadata(path)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Ok(_) => Err(error),
                Err(error) => Err(error),
            };
        }
        Err(error) => return Err(error),
    };
    let metadata = retry_layout_interrupted(|| file.metadata())?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "El layout debe estar en un archivo regular",
        ));
    }
    read_state_bytes_with_limit(file, metadata.len(), limit).map(Some)
}

fn read_state_bytes_with_limit(
    reader: impl Read,
    declared_len: u64,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    if declared_len > limit as u64 {
        return Err(layout_size_error(limit));
    }
    let sentinel_limit = limit.checked_add(1).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Límite de layout inválido",
        )
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(declared_len as usize)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::OutOfMemory, error))?;
    // Take limits growth to one sentinel byte. Read::read_to_end retries
    // Interrupted; every other read/EOF error propagates without parsing.
    reader.take(sentinel_limit as u64).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(layout_size_error(limit));
    }
    Ok(bytes)
}

struct LimitedLayoutBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for LimitedLayoutBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(layout_size_error(self.limit));
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::OutOfMemory, error))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialize_state_with_limit(state: &AppState, limit: usize) -> anyhow::Result<Vec<u8>> {
    use anyhow::Context;
    let mut buffer = LimitedLayoutBuffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer_pretty(&mut buffer, state).with_context(|| {
        format!("No se pudo serializar el layout: el límite es de {limit} bytes")
    })?;
    buffer.write_all(b"\n").with_context(|| {
        format!("El layout con su salto final supera el límite de {limit} bytes")
    })?;
    Ok(buffer.bytes)
}

fn save_state_to_path_with_limit(
    path: &Path,
    state: &AppState,
    limit: usize,
) -> anyhow::Result<()> {
    // Complete bounded serialization, including the historical final LF,
    // before creating directories, temporaries or rotating any backup.
    let bytes = serialize_state_with_limit(state, limit)?;
    crate::state::durable_write::write_durable(path, &bytes)?;
    Ok(())
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
    save_state_to_path_with_limit(path, state, MAX_LAYOUT_FILE_BYTES)
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
    fn a_missing_layout_is_distinct_from_an_unreadable_existing_snapshot() {
        let dir = unique_temp_dir();
        let path = dir.join("layout.json");
        assert_eq!(
            load_state_result_from_path(&path),
            StateLoadResult::MissingOrUnreadable
        );
        fs::create_dir(&path).unwrap();
        save_state_to_path(
            &crate::state::durable_write::backup_path(&path, 0),
            &sample_state("readable-but-older"),
        )
        .unwrap();
        assert!(matches!(
            load_state_result_from_path(&path),
            StateLoadResult::ReadError { path: failed, .. } if failed == path
        ));
        assert!(path.is_dir());
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_primary_cannot_authorize_replacing_it_from_an_older_backup() {
        use std::os::windows::fs::OpenOptionsExt;

        for with_backup in [false, true] {
            let dir = unique_temp_dir();
            let path = dir.join("layout.json");
            save_state_to_path(&path, &sample_state("newer-unread-layout")).unwrap();
            let original = fs::read(&path).unwrap();
            let backup = crate::state::durable_write::backup_path(&path, 0);
            if with_backup {
                save_state_to_path(&backup, &sample_state("older-readable-backup")).unwrap();
            }
            let backup_bytes = fs::read(&backup).ok();
            // Windows can deny reading while allowing replacement/delete.
            // Treating this as NotFound would enable a destructive fresh save.
            let held = fs::OpenOptions::new()
                .read(true)
                .share_mode(4)
                .open(&path)
                .unwrap();
            assert!(matches!(
                load_state_result_from_path(&path),
                StateLoadResult::ReadError { path: failed, .. } if failed == path
            ));
            drop(held);
            assert_eq!(fs::read(&path).unwrap(), original);
            assert_eq!(fs::read(&backup).ok(), backup_bytes);
        }
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
                    leaf_memory_task_ids: Default::default(),
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

#[cfg(test)]
mod bounded_layout_tests {
    use std::io::{self, Read};
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use super::{
        load_state_result_from_path_with_limit, load_state_result_with_reader,
        read_state_bytes_with_limit, retry_layout_interrupted, save_state_to_path_with_limit,
        serialize_state_with_limit, AppState, LegacyCanvasState, LegacyCanvasUiState,
        StateLoadResult, WorkspaceDesktopState, WorkspaceState, APP_STATE_SCHEMA_VERSION,
        MAX_LAYOUT_FILE_BYTES,
    };
    use crate::orchestration::{OrchestrationState, TaskCard, TaskState};
    use crate::state::durable_write::{backup_path, BACKUP_SLOTS};

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("tc-layout-bounded-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct ReadonlyGuard {
        path: PathBuf,
        original: std::fs::Permissions,
    }

    impl ReadonlyGuard {
        fn new(path: &Path) -> Self {
            let original = std::fs::metadata(path).unwrap().permissions();
            let mut readonly = original.clone();
            readonly.set_readonly(true);
            std::fs::set_permissions(path, readonly).unwrap();
            Self {
                path: path.to_path_buf(),
                original,
            }
        }
    }

    impl Drop for ReadonlyGuard {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.path, self.original.clone());
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Entry {
        kind: &'static str,
        bytes: Option<Vec<u8>>,
        link: Option<PathBuf>,
        modified: SystemTime,
        readonly: bool,
        #[cfg(unix)]
        mode: u32,
        #[cfg(unix)]
        inode: u64,
    }

    fn snapshot(root: &Path) -> Vec<(PathBuf, Entry)> {
        fn visit(root: &Path, path: &Path, entries: &mut Vec<(PathBuf, Entry)>) {
            let metadata = std::fs::symlink_metadata(path).unwrap();
            let is_link = metadata.file_type().is_symlink();
            let kind = if is_link {
                "link"
            } else if metadata.is_file() {
                "file"
            } else if metadata.is_dir() {
                "directory"
            } else {
                "special"
            };
            #[cfg(unix)]
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            entries.push((
                path.strip_prefix(root).unwrap().to_path_buf(),
                Entry {
                    kind,
                    bytes: (kind == "file").then(|| std::fs::read(path).unwrap()),
                    link: is_link.then(|| std::fs::read_link(path).unwrap()),
                    modified: metadata.modified().unwrap(),
                    readonly: metadata.permissions().readonly(),
                    #[cfg(unix)]
                    mode: metadata.permissions().mode(),
                    #[cfg(unix)]
                    inode: metadata.ino(),
                },
            ));
            if kind == "directory" {
                for child in std::fs::read_dir(path).unwrap() {
                    visit(root, &child.unwrap().path(), entries);
                }
            }
        }
        let mut entries = Vec::new();
        visit(root, root, &mut entries);
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    fn sample_state(label: &str) -> AppState {
        let workspace_id = uuid::Uuid::new_v4();
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-07T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        AppState {
            schema_version: APP_STATE_SCHEMA_VERSION,
            workspaces: vec![WorkspaceState {
                id: workspace_id.to_string(),
                name: format!("{label} café 🦀 \"quoted\"\r\nline\\path"),
                cwd: Some(PathBuf::from("/synthetic/Unicode café")),
                panels: Vec::new(),
                desktop: WorkspaceDesktopState::default(),
                legacy_canvas: LegacyCanvasState::default(),
            }],
            active_ws: 0,
            sidebar_visible: true,
            legacy_canvas_ui: LegacyCanvasUiState::default(),
            local_device_id: "synthetic-layout-device".to_owned(),
            trusted_devices: Vec::new(),
            orchestration: OrchestrationState {
                tasks: vec![TaskCard {
                    id: uuid::Uuid::new_v4(),
                    workspace_id,
                    title: "Synthetic task 🦀".to_owned(),
                    brief: "metadata \"quote\"\\path\r\nsecond line".to_owned(),
                    state: TaskState::ReviewReady,
                    provider_hint: None,
                    session_ids: Vec::new(),
                    conflict_risk: false,
                    created_at: now,
                    updated_at: now,
                }],
                ..OrchestrationState::default()
            },
        }
    }

    fn pretty_bytes(state: &AppState) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(state).unwrap();
        bytes.push(b'\n');
        assert!(bytes.len() < 32 * 1024, "fixtures must remain small");
        bytes
    }

    struct ProbeReader<'a> {
        bytes: &'a [u8],
        offset: usize,
        calls: usize,
        interruptions: usize,
        fail_at: Option<usize>,
        failure_kind: io::ErrorKind,
        max_read: usize,
    }

    impl<'a> ProbeReader<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Self {
                bytes,
                offset: 0,
                calls: 0,
                interruptions: 0,
                fail_at: None,
                failure_kind: io::ErrorKind::Other,
                max_read: usize::MAX,
            }
        }
    }

    impl Read for ProbeReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if buffer.is_empty() {
                return Ok(0);
            }
            self.calls += 1;
            if self.interruptions > 0 {
                self.interruptions -= 1;
                return Err(io::ErrorKind::Interrupted.into());
            }
            if self.fail_at.is_some_and(|at| self.offset >= at) {
                return Err(io::Error::new(
                    self.failure_kind,
                    "synthetic layout read failure",
                ));
            }
            let before_failure = self
                .fail_at
                .map_or(usize::MAX, |at| at.saturating_sub(self.offset));
            let count = buffer
                .len()
                .min(self.bytes.len() - self.offset)
                .min(self.max_read)
                .min(before_failure);
            buffer[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
            self.offset += count;
            Ok(count)
        }
    }

    fn expect_read_error(result: StateLoadResult, expected_path: &Path) -> String {
        match result {
            StateLoadResult::ReadError { path, error } => {
                assert_eq!(path, expected_path);
                error
            }
            other => panic!("unknown earlier snapshot must block fallback, got {other:?}"),
        }
    }

    #[test]
    fn bounded_layout_round_trip_counts_pretty_json_and_the_final_newline_exactly() {
        assert_eq!(MAX_LAYOUT_FILE_BYTES, 16 * 1024 * 1024);
        let directory = TempDirectory::new();
        let path = directory.path().join("layout.json");
        let state = sample_state("exact boundary");
        let bytes = pretty_bytes(&state);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("café 🦀"));
        assert!(text.contains("\\\"quoted\\\"\\r\\nline\\\\path"));
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(
            serialize_state_with_limit(&state, bytes.len()).unwrap(),
            bytes
        );
        save_state_to_path_with_limit(&path, &state, bytes.len()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            load_state_result_from_path_with_limit(&path, bytes.len()),
            StateLoadResult::Loaded(state)
        );
    }

    #[test]
    fn bounded_layout_save_rejects_body_or_final_newline_overflow_before_any_io() {
        let directory = TempDirectory::new();
        let path = directory.path().join("layout.json");
        let older = sample_state("older");
        let older_bytes = pretty_bytes(&older);
        std::fs::write(&path, &older_bytes).unwrap();
        for slot in 0..BACKUP_SLOTS {
            std::fs::write(backup_path(&path, slot), format!("backup {slot}")).unwrap();
        }
        let mut newer = sample_state("newer");
        newer.orchestration.tasks[0].brief = "🦀\\\"\r\nmetadata".repeat(40);
        let newer_bytes = pretty_bytes(&newer);
        let before = snapshot(directory.path());
        for limit in [newer_bytes.len() - 2, newer_bytes.len() - 1] {
            let error = save_state_to_path_with_limit(&path, &newer, limit).unwrap_err();
            assert!(format!("{error:#}").contains(&limit.to_string()));
            assert_eq!(snapshot(directory.path()), before);
            let absent = directory.path().join("not-created").join("layout.json");
            assert!(save_state_to_path_with_limit(&absent, &newer, limit).is_err());
            assert!(!absent.parent().unwrap().exists());
            assert_eq!(snapshot(directory.path()), before);
        }
    }

    #[test]
    fn bounded_layout_missing_and_readable_invalid_snapshots_keep_ordered_fallback() {
        let directory = TempDirectory::new();
        let path = directory.path().join("layout.json");
        let state = sample_state("compatible backup");
        let bytes = pretty_bytes(&state);
        assert_eq!(
            load_state_result_from_path_with_limit(&path, bytes.len()),
            StateLoadResult::MissingOrUnreadable
        );
        std::fs::write(&path, b"{invalid primary").unwrap();
        std::fs::write(backup_path(&path, 1), b"[]").unwrap();
        std::fs::write(backup_path(&path, 2), &bytes).unwrap();
        let before = snapshot(directory.path());
        assert_eq!(
            load_state_result_from_path_with_limit(&path, bytes.len()),
            StateLoadResult::Loaded(state)
        );
        assert_eq!(snapshot(directory.path()), before);
    }

    #[test]
    fn bounded_layout_oversized_primary_blocks_readable_older_backups() {
        let directory = TempDirectory::new();
        let path = directory.path().join("layout.json");
        let older = sample_state("older backup");
        let older_bytes = pretty_bytes(&older);
        let mut primary = sample_state("unknown larger primary");
        primary.schema_version = APP_STATE_SCHEMA_VERSION + 10;
        primary.orchestration.tasks[0].brief = "metadata café 🦀".repeat(100);
        std::fs::write(&path, pretty_bytes(&primary)).unwrap();
        std::fs::write(backup_path(&path, 0), &older_bytes).unwrap();
        let before = snapshot(directory.path());
        let error = expect_read_error(
            load_state_result_from_path_with_limit(&path, older_bytes.len()),
            &path,
        );
        assert!(error.contains(&older_bytes.len().to_string()));
        assert_eq!(snapshot(directory.path()), before);
    }

    #[test]
    fn bounded_layout_first_oversized_backup_blocks_later_compatible_snapshots() {
        let directory = TempDirectory::new();
        let path = directory.path().join("layout.json");
        let older = sample_state("older later backup");
        let older_bytes = pretty_bytes(&older);
        let mut unread = sample_state("unknown earlier backup");
        unread.orchestration.tasks[0].brief = "metadata".repeat(200);
        let first = backup_path(&path, 0);
        std::fs::write(&path, b"{invalid primary").unwrap();
        std::fs::write(&first, pretty_bytes(&unread)).unwrap();
        std::fs::write(backup_path(&path, 1), &older_bytes).unwrap();
        let before = snapshot(directory.path());
        expect_read_error(
            load_state_result_from_path_with_limit(&path, older_bytes.len()),
            &first,
        );
        assert_eq!(snapshot(directory.path()), before);
    }

    #[test]
    fn bounded_layout_future_primary_or_backup_still_blocks_schema_downgrade() {
        for future_in_backup in [false, true] {
            let directory = TempDirectory::new();
            let path = directory.path().join("layout.json");
            let older = sample_state("compatible but older");
            let older_bytes = pretty_bytes(&older);
            let mut future = sample_state("future");
            future.schema_version = APP_STATE_SCHEMA_VERSION + 2;
            let future_bytes = pretty_bytes(&future);
            let limit = older_bytes.len().max(future_bytes.len());
            if future_in_backup {
                std::fs::write(&path, b"{invalid primary").unwrap();
                std::fs::write(backup_path(&path, 0), &future_bytes).unwrap();
                std::fs::write(backup_path(&path, 1), &older_bytes).unwrap();
            } else {
                std::fs::write(&path, &future_bytes).unwrap();
                std::fs::write(backup_path(&path, 0), &older_bytes).unwrap();
            }
            let before = snapshot(directory.path());
            assert_eq!(
                load_state_result_from_path_with_limit(&path, limit),
                StateLoadResult::IncompatibleFuture {
                    version: APP_STATE_SCHEMA_VERSION + 2
                }
            );
            assert_eq!(snapshot(directory.path()), before);
        }
    }

    #[test]
    fn bounded_layout_legacy_migration_and_clamps_remain_available_within_limit() {
        let directory = TempDirectory::new();
        let path = directory.path().join("layout.json");
        let legacy = serde_json::json!({
            "workspaces": [{"id": "legacy-invalid-id", "name": "Legacy café 🦀",
                "cwd": null, "panels": [], "viewport_pan": [2.0, 3.0],
                "viewport_zoom": 1.5, "next_z": 7, "next_color": 2}],
            "active_ws": 99,
            "sidebar_visible": true,
            "local_device_id": "legacy-device"
        });
        let bytes = serde_json::to_vec_pretty(&legacy).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let before = snapshot(directory.path());
        let loaded = match load_state_result_from_path_with_limit(&path, bytes.len()) {
            StateLoadResult::Loaded(state) => state,
            result => panic!("compatible bounded legacy layout must migrate: {result:?}"),
        };
        assert_eq!(loaded.schema_version, APP_STATE_SCHEMA_VERSION);
        assert_eq!(loaded.active_ws, 0);
        assert_eq!(loaded.workspaces[0].name, "Legacy café 🦀");
        assert_eq!(loaded.workspaces[0].legacy_canvas.viewport_zoom, 1.5);
        assert_eq!(loaded.workspaces[0].desktop.next_z, 7);
        assert!(loaded.legacy_canvas_ui.show_grid);
        assert!(!loaded.legacy_canvas_ui.show_minimap);
        assert!(uuid::Uuid::parse_str(&loaded.workspaces[0].id).is_ok_and(|id| !id.is_nil()));
        assert_eq!(snapshot(directory.path()), before);
    }

    #[test]
    fn bounded_layout_huge_advertised_length_is_rejected_without_any_read() {
        let mut reader = ProbeReader::new(b"{}");
        let error = read_state_bytes_with_limit(&mut reader, u64::MAX, 32).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("32"));
        assert_eq!(reader.calls, 0);
        assert_eq!(reader.offset, 0);
    }

    #[test]
    fn bounded_layout_growth_consumes_only_cap_plus_one_and_never_accepts_a_json_prefix() {
        let directory = TempDirectory::new();
        let path = directory.path().join("layout.json");
        let state = sample_state("complete JSON prefix");
        let bytes = pretty_bytes(&state);
        let limit = bytes.len();
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(backup_path(&path, 0), &bytes).unwrap();
        let before = snapshot(directory.path());
        let mut grown = bytes.clone();
        grown.extend_from_slice(&[b' '; 128]);
        let mut reader = ProbeReader::new(&grown);
        reader.max_read = 7;
        let mut requested = Vec::new();
        let result = load_state_result_with_reader(&path, |candidate| {
            requested.push(candidate.to_path_buf());
            assert_eq!(candidate, path);
            read_state_bytes_with_limit(&mut reader, limit as u64, limit).map(Some)
        });
        expect_read_error(result, &path);
        assert_eq!(requested, vec![path.clone()]);
        assert_eq!(reader.offset, limit + 1);
        assert!(reader.offset < grown.len());
        assert_eq!(snapshot(directory.path()), before);
    }

    #[test]
    fn bounded_layout_reader_retries_interrupted_calls_and_preserves_other_io_errors() {
        let bytes = b"synthetic JSON bytes";
        let mut interrupted = ProbeReader::new(bytes);
        interrupted.interruptions = 2;
        interrupted.max_read = 3;
        assert_eq!(
            read_state_bytes_with_limit(&mut interrupted, bytes.len() as u64, bytes.len()).unwrap(),
            bytes
        );
        assert_eq!(interrupted.interruptions, 0);
        let mut calls = 0;
        assert_eq!(
            retry_layout_interrupted(|| {
                calls += 1;
                if calls < 3 {
                    Err(io::ErrorKind::Interrupted.into())
                } else {
                    Ok(42)
                }
            })
            .unwrap(),
            42
        );
        assert_eq!(calls, 3);
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::NotFound,
            io::ErrorKind::Other,
        ] {
            for fail_at in [0, 5, bytes.len()] {
                let mut failed = ProbeReader::new(bytes);
                failed.fail_at = Some(fail_at);
                failed.failure_kind = kind;
                failed.max_read = 3;
                let error =
                    read_state_bytes_with_limit(&mut failed, bytes.len() as u64, bytes.len())
                        .unwrap_err();
                assert_eq!(error.kind(), kind);
                assert!(error.to_string().contains("synthetic layout read failure"));
            }
        }
    }

    #[test]
    fn bounded_layout_error_after_complete_json_does_not_become_missing_or_backup_fallback() {
        let directory = TempDirectory::new();
        let path = directory.path().join("layout.json");
        let state = sample_state("newer complete but unverified");
        let bytes = pretty_bytes(&state);
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(backup_path(&path, 0), pretty_bytes(&sample_state("older"))).unwrap();
        let before = snapshot(directory.path());
        for kind in [io::ErrorKind::NotFound, io::ErrorKind::PermissionDenied] {
            let mut failed = ProbeReader::new(&bytes);
            failed.fail_at = Some(bytes.len());
            failed.failure_kind = kind;
            let mut requested = Vec::new();
            let result = load_state_result_with_reader(&path, |candidate| {
                requested.push(candidate.to_path_buf());
                assert_eq!(candidate, path);
                read_state_bytes_with_limit(&mut failed, bytes.len() as u64, bytes.len()).map(Some)
            });
            let error = expect_read_error(result, &path);
            assert!(error.contains("synthetic layout read failure"));
            assert_eq!(failed.offset, bytes.len());
            assert_eq!(requested, vec![path.clone()]);
            assert_eq!(snapshot(directory.path()), before);
        }
    }

    #[test]
    fn bounded_layout_zero_and_overflowing_sentinel_limits_do_not_read_unbounded_data() {
        let mut empty = ProbeReader::new(b"");
        assert!(read_state_bytes_with_limit(&mut empty, 0, 0)
            .unwrap()
            .is_empty());
        let mut nonempty = ProbeReader::new(b"xx");
        assert_eq!(
            read_state_bytes_with_limit(&mut nonempty, 0, 0)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(nonempty.offset, 1);
        let mut invalid = ProbeReader::new(b"xx");
        assert_eq!(
            read_state_bytes_with_limit(&mut invalid, 0, usize::MAX)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(invalid.calls, 0);
    }

    #[test]
    fn bounded_layout_non_regular_primary_or_backup_is_an_unknown_earlier_snapshot() {
        for invalid_in_backup in [false, true] {
            let directory = TempDirectory::new();
            let path = directory.path().join("layout.json");
            let state = sample_state("older readable");
            let bytes = pretty_bytes(&state);
            let invalid = if invalid_in_backup {
                std::fs::write(&path, b"{invalid primary").unwrap();
                std::fs::write(backup_path(&path, 1), &bytes).unwrap();
                backup_path(&path, 0)
            } else {
                std::fs::write(backup_path(&path, 0), &bytes).unwrap();
                path.clone()
            };
            std::fs::create_dir(&invalid).unwrap();
            std::fs::write(invalid.join("keep"), b"metadata").unwrap();
            let before = snapshot(directory.path());
            expect_read_error(
                load_state_result_from_path_with_limit(&path, bytes.len()),
                &invalid,
            );
            assert_eq!(snapshot(directory.path()), before);
        }
    }

    #[test]
    fn bounded_layout_rejections_preserve_readonly_primary_and_backup_metadata() {
        let directory = TempDirectory::new();
        let path = directory.path().join("layout.json");
        let backup = backup_path(&path, 0);
        let older = sample_state("readonly backup");
        let older_bytes = pretty_bytes(&older);
        let mut larger = sample_state("readonly primary");
        larger.orchestration.tasks[0].brief = "larger metadata".repeat(100);
        let larger_bytes = pretty_bytes(&larger);
        std::fs::write(&path, &larger_bytes).unwrap();
        std::fs::write(&backup, &older_bytes).unwrap();
        let _main_readonly = ReadonlyGuard::new(&path);
        let _backup_readonly = ReadonlyGuard::new(&backup);
        let before = snapshot(directory.path());
        expect_read_error(
            load_state_result_from_path_with_limit(&path, older_bytes.len()),
            &path,
        );
        assert!(save_state_to_path_with_limit(&path, &larger, larger_bytes.len() - 1).is_err());
        assert_eq!(snapshot(directory.path()), before);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_layout_regular_links_load_but_present_dangling_links_block_fallback() {
        use std::os::unix::fs::symlink;
        for link_in_backup in [false, true] {
            for dangling in [false, true] {
                let directory = TempDirectory::new();
                let path = directory.path().join("layout.json");
                let target = directory.path().join("target.json");
                let state = sample_state("regular linked source");
                let bytes = pretty_bytes(&state);
                if !dangling {
                    std::fs::write(&target, &bytes).unwrap();
                }
                let link = if link_in_backup {
                    std::fs::write(&path, b"{invalid primary").unwrap();
                    std::fs::write(backup_path(&path, 1), &bytes).unwrap();
                    backup_path(&path, 0)
                } else {
                    std::fs::write(backup_path(&path, 0), &bytes).unwrap();
                    path.clone()
                };
                symlink(&target, &link).unwrap();
                let before = snapshot(directory.path());
                let result = load_state_result_from_path_with_limit(&path, bytes.len());
                if dangling {
                    expect_read_error(result, &link);
                } else {
                    assert_eq!(result, StateLoadResult::Loaded(state));
                }
                assert_eq!(snapshot(directory.path()), before);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn bounded_layout_fifo_primary_and_each_backup_fail_without_a_peer_or_unbounded_join() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::sync::mpsc;
        use std::time::Duration;

        for index in 0..=BACKUP_SLOTS {
            let directory = TempDirectory::new();
            let path = directory.path().join("layout.json");
            let state = sample_state("later readable snapshot");
            let bytes = pretty_bytes(&state);
            let candidates: Vec<_> = std::iter::once(path.clone())
                .chain((0..BACKUP_SLOTS).map(|slot| backup_path(&path, slot)))
                .collect();
            for earlier in &candidates[..index] {
                std::fs::write(earlier, b"{invalid earlier snapshot").unwrap();
            }
            if let Some(later) = candidates.get(index + 1) {
                std::fs::write(later, &bytes).unwrap();
            }
            let fifo = &candidates[index];
            let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
            // SAFETY: fifo_c owns a NUL-terminated path valid for this call.
            assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
            let before = snapshot(directory.path());
            let worker_path = path.clone();
            let limit = bytes.len();
            let (sender, receiver) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let result = load_state_result_from_path_with_limit(&worker_path, limit);
                let _ = sender.send(result);
            });
            // A timeout detaches this handle during unwinding. No scoped join
            // or FIFO peer can mask a regression that waits in open/read.
            let result = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            worker.join().unwrap();
            expect_read_error(result, fifo);
            assert_eq!(snapshot(directory.path()), before);
        }
    }
}
