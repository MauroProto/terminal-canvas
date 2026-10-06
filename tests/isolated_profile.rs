use std::path::PathBuf;
use std::process::{Command, Output};

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

    fn command(&self, executable: &str) -> Command {
        let mut command = Command::new(executable);
        command
            .current_dir(&self.0)
            .env("TERMINAL_CANVAS_HOME", &self.0)
            .env_remove("TC_MEMORY_DB")
            .env_remove("TC_MEMORY_TASK_ID")
            .env_remove("MI_TERMINAL_SCROLLBACK_DIR")
            .env_remove("MI_TERMINAL_DAEMON_DIR");
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

#[test]
fn helper_memory_survives_process_restart_inside_an_isolated_profile() {
    let profile = Profile::new();
    let binary = env!("CARGO_BIN_EXE_tc-memory");
    let health = json(profile.command(binary).arg("health").output().unwrap());
    assert_eq!(
        PathBuf::from(health["db"].as_str().unwrap()),
        profile.0.join("data/memory/memory.db")
    );
    json(
        profile
            .command(binary)
            .args([
                "remember",
                "--cwd",
                profile.0.to_str().unwrap(),
                "--key",
                "restart-proof",
                "--content",
                "Persistent isolated memory",
                "--scope",
                "project",
            ])
            .output()
            .unwrap(),
    );
    let restored = profile
        .command(binary)
        .args(["context", "--cwd", profile.0.to_str().unwrap(), "--text"])
        .output()
        .unwrap();
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
        let output = profile
            .command(binary)
            .env("TERMINAL_CANVAS_HOME", "relative-profile")
            .arg(flag)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("TERMINAL_CANVAS_HOME"));
    }
    assert_eq!(std::fs::read_dir(&profile.0).unwrap().count(), 0);
}

#[test]
fn package_health_check_is_read_only_and_reports_every_state_path() {
    let profile = Profile::new();
    let report = json(
        profile
            .command(env!("CARGO_BIN_EXE_mi-terminal"))
            .arg("--health-check")
            .output()
            .unwrap(),
    );
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
        let output = profile
            .command(std::env::current_exe().unwrap().to_str().unwrap())
            .args(["--exact", "profile_persistence_subprocess", "--nocapture"])
            .env("TC_PROFILE_TEST_PHASE", phase)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{phase}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!profile.0.join("data/run.marker").exists());
    }
    assert!(profile.0.join("config/config.toml").is_file());
    assert!(profile.0.join("data/layout.json").is_file());
    assert!(!profile.0.join("layout.json").exists());
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
    assert!(state::run_marker::begin_run().is_none());
    assert!(state::run_marker::persistence_claim_error().is_none());
    let panel = uuid::Uuid::from_u128(1);
    let leaves = [uuid::Uuid::from_u128(2), uuid::Uuid::from_u128(3)];
    let history_dir = state::scrollback_store::scrollback_dir().unwrap();
    assert_eq!(history_dir, root.join("data/scrollback"));
    match phase.as_str() {
        "save" => {
            let settings = config::AppConfig {
                font_size: 16.5,
                onboarding_dismissed: true,
                ..Default::default()
            };
            config::save(&settings).unwrap();
            let layout: state::AppState = serde_json::from_value(serde_json::json!({
                "workspaces": [{ "id": uuid::Uuid::from_u128(4).to_string(),
                    "name": "Proyecto ñ", "cwd": root, "panels": [] }],
                "active_ws": 0, "sidebar_visible": false
            }))
            .unwrap();
            state::persistence::try_save_state(&layout).unwrap();
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
        "restore" => {
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
    state::run_marker::end_run_clean();
}
