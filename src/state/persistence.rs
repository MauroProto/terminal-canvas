use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::collab::TrustedDevice;
use crate::orchestration::OrchestrationState;
use crate::state::panel_state::PanelState;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppState {
    pub workspaces: Vec<WorkspaceState>,
    pub active_ws: usize,
    pub sidebar_visible: bool,
    pub show_grid: bool,
    pub show_minimap: bool,
    #[serde(default = "default_local_device_id")]
    pub local_device_id: String,
    #[serde(default)]
    pub trusted_devices: Vec<TrustedDevice>,
    #[serde(default)]
    pub orchestration: OrchestrationState,
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
    pub viewport_pan: [f32; 2],
    pub viewport_zoom: f32,
    pub next_z: u32,
    pub next_color: usize,
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
    let path = state_file_path()?;
    load_state_from_path(&path)
}

pub fn load_state_from_path(path: &Path) -> Option<AppState> {
    read_state_file(path).or_else(|| read_state_file(&backup_file_path(path)))
}

pub fn save_state(state: &AppState) {
    if let Err(err) = try_save_state(state) {
        log::warn!("Failed to write state file: {err}");
    }
}

pub fn try_save_state(state: &AppState) -> anyhow::Result<()> {
    let Some(path) = state_file_path() else {
        anyhow::bail!("Could not determine state file path");
    };
    save_state_to_path(&path, state)
}

pub fn save_state_to_path(path: &Path, state: &AppState) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp_path = temp_file_path(path);
    let backup_path = backup_file_path(path);

    write_json_file(&tmp_path, state)?;

    if path.exists() {
        if backup_path.exists() {
            let _ = std::fs::remove_file(&backup_path);
        }
        std::fs::rename(path, &backup_path)?;
    }

    if let Err(err) = std::fs::rename(&tmp_path, path) {
        if backup_path.exists() && !path.exists() {
            let _ = std::fs::rename(&backup_path, path);
        }
        return Err(err.into());
    }

    Ok(())
}

fn read_state_file(path: &Path) -> Option<AppState> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_json_file(path: &Path, state: &AppState) -> anyhow::Result<()> {
    use std::io::Write as _;

    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, state)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    let file = writer.into_inner().map_err(|err| err.into_error())?;
    file.sync_all()?;
    Ok(())
}

fn backup_file_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

fn temp_file_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use chrono::Utc;
    use uuid::Uuid;

    use super::{
        backup_file_path, load_state_from_path, save_state_to_path, temp_file_path, AppState,
        AutosaveController, AutosaveDecision, WorkspaceState,
    };
    use crate::collab::TrustedDevice;
    use crate::orchestration::OrchestrationState;
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

        assert!(!temp_file_path(&path).exists());
    }

    fn sample_state(label: &str) -> AppState {
        AppState {
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
                }],
                viewport_pan: [0.0, 0.0],
                viewport_zoom: 1.0,
                next_z: 2,
                next_color: 1,
            }],
            active_ws: 0,
            sidebar_visible: true,
            show_grid: true,
            show_minimap: false,
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
    fn backup_path_uses_expected_suffix() {
        let path = PathBuf::from("/tmp/layout.json");
        assert_eq!(
            backup_file_path(&path),
            PathBuf::from("/tmp/layout.json.bak")
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

        assert_eq!(loaded.workspaces[0].panels.len(), 1);
        assert_eq!(loaded.workspaces[0].panels[0].title, "Terminal");
        assert!(!loaded.local_device_id.is_empty());
        assert!(loaded.trusted_devices.is_empty());
    }

    #[test]
    fn saved_state_omits_runtime_session_metadata() {
        let dir = unique_temp_dir();
        let path = dir.join("layout.json");
        let state = sample_state("ui-only");

        save_state_to_path(&path, &state).unwrap();

        let serialized = fs::read_to_string(&path).unwrap();
        assert!(!serialized.contains("runtime_session_id"));
        assert!(!serialized.contains("runtime_sessions"));
    }
}
