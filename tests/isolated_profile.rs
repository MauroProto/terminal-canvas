use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

fn output_bounded(command: &mut Command) -> Output {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Profile command did not exit within ten seconds");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

struct Profile(PathBuf);

impl Profile {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("tc-isolated-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn command(&self, executable: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(executable);
        command
            .current_dir(&self.0)
            .env("TERMINAL_CANVAS_HOME", &self.0)
            .env_remove("TC_MEMORY_DB")
            .env_remove("TC_MEMORY_TASK_ID")
            .env_remove("MI_TERMINAL_SCROLLBACK_DIR")
            .env_remove("MI_TERMINAL_DAEMON_DIR")
            .env_remove("TC_PROFILE_TEST_PHASE")
            .env_remove("TC_PROFILE_EXPECTED_DIRTY_MARKER");
        command
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn json(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn run_profile_phase(profile: &Profile, phase: &str, previous_marker: Option<&str>) {
    let mut command = profile.command(std::env::current_exe().unwrap());
    command
        .args(["--exact", "profile_persistence_subprocess", "--nocapture"])
        .env("TC_PROFILE_TEST_PHASE", phase);
    if let Some(marker) = previous_marker {
        command.env("TC_PROFILE_EXPECTED_DIRTY_MARKER", marker);
    }
    let output = output_bounded(&mut command);
    assert!(
        output.status.success(),
        "{phase}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[derive(Debug, PartialEq, Eq)]
struct ProfileEntry {
    directory: bool,
    length: u64,
    modified: SystemTime,
    readonly: bool,
    #[cfg(unix)]
    mode: u32,
    #[cfg(windows)]
    attributes: u32,
    contents: Option<Vec<u8>>,
}

// Include directory mtimes and permissions: rewriting identical bytes, creating
// a run marker, or repairing configuration must still fail a read-only check.
fn profile_snapshot(root: &Path) -> BTreeMap<PathBuf, ProfileEntry> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, ProfileEntry>) {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        assert!(!metadata.file_type().is_symlink());
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt;
        entries.insert(
            path.strip_prefix(root).unwrap().to_path_buf(),
            ProfileEntry {
                directory: metadata.is_dir(),
                length: metadata.len(),
                modified: metadata.modified().unwrap(),
                readonly: metadata.permissions().readonly(),
                #[cfg(unix)]
                mode: metadata.mode(),
                #[cfg(windows)]
                attributes: metadata.file_attributes(),
                contents: metadata.is_file().then(|| std::fs::read(path).unwrap()),
            },
        );
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn saved_layout(root: &Path) -> mi_terminal::state::AppState {
    serde_json::from_value(serde_json::json!({
        "workspaces": [{ "id": uuid::Uuid::from_u128(4).to_string(),
            "name": "Proyecto ñ", "cwd": root, "panels": [] }],
        "active_ws": 0, "sidebar_visible": false
    }))
    .unwrap()
}

#[test]
fn helper_memory_survives_process_restart_inside_an_isolated_profile() {
    let profile = Profile::new();
    let binary = env!("CARGO_BIN_EXE_tc-memory");
    let health = json(output_bounded(profile.command(binary).arg("health")));
    assert_eq!(
        PathBuf::from(health["db"].as_str().unwrap()),
        profile.0.join("data/memory/memory.db")
    );
    json(output_bounded(profile.command(binary).args([
        "remember",
        "--cwd",
        profile.0.to_str().unwrap(),
        "--key",
        "restart-proof",
        "--content",
        "Persistent isolated memory",
        "--scope",
        "project",
    ])));
    let restored = output_bounded(profile.command(binary).args([
        "context",
        "--cwd",
        profile.0.to_str().unwrap(),
        "--text",
    ]));
    assert!(restored.status.success());
    assert!(String::from_utf8_lossy(&restored.stdout).contains("Persistent isolated memory"));
    assert!(
        !profile.0.join("memory.db").exists(),
        "No cwd fallback database"
    );
}

#[test]
fn invalid_profile_fails_before_starting_ui_or_creating_fallback_data() {
    let profile = Profile::new();
    for binary in [
        env!("CARGO_BIN_EXE_tc-memory"),
        env!("CARGO_BIN_EXE_mi-terminal"),
    ] {
        let flag = if binary == env!("CARGO_BIN_EXE_tc-memory") {
            "health"
        } else {
            "--health-check"
        };
        let output = output_bounded(
            profile
                .command(binary)
                .env("TERMINAL_CANVAS_HOME", "relative-profile")
                .arg(flag),
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("TERMINAL_CANVAS_HOME"));
    }
    assert_eq!(std::fs::read_dir(&profile.0).unwrap().count(), 0);
}

#[test]
fn package_health_check_is_read_only_and_reports_every_state_path() {
    let profile = Profile::new();
    let report = json(output_bounded(
        profile
            .command(env!("CARGO_BIN_EXE_mi-terminal"))
            .arg("--health-check"),
    ));
    assert_eq!(report["profile"]["isolated"], true);
    assert_eq!(report["profile"]["global_agent_configuration"], false);
    for field in ["config", "data", "cache", "panic_log"] {
        assert!(PathBuf::from(report["profile"][field].as_str().unwrap()).starts_with(&profile.0));
    }
    assert_eq!(std::fs::read_dir(&profile.0).unwrap().count(), 0);
}

#[test]
fn extra_cli_arguments_are_rejected_without_starting_the_app() {
    let profile = Profile::new();
    for args in [["--health-check", "--json"], ["ignored", "--version"]] {
        let output = output_bounded(
            profile
                .command(env!("CARGO_BIN_EXE_mi-terminal"))
                .args(args),
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("without additional arguments"));
    }
    assert_eq!(std::fs::read_dir(&profile.0).unwrap().count(), 0);
}

#[test]
fn layout_settings_split_history_and_notes_survive_a_clean_process_restart() {
    let profile = Profile::new();
    for phase in ["save", "restore"] {
        run_profile_phase(&profile, phase, None);
        assert!(!profile.0.join("data/run.marker").exists());
    }
    assert!(profile.0.join("config/config.toml").is_file());
    assert!(profile.0.join("data/layout.json").is_file());
    assert!(!profile.0.join("layout.json").exists());
}

#[test]
fn a_process_exit_without_cleanup_reports_dirty_run_and_preserves_saved_state() {
    let profile = Profile::new();
    run_profile_phase(&profile, "save-interrupted", None);
    let marker = std::fs::read_to_string(profile.0.join("data/run.marker")).unwrap();
    assert!(!marker.trim().is_empty());
    let settings: toml::Value =
        toml::from_str(&std::fs::read_to_string(profile.0.join("config/config.toml")).unwrap())
            .unwrap();
    assert_eq!(settings["terminal"]["font_size"].as_float(), Some(16.5));
    let layout: serde_json::Value =
        serde_json::from_slice(&std::fs::read(profile.0.join("data/layout.json")).unwrap())
            .unwrap();
    assert_eq!(layout["workspaces"][0]["name"], "Proyecto ñ");

    run_profile_phase(&profile, "restore-interrupted", Some(marker.trim()));
    assert!(!profile.0.join("data/run.marker").exists());
    let diagnostic = std::fs::read_to_string(profile.0.join("data/runs.log")).unwrap();
    assert!(diagnostic.contains("murió sin cierre limpio"));
    assert!(diagnostic.contains(marker.trim()));

    // A fresh process after recovery must see a clean previous exit.
    run_profile_phase(&profile, "restore", None);
    assert!(!profile.0.join("data/run.marker").exists());
}

#[test]
fn read_only_cli_preserves_an_existing_profile_including_a_stale_run_marker() {
    let profile = Profile::new();
    run_profile_phase(&profile, "save-interrupted", None);
    std::fs::create_dir_all(profile.0.join("cache/updates")).unwrap();
    std::fs::write(profile.0.join("cache/updates/package.part"), b"unfinished").unwrap();
    let before = profile_snapshot(&profile.0);
    assert!(before.contains_key(Path::new("config/config.toml")));
    assert!(before.contains_key(Path::new("data/layout.json")));
    assert!(before.contains_key(Path::new("data/run.marker")));
    assert!(before.contains_key(Path::new("data/scrollback")));
    assert!(
        before.len() > 10,
        "The fixture must contain real persisted state"
    );

    let binary = env!("CARGO_BIN_EXE_mi-terminal");
    for flag in ["--version", "--help", "--health-check"] {
        let output = output_bounded(profile.command(binary).arg(flag));
        assert!(
            output.status.success(),
            "{flag}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if flag == "--health-check" {
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["profile"]["isolated"], true);
            assert_eq!(
                PathBuf::from(report["profile"]["data"].as_str().unwrap()),
                profile.0.join("data")
            );
        } else {
            assert!(String::from_utf8_lossy(&output.stdout).contains("TerminalCanvas"));
        }
        assert_eq!(
            profile_snapshot(&profile.0),
            before,
            "{flag} modified state"
        );
    }
    for args in [
        ["--version", "unexpected"],
        ["--help", "unexpected"],
        ["--health-check", "unexpected"],
        ["unexpected", "--version"],
    ] {
        let output = output_bounded(profile.command(binary).args(args));
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("without additional arguments"));
        assert_eq!(
            profile_snapshot(&profile.0),
            before,
            "{args:?} modified state"
        );
    }
}

#[test]
fn a_file_blocking_a_profile_directory_fails_without_fallback_or_data_loss() {
    for blocked in ["config", "data"] {
        let profile = Profile::new();
        let blocker = profile.0.join(blocked);
        std::fs::write(&blocker, b"Preserve this existing file, never replace it").unwrap();
        if blocked == "config" {
            std::fs::create_dir_all(profile.0.join("data")).unwrap();
            std::fs::write(
                profile.0.join("data/layout.json"),
                serde_json::to_vec(&saved_layout(&profile.0)).unwrap(),
            )
            .unwrap();
        } else {
            std::fs::create_dir_all(profile.0.join("config")).unwrap();
            std::fs::write(
                profile.0.join("config/config.toml"),
                b"[terminal]\nfont_size = 16.5\n",
            )
            .unwrap();
        }
        let before = profile_snapshot(&profile.0);
        run_profile_phase(&profile, &format!("blocked-{blocked}"), None);
        let after = profile_snapshot(&profile.0);
        assert_eq!(after[Path::new(blocked)], before[Path::new(blocked)]);
        if blocked == "data" {
            assert_eq!(
                after, before,
                "A failed ownership claim must not write state"
            );
        } else {
            assert_eq!(
                after[Path::new("data/layout.json")],
                before[Path::new("data/layout.json")]
            );
            assert!(!profile.0.join("data/run.marker").exists());
            assert!(profile.0.join("data/run.lock").is_file());
        }
        assert!(!profile.0.join("config.toml").exists());
        assert!(!profile.0.join("layout.json").exists());
        assert!(!profile.0.join("memory.db").exists());
    }
}

// This test is also the child entry point. Fresh processes exercise the real
// process-wide path resolver and run ownership without changing the test
// runner's environment or accessing an existing application profile.
#[test]
fn profile_persistence_subprocess() {
    let Ok(phase) = std::env::var("TC_PROFILE_TEST_PHASE") else {
        return;
    };
    use mi_terminal::{config, orchestration, state};
    let root = PathBuf::from(std::env::var_os("TERMINAL_CANVAS_HOME").unwrap());
    let paths = mi_terminal::utils::app_paths::get().unwrap();
    assert_eq!(paths.data, root.join("data"));
    assert_eq!(paths.config, root.join("config"));
    assert_eq!(
        config::config_file_path(),
        Some(root.join("config/config.toml"))
    );
    assert_eq!(
        state::persistence::state_file_path(),
        Some(root.join("data/layout.json"))
    );
    let dirty = state::run_marker::begin_run();
    if phase == "blocked-data" {
        assert!(dirty.is_none());
        assert!(state::run_marker::persistence_claim_error().is_some());
        assert!(!state::run_marker::current_process_may_write());
        assert!(state::run_marker::acquire_write_guard().is_err());
        assert!(state::persistence::try_save_state(&saved_layout(&root)).is_err());
        assert!(state::load_state().is_none());
        state::run_marker::end_run_clean();
        return;
    }
    if phase == "restore-interrupted" {
        let expected = std::env::var("TC_PROFILE_EXPECTED_DIRTY_MARKER").unwrap();
        assert_eq!(
            dirty
                .expect("An exit without cleanup must be reported")
                .marker,
            expected
        );
    } else {
        assert!(dirty.is_none());
    }
    assert!(state::run_marker::persistence_claim_error().is_none());
    assert!(state::run_marker::current_process_may_write());
    if phase == "blocked-config" {
        assert!(config::save(&config::AppConfig::default()).is_err());
        let layout = state::load_state().expect("Unrelated existing layout must remain readable");
        assert_eq!(layout.workspaces[0].name, "Proyecto ñ");
        assert_eq!(layout.workspaces[0].cwd.as_deref(), Some(root.as_path()));
        state::run_marker::end_run_clean();
        return;
    }
    let panel = uuid::Uuid::from_u128(1);
    let leaves = [uuid::Uuid::from_u128(2), uuid::Uuid::from_u128(3)];
    let history_dir = state::scrollback_store::scrollback_dir().unwrap();
    assert_eq!(history_dir, root.join("data/scrollback"));
    match phase.as_str() {
        "save" | "save-interrupted" => {
            let settings = config::AppConfig {
                font_size: 16.5,
                onboarding_dismissed: true,
                ..Default::default()
            };
            config::save(&settings).unwrap();
            state::persistence::try_save_state(&saved_layout(&root)).unwrap();
            for (index, leaf) in leaves.into_iter().enumerate() {
                state::scrollback_store::save_leaf_scrollback_versioned(
                    &history_dir,
                    panel,
                    Some(leaf),
                    7,
                    &format!("hoja {index}: salida ñ\n"),
                )
                .unwrap();
            }
            let mut notes = orchestration::DiffNotes::default();
            notes.add("archivo ñ.rs", None, 3, "Conservar esta decisión");
            orchestration::save_notes(&root, &notes).unwrap();
        }
        "restore" | "restore-interrupted" => {
            let settings = config::load();
            assert_eq!(settings.font_size, 16.5);
            assert!(settings.onboarding_dismissed);
            let layout = state::load_state().unwrap();
            assert_eq!(layout.workspaces[0].name, "Proyecto ñ");
            assert_eq!(layout.workspaces[0].cwd.as_deref(), Some(root.as_path()));
            assert!(!layout.sidebar_visible);
            for (index, leaf) in leaves.into_iter().enumerate() {
                let (generation, text) = state::scrollback_store::load_leaf_scrollback_checkpoint(
                    &history_dir,
                    panel,
                    Some(leaf),
                )
                .unwrap();
                assert_eq!(generation, Some(7));
                assert_eq!(text, format!("hoja {index}: salida ñ\n"));
            }
            let notes = orchestration::load_notes(&root);
            assert_eq!(notes.notes.len(), 1);
            assert_eq!(notes.notes[0].body, "Conservar esta decisión");
        }
        other => panic!("Unexpected child phase: {other}"),
    }
    assert!(!mi_terminal::utils::app_paths::permits_global_agent_configuration());
    orchestration::install_claude_hooks().unwrap();
    orchestration::uninstall_claude_hooks().unwrap();
    if phase == "save-interrupted" {
        // All writes above completed successfully. Exit without destructors or
        // end_run_clean to exercise recovery from interrupted process teardown.
        // This is not a power-loss or native-crash simulation.
        std::process::exit(0);
    }
    state::run_marker::end_run_clean();
}
