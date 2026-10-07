use std::fs;

use egui::{pos2, vec2, CentralPanel, Color32, Modifiers, RawInput, Rect};
use uuid::Uuid;

use super::taskbar::{
    clamp_rect_to_desktop, desktop_required_snap_slot_for_pointer, desktop_snap_rect_for_pointer,
    desktop_snap_slot_for_pointer, taskbar_provider_accent, taskbar_provider_label,
};
use super::windowing::slot_drag_started;
use super::{
    close_workspace_for_good, desktop_canvas_rect, interpolate_viewport,
    overview_viewport_for_panels, panel_scroll_capture_active,
    should_stop_collaboration_on_workspace_close, split_resize_hit, top_panel_hit,
    top_panel_scroll_hit, upsert_workspace_for_folder, SplitResizeAxis, WindowTransition,
    WindowTransitionKind,
};
use crate::canvas::config::{MINIMAP_BG, MINIMAP_HEIGHT, MINIMAP_PADDING, MINIMAP_WIDTH};
use crate::canvas::minimap;
use crate::canvas::viewport::Viewport;
use crate::collab::CollabMode;
use crate::orchestration::AgentProvider;
use crate::panel::CanvasPanel;
use crate::state::{SnapSlot, Workspace};
use crate::terminal::panel::{PanelHitArea, TerminalPanel, PANEL_BG};

#[test]
fn test_app_never_owns_the_users_run_marker() {
    let ctx = egui::Context::default();
    let app = super::TerminalApp::new_for_tests(&ctx);

    assert!(!app.run_marker_active);
    assert!(!app.persistence_writes_enabled);
}

/// Run real closing writes in a fresh process: cached paths and the writer
/// claim must never be changed in the test runner's own user profile.
fn run_final_save_fixture(phase: &str) {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let profile = unique_temp_dir("tc-final-save-profile");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "app::tests::final_save_profile_fixture",
            "--ignored",
            "--nocapture",
        ])
        .current_dir(&profile)
        .env("TERMINAL_CANVAS_HOME", &profile)
        .env("TC_FINAL_SAVE_FIXTURE", phase)
        .env_remove("MI_TERMINAL_SCROLLBACK_DIR")
        .env_remove("MI_TERMINAL_DAEMON_DIR")
        .env_remove("TC_MEMORY_DB")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_dir_all(&profile);
            panic!("Final save fixture timed out: {phase}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    let _ = fs::remove_dir_all(&profile);
    assert!(
        output.status.success(),
        "{phase}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn final_save_drains_stale_autosave_before_publishing_and_removing_marker() {
    run_final_save_fixture("ordered-close");
}

#[test]
fn final_save_confirmed_workspace_close_cannot_be_replaced_by_stale_autosave() {
    run_final_save_fixture("workspace-close");
}

#[test]
fn final_save_retains_recovery_marker_when_layout_write_fails() {
    run_final_save_fixture("layout-failure");
}

#[test]
fn final_save_preserves_history_referenced_by_old_layout_when_publication_fails() {
    run_final_save_fixture("layout-failure-keeps-old-history");
}

#[test]
fn final_save_incremental_pruning_waits_for_current_layout_acknowledgement() {
    run_final_save_fixture("incremental-pruning-after-layout-ack");
}

#[test]
fn final_save_retains_recovery_marker_when_history_write_fails() {
    run_final_save_fixture("history-failure");
}

#[test]
fn final_save_rescues_layout_and_history_after_accepted_settings_write_fails() {
    run_final_save_fixture("settings-failure");
}

#[test]
fn final_save_does_not_write_or_remove_successor_marker_after_losing_lease() {
    run_final_save_fixture("lease-lost");
}

#[test]
fn final_save_crash_recovery_preserves_the_dirty_marker() {
    run_final_save_fixture("crash-recovery");
}

#[test]
fn workspace_close_applies_durable_ack_before_capturing_a_new_batch() {
    run_final_save_fixture("ack-workspace-close");
}

#[test]
fn replaced_session_rejects_an_incremental_ack_from_its_previous_pty() {
    run_final_save_fixture("ack-replaced-session-incremental");
}

#[test]
fn replaced_session_rejects_a_full_checkpoint_ack_from_its_previous_pty() {
    run_final_save_fixture("ack-replaced-session-full");
}

#[test]
fn workspace_close_preserves_rollover_consumed_by_the_idle_barrier() {
    run_final_save_fixture("ack-rollover-workspace-close");
}

#[test]
fn shutdown_during_history_replay_rescues_pending_output_and_retains_recovery() {
    run_final_save_fixture("ack-restoring-shutdown");
}

#[test]
fn completed_history_replay_can_publish_a_full_checkpoint_and_clean_marker() {
    run_final_save_fixture("ack-restoring-completed");
}

#[test]
fn full_save_waits_for_initial_capture_before_any_new_output_is_written() {
    run_final_save_fixture("ack-capture-pending");
}

#[test]
fn ready_unapplied_history_cannot_be_replaced_by_a_fresh_live_grid() {
    run_final_save_fixture("ack-capture-ready");
}

#[test]
fn legacy_root_rescue_preserves_the_alias_until_a_complete_canonical_checkpoint() {
    run_final_save_fixture("ack-capture-legacy");
}

#[test]
#[ignore = "only run in an isolated child process through run_final_save_fixture"]
fn final_save_profile_fixture() {
    use eframe::App;
    use std::time::{Duration, Instant};

    fn hold_stale_autosave(
        app: &mut super::TerminalApp,
        snapshot: crate::state::AppState,
        observed_layout: std::path::PathBuf,
    ) -> std::thread::JoinHandle<()> {
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        app.persistence_worker =
            super::persistence_worker::PersistenceWorker::with_state_processor_for_tests(
                move |snapshot| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                    crate::state::persistence::try_save_state(snapshot)
                },
            );
        assert!(app.persistence_worker.submit_state(snapshot));
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        std::thread::spawn(move || {
            // A premature final save publishes the new file while the old
            // autosave is still blocked, then the stale worker replaces it.
            // A correct barrier keeps the file absent until we release it.
            let deadline = Instant::now() + Duration::from_millis(500);
            while !observed_layout.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            release_tx.send(()).unwrap();
        })
    }

    let phase = std::env::var("TC_FINAL_SAVE_FIXTURE").expect("isolated fixture phase");
    let profile = std::path::PathBuf::from(
        std::env::var_os("TERMINAL_CANVAS_HOME").expect("isolated fixture profile"),
    );
    assert!(profile.is_absolute());
    let data = crate::utils::app_paths::data_dir().unwrap();
    let layout = crate::state::persistence::state_file_path().unwrap();
    let history = crate::state::scrollback_store::scrollback_dir().unwrap();
    let marker = data.join("run.marker");
    assert!(data.starts_with(&profile));
    assert!(layout.starts_with(&profile));
    assert!(history.starts_with(&profile));
    crate::state::run_marker::begin_run();
    assert!(crate::state::run_marker::current_process_may_write());
    assert!(marker.exists());
    let ctx = egui::Context::default();
    let mut app = if phase.starts_with("ack-") {
        // A saved layout creates detached sessions. Install an in-memory
        // spawner before attaching them so these regressions start no shell.
        let mut workspace = Workspace::new("ACK regression", None);
        workspace.add_restored_terminal(TerminalPanel::new(
            pos2(0.0, 0.0),
            vec2(300.0, 200.0),
            Color32::WHITE,
            1,
        ));
        let state = crate::state::AppState {
            schema_version: crate::state::persistence::APP_STATE_SCHEMA_VERSION,
            workspaces: vec![workspace.to_saved()],
            active_ws: 0,
            sidebar_visible: true,
            legacy_canvas_ui: Default::default(),
            local_device_id: Uuid::new_v4().to_string(),
            trusted_devices: Vec::new(),
            orchestration: Default::default(),
        };
        let mut app = super::TerminalApp::build(&ctx, None, Some(state), None, false, false);
        // This artificial initial layout has no history yet. Later phases
        // explicitly install their old artifacts and controlled capture job.
        let _ = app.drain_persistence_worker();
        let manager = app.workspaces[0].pty_manager();
        manager
            .lock()
            .unwrap()
            .set_remote_spawner(Box::new(|_, _, _, _, _| {
                Ok((
                    Uuid::new_v4(),
                    crate::terminal::pty::PtyHandle::for_persistence_tests(b""),
                ))
            }));
        let CanvasPanel::Terminal(panel) = &mut app.workspaces[0].panels[0];
        panel.attach_session_with_spec(manager, None, crate::runtime::SessionSpec::default());
        app
    } else {
        super::TerminalApp::new_for_tests(&ctx)
    };
    app.persistence_writes_enabled = true;
    app.run_marker_active = true;
    app.workspaces[0].name = "Final workspace".to_owned();

    match phase.as_str() {
        "ordered-close" => {
            let mut stale = app.snapshot_state();
            stale.workspaces[0].name = "Stale autosave workspace".to_owned();
            let release = hold_stale_autosave(&mut app, stale, layout.clone());
            app.on_exit(None);
            release.join().unwrap();
            let saved: crate::state::AppState =
                serde_json::from_slice(&fs::read(&layout).unwrap()).unwrap();
            assert_eq!(saved.workspaces[0].name, "Final workspace");
            assert!(!marker.exists(), "a successful close removes its marker");
        }
        "workspace-close" => {
            let removed = Workspace::new("Confirmed closed workspace", None);
            let removed_id = removed.id;
            app.workspaces.push(removed);
            let stale = app.snapshot_state();
            assert_eq!(stale.workspaces.len(), 2);
            let release = hold_stale_autosave(&mut app, stale, layout.clone());
            app.close_workspace_confirmed(removed_id);
            release.join().unwrap();
            let _ = app.drain_persistence_worker();
            let saved: crate::state::AppState =
                serde_json::from_slice(&fs::read(&layout).unwrap()).unwrap();
            assert_eq!(saved.workspaces.len(), 1);
            assert!(saved
                .workspaces
                .iter()
                .all(|workspace| workspace.id != removed_id.to_string()));
            assert_eq!(saved.workspaces[0].name, "Final workspace");
            assert!(
                marker.exists(),
                "closing a workspace does not end the app run"
            );
        }
        "layout-failure" => {
            fs::create_dir(&layout).unwrap();
            app.on_exit(None);
            assert!(layout.is_dir());
            assert!(
                history.is_dir(),
                "history rescue still runs after layout failure"
            );
            assert!(marker.exists());
        }
        "layout-failure-keeps-old-history" => {
            let panel_id = app.workspaces[0].panels[0].id();
            let leaf_id = app.workspaces[0].panels[0].root_leaf_id();
            let old_layout = app.snapshot_state();
            let backup = crate::state::durable_write::backup_path(&layout, 0);
            crate::state::persistence::save_state_to_path(&backup, &old_layout).unwrap();
            let old_backup_bytes = fs::read(&backup).unwrap();
            crate::state::scrollback_store::save_leaf_scrollback_versioned(
                &history,
                panel_id,
                Some(leaf_id),
                1,
                "Old recoverable terminal output\n",
            )
            .unwrap();
            app.remember_scrollback_layout();
            app.workspaces[0].panels.clear();
            // Nonempty directory guarantees the new file cannot be renamed
            // onto the layout path on any supported operating system.
            fs::create_dir(&layout).unwrap();
            fs::write(
                layout.join("publication-blocker"),
                b"keep directory nonempty",
            )
            .unwrap();
            app.on_exit(None);
            assert_eq!(fs::read(&backup).unwrap(), old_backup_bytes);
            let restored = crate::state::persistence::load_state().unwrap();
            assert_eq!(restored.workspaces[0].panels[0].id, panel_id.to_string());
            assert_eq!(
                crate::state::scrollback_store::load_leaf_scrollback(
                    &history,
                    panel_id,
                    Some(leaf_id)
                )
                .as_deref(),
                Some("Old recoverable terminal output\n")
            );
            assert!(marker.exists());
        }
        "incremental-pruning-after-layout-ack" => {
            let panel_id = app.workspaces[0].panels[0].id();
            let leaf_id = app.workspaces[0].panels[0].root_leaf_id();
            let old_layout = app.snapshot_state();
            crate::state::persistence::try_save_state(&old_layout).unwrap();
            app.persisted_state = Some(old_layout.clone());
            crate::state::scrollback_store::save_leaf_scrollback_versioned(
                &history,
                panel_id,
                Some(leaf_id),
                1,
                "History retained until removal is durable\n",
            )
            .unwrap();
            app.remember_scrollback_layout();
            app.workspaces[0].panels.clear();
            // The last acknowledged layout still references this closed panel.
            assert!(app.persist_scrollbacks(false));
            let _ = app.drain_persistence_worker();
            assert!(app.scrollback_known_leaves.contains_key(&panel_id));
            assert_eq!(
                crate::state::scrollback_store::load_leaf_scrollback(
                    &history,
                    panel_id,
                    Some(leaf_id)
                )
                .as_deref(),
                Some("History retained until removal is durable\n")
            );
            let current_layout = app.snapshot_state();
            crate::state::persistence::try_save_state(&current_layout).unwrap();
            app.persisted_state = Some(current_layout.clone());
            // Even a matching acknowledgement cannot authorize pruning while
            // an older accepted state write can replace it in the worker.
            let release = hold_stale_autosave(&mut app, old_layout, layout.clone());
            assert!(app.persistence_worker.state_in_flight());
            assert!(app.persist_scrollbacks(false));
            release.join().unwrap();
            let _ = app.drain_persistence_worker();
            assert!(app.scrollback_known_leaves.contains_key(&panel_id));
            assert!(crate::state::scrollback_store::load_leaf_scrollback(
                &history,
                panel_id,
                Some(leaf_id)
            )
            .is_some());
            // After the final layout publication succeeds and the worker is
            // idle, the retained scope is finally eligible for pruning.
            crate::state::persistence::try_save_state(&current_layout).unwrap();
            app.persisted_state = Some(current_layout);
            assert!(app.persist_scrollbacks(false));
            let _ = app.drain_persistence_worker();
            assert!(!app.scrollback_known_leaves.contains_key(&panel_id));
            assert!(crate::state::scrollback_store::load_leaf_scrollback(
                &history,
                panel_id,
                Some(leaf_id)
            )
            .is_none());
            assert!(marker.exists());
        }
        "history-failure" => {
            fs::write(&history, b"blocked history directory").unwrap();
            app.on_exit(None);
            let saved: crate::state::AppState =
                serde_json::from_slice(&fs::read(&layout).unwrap()).unwrap();
            assert_eq!(saved.workspaces[0].name, "Final workspace");
            assert_eq!(fs::read(&history).unwrap(), b"blocked history directory");
            assert!(marker.exists());
        }
        "settings-failure" => {
            let config = crate::utils::app_paths::config_dir().unwrap();
            assert!(config.starts_with(&profile));
            fs::write(&config, b"blocked configuration directory").unwrap();
            app.preferences_worker.save_settings(Default::default());
            app.on_exit(None);
            assert!(app.preferences_worker.drain().is_err());
            let saved: crate::state::AppState =
                serde_json::from_slice(&fs::read(&layout).unwrap()).unwrap();
            assert_eq!(saved.workspaces[0].name, "Final workspace");
            assert!(history.is_dir());
            assert!(marker.exists());
        }
        "lease-lost" => {
            crate::state::persistence::try_save_state(&app.snapshot_state()).unwrap();
            let before = fs::read(&layout).unwrap();
            app.workspaces[0].name = "Must not overwrite successor".to_owned();
            let successor = b"12345 successor-claim timestamp\n";
            fs::write(&marker, successor).unwrap();
            app.on_exit(None);
            assert_eq!(fs::read(&layout).unwrap(), before);
            assert_eq!(fs::read(&marker).unwrap(), successor);
            assert!(!history.exists());
            assert!(!app.persistence_writes_enabled);
        }
        "crash-recovery" => {
            app.persist_final_state().unwrap();
            let saved: crate::state::AppState =
                serde_json::from_slice(&fs::read(&layout).unwrap()).unwrap();
            assert_eq!(saved.workspaces[0].name, "Final workspace");
            assert!(marker.exists(), "crash recovery must remain a dirty run");
        }
        "ack-workspace-close" | "ack-rollover-workspace-close" => {
            let panel_id = app.workspaces[0].panels[0].id();
            let leaf_id = app.workspaces[0].panels[0].root_leaf_id();
            let runtime_id = app.workspaces[0].panels[0].runtime_session_id().unwrap();
            let manager = app.workspaces[0].pty_manager();
            let handle = manager.lock().unwrap().handle(runtime_id).unwrap();
            handle
                .lock()
                .unwrap()
                .feed_output_for_persistence_tests(b"captured once\r\n");
            let log_path = history.join(
                crate::state::scrollback_store::scrollback_leaf_log_file_name(
                    panel_id,
                    Some(leaf_id),
                ),
            );
            if phase == "ack-rollover-workspace-close" {
                fs::create_dir_all(&history).unwrap();
                crate::state::scrollback_log::reset_log(&log_path, 0).unwrap();
                crate::state::scrollback_log::append_frames(
                    &log_path,
                    &crate::state::scrollback_log::encode_frame(
                        u64::MAX,
                        crate::state::scrollback_log::FrameKind::Output,
                        b"exhausted sequence",
                    ),
                )
                .unwrap();
            }
            assert!(app.persist_scrollbacks(false));
            // New output after capture is outside the old ACK's prefix.
            handle
                .lock()
                .unwrap()
                .feed_output_for_persistence_tests(b"after capture\r\n");
            let closed = Workspace::new("Another workspace", None);
            let closed_id = closed.id;
            app.workspaces.push(closed);
            app.close_workspace_confirmed(closed_id);
            let _ = app.drain_persistence_worker();
            assert!(handle.lock().unwrap().pending_log_snapshot().is_empty());
            if phase == "ack-workspace-close" {
                let (_, frames) =
                    crate::state::scrollback_log::read_frames(&fs::read(log_path).unwrap())
                        .unwrap();
                assert_eq!(
                    frames
                        .iter()
                        .map(|frame| frame.payload.as_slice())
                        .collect::<Vec<_>>(),
                    vec![
                        b"captured once\r\n".as_slice(),
                        b"after capture\r\n".as_slice()
                    ],
                    "the accepted first batch must not be appended twice"
                );
                let restored = super::collect_leaf_histories(&history, panel_id);
                assert_eq!(restored.len(), 1);
                assert_eq!(
                    restored[0].2.len(),
                    2,
                    "restart replays each accepted output exactly once"
                );
            } else {
                assert!(
                    !log_path.exists(),
                    "a barrier-consumed rollover still requires a full checkpoint"
                );
                let checkpoint = crate::state::scrollback_store::load_leaf_scrollback(
                    &history,
                    panel_id,
                    Some(leaf_id),
                )
                .unwrap();
                assert!(checkpoint.contains("captured once"));
                assert!(checkpoint.contains("after capture"));
            }
            let saved = crate::state::persistence::load_state().unwrap();
            assert_eq!(saved.workspaces.len(), 1);
            assert!(marker.exists());
        }
        "ack-replaced-session-incremental" | "ack-replaced-session-full" => {
            let panel_id = app.workspaces[0].panels[0].id();
            let leaf_id = app.workspaces[0].panels[0].root_leaf_id();
            let old_runtime = app.workspaces[0].panels[0].runtime_session_id().unwrap();
            let manager = app.workspaces[0].pty_manager();
            let old_handle = manager.lock().unwrap().handle(old_runtime).unwrap();
            old_handle
                .lock()
                .unwrap()
                .feed_output_for_persistence_tests(b"previous session\r\n");
            if phase == "ack-replaced-session-full" {
                let entries = app.full_scrollback_entries(&history, None);
                assert_eq!(entries.len(), 1);
                assert!(app.persistence_worker.submit_full(entries));
            } else {
                assert!(app.persist_scrollbacks(false));
            }
            let workspace_id = app.workspaces[0].id;
            let CanvasPanel::Terminal(panel) = &mut app.workspaces[0].panels[0];
            assert!(panel.replace_focused_session(
                manager.clone(),
                None,
                workspace_id,
                "in-memory-resume".to_owned(),
                "test agent".to_owned()
            ));
            assert_eq!(panel.root_leaf_id(), leaf_id);
            let new_runtime = panel.runtime_session_id().unwrap();
            assert_ne!(old_runtime, new_runtime);
            let new_handle = manager.lock().unwrap().handle(new_runtime).unwrap();
            new_handle
                .lock()
                .unwrap()
                .feed_output_for_persistence_tests(b"replacement session\r\n");
            let pending = new_handle.lock().unwrap().pending_log_snapshot();
            let _ = app.drain_persistence_worker();
            assert_eq!(
                new_handle.lock().unwrap().pending_log_snapshot(),
                pending,
                "the old session's ACK must not remove any new output"
            );
            assert!(app.persist_scrollbacks(false));
            new_handle
                .lock()
                .unwrap()
                .feed_output_for_persistence_tests(b"fresh output after capture\r\n");
            let _ = app.drain_persistence_worker();
            let remaining = new_handle.lock().unwrap().pending_log_snapshot();
            let mut encoded = crate::state::scrollback_log::encode_header(0);
            encoded.extend_from_slice(&remaining);
            let (_, frames) = crate::state::scrollback_log::read_frames(&encoded).unwrap();
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].payload, b"fresh output after capture\r\n");
            assert!(app.persist_scrollbacks(false));
            let _ = app.drain_persistence_worker();
            assert!(new_handle.lock().unwrap().pending_log_snapshot().is_empty());
            let restored = super::collect_leaf_histories(&history, panel_id);
            assert_eq!(restored.len(), 1);
            let expected = if phase == "ack-replaced-session-full" {
                vec![
                    b"replacement session\r\n".as_slice(),
                    b"fresh output after capture\r\n".as_slice(),
                ]
            } else {
                vec![
                    b"previous session\r\n".as_slice(),
                    b"replacement session\r\n".as_slice(),
                    b"fresh output after capture\r\n".as_slice(),
                ]
            };
            assert_eq!(
                restored[0]
                    .2
                    .iter()
                    .map(|frame| frame.payload.as_slice())
                    .collect::<Vec<_>>(),
                expected
            );
            if phase == "ack-replaced-session-full" {
                assert!(restored[0].1.contains("previous session"));
            }
            assert!(marker.exists());
        }
        "ack-restoring-shutdown" | "ack-restoring-completed" => {
            let panel_id = app.workspaces[0].panels[0].id();
            let leaf_id = app.workspaces[0].panels[0].root_leaf_id();
            let runtime_id = app.workspaces[0].panels[0].runtime_session_id().unwrap();
            let manager = app.workspaces[0].pty_manager();
            let handle = manager.lock().unwrap().handle(runtime_id).unwrap();
            // A detached panel owns no new output and must not make the
            // eventual complete save fail just because it has no live PTY.
            app.workspaces[0]
                .panels
                .push(CanvasPanel::Terminal(TerminalPanel::new(
                    pos2(310.0, 0.0),
                    vec2(300.0, 200.0),
                    Color32::WHITE,
                    2,
                )));
            crate::state::scrollback_store::save_leaf_scrollback_versioned(
                &history,
                panel_id,
                Some(leaf_id),
                3,
                "old durable history\n",
            )
            .unwrap();
            let checkpoint_path = history.join(
                crate::state::scrollback_store::scrollback_leaf_file_name(panel_id, Some(leaf_id)),
            );
            let checkpoint_before = fs::read(&checkpoint_path).unwrap();
            let log_path = history.join(
                crate::state::scrollback_store::scrollback_leaf_log_file_name(
                    panel_id,
                    Some(leaf_id),
                ),
            );
            crate::state::scrollback_log::reset_log(&log_path, 3).unwrap();
            crate::state::scrollback_log::append_frames(
                &log_path,
                &crate::state::scrollback_log::encode_frame(
                    1,
                    crate::state::scrollback_log::FrameKind::Output,
                    b"old incremental tail\r\n",
                ),
            )
            .unwrap();
            let old_frames = super::load_leaf_session_frames(&history, panel_id, Some(leaf_id));
            let closed_panel = Uuid::new_v4();
            crate::state::scrollback_store::save_scrollback(
                &history,
                closed_panel,
                "retained recovery for closed panel\n",
            )
            .unwrap();
            app.scrollback_known_leaves.entry(closed_panel).or_default();
            handle
                .lock()
                .unwrap()
                .feed_output_for_persistence_tests(b"new output during restore\r\n");
            let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
            let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
            handle.lock().unwrap().replay_session_preserving_live(
                b"old durable history\r\n",
                &old_frames,
                move |term| {
                    let output = crate::terminal::export::scrollback_to_ansi(term).into_bytes();
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    output
                },
            );
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(handle.lock().unwrap().history_restore_in_progress());
            // The real replay worker is blocked while holding `term`. Saving
            // must use the pending frame prefix without waiting for that grid.
            app.on_exit(None);
            assert!(
                marker.exists(),
                "unfinished replay cannot justify clean shutdown"
            );
            assert_eq!(
                fs::read(&checkpoint_path).unwrap(),
                checkpoint_before,
                "partial live grid must not replace the old checkpoint"
            );
            let restored = super::collect_leaf_histories(&history, panel_id);
            assert_eq!(restored.len(), 1);
            assert_eq!(restored[0].1, "old durable history\n");
            assert_eq!(
                restored[0]
                    .2
                    .iter()
                    .map(|frame| frame.payload.as_slice())
                    .collect::<Vec<_>>(),
                vec![
                    b"old incremental tail\r\n".as_slice(),
                    b"new output during restore\r\n".as_slice()
                ]
            );
            assert!(
                handle.lock().unwrap().pending_log_snapshot().is_empty(),
                "only the rescued prefix is acknowledged"
            );
            assert!(
                crate::state::scrollback_store::load_scrollback(&history, closed_panel).is_some(),
                "incomplete checkpoints cannot authorize pruning"
            );
            release_tx.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while handle.lock().unwrap().history_restore_in_progress() {
                assert!(Instant::now() < deadline, "released replay did not finish");
                std::thread::sleep(Duration::from_millis(5));
            }
            if phase == "ack-restoring-completed" {
                // The app may continue after a refused installation/save. A
                // later autosave must use the sequence AFTER the rescued tail.
                handle
                    .lock()
                    .unwrap()
                    .feed_output_for_persistence_tests(b"output after rescue\r\n");
                assert!(app.persist_scrollbacks(false));
                let _ = app.drain_persistence_worker();
                let (_, frames) =
                    crate::state::scrollback_log::read_frames(&fs::read(&log_path).unwrap())
                        .unwrap();
                assert_eq!(
                    frames.iter().map(|frame| frame.seq).collect::<Vec<_>>(),
                    vec![1, 2, 3]
                );
                assert_eq!(frames[2].payload, b"output after rescue\r\n");
                app.on_exit(None);
                assert!(!marker.exists());
                assert!(!log_path.exists());
                let checkpoint = crate::state::scrollback_store::load_leaf_scrollback(
                    &history,
                    panel_id,
                    Some(leaf_id),
                )
                .unwrap();
                for output in [
                    "old durable history",
                    "old incremental tail",
                    "new output during restore",
                    "output after rescue",
                ] {
                    assert_eq!(
                        checkpoint.matches(output).count(),
                        1,
                        "missing or duplicated output: {output}"
                    );
                }
                assert!(
                    crate::state::scrollback_store::load_scrollback(&history, closed_panel)
                        .is_none()
                );
            }
        }
        "ack-capture-pending" | "ack-capture-ready" | "ack-capture-legacy" => {
            let panel_id = app.workspaces[0].panels[0].id();
            let leaf_id = app.workspaces[0].panels[0].root_leaf_id();
            let runtime_id = app.workspaces[0].panels[0].runtime_session_id().unwrap();
            let manager = app.workspaces[0].pty_manager();
            let handle = manager.lock().unwrap().handle(runtime_id).unwrap();
            let legacy = phase == "ack-capture-legacy";
            let stored_leaf = if legacy { None } else { Some(leaf_id) };
            crate::state::scrollback_store::save_leaf_scrollback_versioned(
                &history,
                panel_id,
                stored_leaf,
                3,
                "old captured history\n",
            )
            .unwrap();
            let checkpoint_path = history.join(
                crate::state::scrollback_store::scrollback_leaf_file_name(panel_id, stored_leaf),
            );
            let checkpoint_before = fs::read(&checkpoint_path).unwrap();
            let log_path = history.join(
                crate::state::scrollback_store::scrollback_leaf_log_file_name(
                    panel_id,
                    stored_leaf,
                ),
            );
            crate::state::scrollback_log::reset_log(&log_path, 3).unwrap();
            crate::state::scrollback_log::append_frames(
                &log_path,
                &crate::state::scrollback_log::encode_frame(
                    1,
                    crate::state::scrollback_log::FrameKind::Output,
                    b"old captured tail\r\n",
                ),
            )
            .unwrap();
            let log_before = fs::read(&log_path).unwrap();
            let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
            let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
            app.persistence_worker =
                super::persistence_worker::PersistenceWorker::with_restore_processor_for_tests(
                    move |dir, panel| {
                        entered_tx.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        super::collect_leaf_histories(dir, panel)
                    },
                );
            app.scrollback_restored.remove(&panel_id);
            app.scrollback_restore_pending.insert(panel_id);
            assert!(app
                .persistence_worker
                .submit_restore(history.clone(), vec![(panel_id, leaf_id)]));
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            handle
                .lock()
                .unwrap()
                .feed_output_for_persistence_tests(b"new output before apply\r\n");
            // This batch is captured before Completion::Restore installs the
            // legacy alias in the app. The writer must route it only after
            // capturing the immutable old history on its FIFO.
            assert!(app.persist_scrollbacks(false));
            assert_eq!(fs::read(&log_path).unwrap(), log_before);
            release_tx.send(()).unwrap();
            if phase != "ack-capture-pending" {
                let _ = app.drain_persistence_worker();
                assert!(!app.scrollback_restore_pending.contains(&panel_id));
                assert!(app.scrollback_restore_ready.contains_key(&panel_id));
            } else {
                assert!(app.persistence_worker.restore_in_flight());
                assert!(app.scrollback_restore_pending.contains(&panel_id));
            }
            // Both a capture completion not yet consumed and a ready capture
            // not yet replayed must preserve the old checkpoint on shutdown.
            app.on_exit(None);
            assert!(marker.exists());
            assert_eq!(fs::read(&checkpoint_path).unwrap(), checkpoint_before);
            let captured = &app.scrollback_restore_ready[&panel_id];
            assert_eq!(captured.len(), 1);
            assert_eq!(captured[0].1, "old captured history\n");
            assert_eq!(captured[0].2.len(), 1);
            assert_eq!(captured[0].2[0].payload, b"old captured tail\r\n");
            let restored = super::collect_leaf_histories(&history, panel_id);
            assert_eq!(restored.len(), 1);
            assert_eq!(restored[0].0, stored_leaf);
            assert_eq!(restored[0].1, "old captured history\n");
            assert_eq!(
                restored[0]
                    .2
                    .iter()
                    .map(|frame| frame.payload.as_slice())
                    .collect::<Vec<_>>(),
                vec![
                    b"old captured tail\r\n".as_slice(),
                    b"new output before apply\r\n".as_slice(),
                ]
            );
            let canonical_checkpoint_path = history.join(
                crate::state::scrollback_store::scrollback_leaf_file_name(panel_id, Some(leaf_id)),
            );
            let canonical_log_path = history.join(
                crate::state::scrollback_store::scrollback_leaf_log_file_name(
                    panel_id,
                    Some(leaf_id),
                ),
            );
            if legacy {
                assert!(!canonical_checkpoint_path.exists());
                assert!(!canonical_log_path.exists());
            }
            app.restore_pending_scrollbacks();
            let deadline = Instant::now() + Duration::from_secs(3);
            while handle.lock().unwrap().history_restore_in_progress() {
                assert!(Instant::now() < deadline, "captured replay did not finish");
                std::thread::sleep(Duration::from_millis(5));
            }
            if legacy {
                // A complete save can also occur before update installation.
                // Keep the run claim so subsequent output exercises the
                // writer's transition from legacy to canonical storage.
                app.persist_final_state().unwrap();
                assert!(marker.exists());
            } else {
                app.on_exit(None);
                assert!(!marker.exists());
            }
            let checkpoint = crate::state::scrollback_store::load_leaf_scrollback(
                &history,
                panel_id,
                Some(leaf_id),
            )
            .unwrap();
            for output in [
                "old captured history",
                "old captured tail",
                "new output before apply",
            ] {
                assert_eq!(
                    checkpoint.matches(output).count(),
                    1,
                    "missing or duplicated output: {output}"
                );
            }
            if legacy {
                assert_eq!(fs::read(&checkpoint_path).unwrap(), checkpoint_before);
                let legacy_log_before = fs::read(&log_path).unwrap();
                handle
                    .lock()
                    .unwrap()
                    .feed_output_for_persistence_tests(b"after canonical checkpoint\r\n");
                assert!(app.persist_scrollbacks(false));
                let _ = app.drain_persistence_worker();
                assert_eq!(fs::read(&log_path).unwrap(), legacy_log_before);
                let (_, frames) = crate::state::scrollback_log::read_frames(
                    &fs::read(&canonical_log_path).unwrap(),
                )
                .unwrap();
                assert_eq!(frames.len(), 1);
                assert_eq!(frames[0].seq, 1);
                assert_eq!(frames[0].payload, b"after canonical checkpoint\r\n");
                let restored = super::collect_leaf_histories(&history, panel_id);
                let stable_root = restored
                    .iter()
                    .find(|(leaf, _, _)| *leaf == Some(leaf_id))
                    .unwrap();
                assert_eq!(stable_root.1, checkpoint);
                assert_eq!(stable_root.2.len(), 1);
                assert_eq!(stable_root.2[0].payload, b"after canonical checkpoint\r\n");
                app.on_exit(None);
                assert!(!marker.exists());
            }
        }
        _ => panic!("unknown isolated fixture phase: {phase}"),
    }
}

#[test]
fn taskbar_reveals_a_focused_terminal_beyond_the_window_width() {
    use egui_kittest::{kittest::Queryable, Harness};
    let ctx = egui::Context::default();
    let mut app = super::TerminalApp::new_for_tests(&ctx);
    app.ws_mut().panels.clear();
    let mut ids = Vec::new();
    for index in 0..12 {
        let mut panel = CanvasPanel::Terminal(TerminalPanel::new(
            pos2(0.0, 0.0),
            vec2(300.0, 200.0),
            Color32::WHITE,
            index,
        ));
        panel.set_title(format!("Terminal {index}"));
        panel.set_focused(index == 0);
        ids.push(panel.id());
        app.ws_mut().panels.push(panel);
    }
    let mut harness = Harness::builder()
        .with_size(vec2(600.0, 400.0))
        .build_state(
            |ctx, app: &mut super::TerminalApp| {
                app.show_taskbar(ctx);
                CentralPanel::default().show(ctx, |_| {});
            },
            app,
        );
    harness.state_mut().ws_mut().bring_to_front(ids[11]);
    harness.run();
    let last = harness.get_by_label("Terminal 11").rect();
    assert!(
        last.left() >= 0.0 && last.right() <= 600.0,
        "last terminal must be visible: {last:?}"
    );
    harness.state_mut().ws_mut().bring_to_front(ids[0]);
    harness.run();
    let first = harness.get_by_label("Terminal 0").rect();
    assert!(first.left() >= 0.0 && first.right() <= 600.0);
}

#[test]
fn docked_viewer_switches_keyboard_ownership_by_pointer_and_escape() {
    use egui_kittest::{kittest::Queryable, Harness};
    let ctx = egui::Context::default();
    let mut app = super::TerminalApp::new_for_tests(&ctx);
    app.file_viewer = Some(super::file_viewer_ui::FileViewerState {
        path: "example.rs".into(),
        lines: vec!["fn main() {}".into()],
        truncated: false,
        binary: false,
        highlighted: Vec::new(),
        highlight_token: None,
        language: None,
        loading: false,
    });
    let mut harness = Harness::builder()
        .with_size(vec2(1100.0, 700.0))
        .build_state(
            |ctx, app: &mut super::TerminalApp| {
                app.show_file_viewer(ctx);
                CentralPanel::default().show(ctx, |ui| {
                    ui.label("Terminal area");
                });
            },
            app,
        );
    harness.get_by_label("example.rs").click();
    harness.run();
    assert!(!harness.state().terminal_input_is_routable());
    harness.get_by_label("Terminal area").click();
    harness.run();
    assert!(harness.state().terminal_input_is_routable());
    harness.key_press(egui::Key::Escape);
    harness.run();
    assert!(
        harness.state().file_viewer.is_some(),
        "Escape aimed at the terminal must leave the viewer open"
    );
    harness.get_by_label("example.rs").click();
    harness.run();
    harness.key_press(egui::Key::Escape);
    harness.run();
    assert!(harness.state().file_viewer.is_none());
}

#[test]
fn docked_viewer_only_captures_keyboard_when_its_content_is_active() {
    let ctx = egui::Context::default();
    let mut app = super::TerminalApp::new_for_tests(&ctx);
    app.file_viewer = Some(super::file_viewer_ui::FileViewerState {
        path: "example.rs".into(),
        lines: vec!["fn main() {}".into()],
        truncated: false,
        binary: false,
        highlighted: Vec::new(),
        highlight_token: None,
        language: None,
        loading: false,
    });
    assert!(!app.modal_input_is_active());
    assert!(app.terminal_input_is_routable());
    app.file_viewer_keyboard_active = true;
    assert!(!app.terminal_input_is_routable());
    assert!(
        !app.modal_input_is_active(),
        "global commands remain available"
    );
    app.file_viewer_keyboard_active = false;
    assert!(app.terminal_input_is_routable());
}

#[test]
fn test_app_never_starts_the_network_update_checker() {
    let ctx = egui::Context::default();
    let app = super::TerminalApp::new_for_tests(&ctx);

    assert!(matches!(
        app.update_checker.snapshot().status,
        crate::update::UpdateStatus::Disabled
    ));
}

#[test]
fn modal_collaboration_dialogs_capture_terminal_input() {
    let ctx = egui::Context::default();
    let mut app = super::TerminalApp::new_for_tests(&ctx);
    assert!(app.terminal_input_is_routable());

    app.share_workspace_open = true;
    assert!(!app.terminal_input_is_routable());
    app.share_workspace_open = false;

    app.join_session_open = true;
    assert!(!app.terminal_input_is_routable());
}

#[test]
fn memory_dialog_captures_terminal_input() {
    let ctx = egui::Context::default();
    let mut app = super::TerminalApp::new_for_tests(&ctx);
    assert!(app.terminal_input_is_routable());

    app.open_memory_hub();

    assert!(app.memory_ui.is_some());
    assert!(!app.terminal_input_is_routable());
}

#[test]
fn closing_a_project_requires_confirmation_and_captures_terminal_input() {
    let ctx = egui::Context::default();
    let mut app = super::TerminalApp::new_for_tests(&ctx);
    let workspace_id = app.workspaces[0].id;

    app.handle_sidebar_responses(
        vec![crate::sidebar::SidebarResponse::RequestCloseWorkspace(
            workspace_id,
        )],
        &ctx,
    );

    assert_eq!(app.closing_workspace, Some(workspace_id));
    assert!(!app.terminal_input_is_routable());
    assert_eq!(app.workspaces.len(), 1, "todavía no debe cerrarse");
}

#[test]
fn confirming_close_terminates_the_project_terminals_and_leaves_a_clean_workspace() {
    let ctx = egui::Context::default();
    let mut app = super::TerminalApp::new_for_tests(&ctx);
    let workspace_id = app.workspaces[0].id;
    assert_eq!(app.workspaces[0].panels.len(), 1);

    app.close_workspace_confirmed(workspace_id);

    assert_eq!(app.workspaces.len(), 1);
    assert!(app.workspaces[0].cwd().is_none());
    assert!(app.workspaces[0].panels.is_empty());
}

#[test]
fn closing_a_shared_background_project_stops_its_collaboration_session() {
    assert!(should_stop_collaboration_on_workspace_close(
        CollabMode::Host,
        false,
        true,
    ));
    assert!(!should_stop_collaboration_on_workspace_close(
        CollabMode::Host,
        false,
        false,
    ));
}

#[test]
fn consumed_app_shortcut_is_removed_from_the_pty_event_stream() {
    let ctx = egui::Context::default();
    let modifiers = Modifiers {
        ctrl: true,
        shift: true,
        ..Modifiers::NONE
    };
    let shortcut = egui::Event::Key {
        key: egui::Key::T,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    };
    ctx.begin_pass(RawInput {
        events: vec![shortcut, egui::Event::Text("keep".to_owned())],
        ..Default::default()
    });

    super::consume_key_event(&ctx, modifiers, egui::Key::T);

    let remaining = ctx.input(|input| input.events.clone());
    let _ = ctx.end_pass();
    assert_eq!(remaining, vec![egui::Event::Text("keep".to_owned())]);
}

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
    let front = TerminalPanel::new(pos2(20.0, 20.0), vec2(300.0, 200.0), Color32::LIGHT_BLUE, 1);
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
fn taskbar_accent_is_bright_for_codex() {
    assert_eq!(
        taskbar_provider_accent(AgentProvider::CodexCli),
        Color32::from_rgb(120, 190, 255)
    );
}

#[test]
fn auto_layout_uses_full_screen_for_single_panel() {
    use super::taskbar::auto_layout_slots;
    let slots = auto_layout_slots(1);
    assert_eq!(slots, vec![SnapSlot::Maximized]);
}

#[test]
fn auto_layout_uses_left_right_split_for_two_panels() {
    use super::taskbar::auto_layout_slots;
    let slots = auto_layout_slots(2);
    assert_eq!(slots, vec![SnapSlot::LeftHalf, SnapSlot::RightHalf]);
}

#[test]
fn auto_tile_keeps_slots_stable_when_z_order_changes() {
    let mut workspace = Workspace::new("Desktop", None);
    for i in 0..4 {
        let panel = TerminalPanel::new(
            pos2(40.0 * i as f32, 40.0 * i as f32),
            vec2(480.0, 320.0),
            Color32::WHITE,
            i,
        );
        workspace.add_restored_terminal(panel);
    }
    let desktop = Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 720.0));
    super::taskbar::auto_tile_workspace(&mut workspace, desktop);
    let slots_before: Vec<_> = workspace
        .panels
        .iter()
        .map(|panel| panel.placement().clone())
        .collect();

    // Click en un panel del fondo (cambia el z-order) y mover el sash
    // (invalida la firma del tile): antes esto intercambiaba las terminales
    // de slot; ahora cada una conserva el suyo.
    let first_id = workspace.panels[0].id();
    workspace.bring_to_front(first_id);
    workspace.split_x = 0.6;
    super::taskbar::auto_tile_workspace(&mut workspace, desktop);

    let slots_after: Vec<_> = workspace
        .panels
        .iter()
        .map(|panel| panel.placement().clone())
        .collect();
    assert_eq!(slots_before, slots_after);
}

#[test]
fn auto_layout_uses_quadrants_for_four_or_more_panels() {
    use super::taskbar::auto_layout_slots;
    let slots = auto_layout_slots(4);
    assert_eq!(
        slots,
        vec![
            SnapSlot::TopLeft,
            SnapSlot::TopRight,
            SnapSlot::BottomLeft,
            SnapSlot::BottomRight,
        ]
    );
    assert_eq!(auto_layout_slots(7), auto_layout_slots(4));
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
fn snap_slot_for_pointer_maps_top_edge_to_maximize() {
    let desktop = Rect::from_min_max(pos2(16.0, 16.0), pos2(1016.0, 716.0));

    let slot = desktop_snap_slot_for_pointer(pos2(420.0, 18.0), desktop);

    assert_eq!(slot, Some(SnapSlot::Maximized));
}

#[test]
fn snap_slot_for_pointer_maps_bottom_left_corner() {
    let desktop = Rect::from_min_max(pos2(16.0, 16.0), pos2(1016.0, 716.0));

    let slot = desktop_snap_slot_for_pointer(pos2(18.0, 714.0), desktop);

    assert_eq!(slot, Some(SnapSlot::BottomLeft));
}

#[test]
fn snap_slot_for_pointer_recognizes_top_left_without_touching_exact_corner() {
    let desktop = Rect::from_min_max(pos2(16.0, 16.0), pos2(1016.0, 716.0));

    let slot = desktop_snap_slot_for_pointer(pos2(220.0, 28.0), desktop);

    assert_eq!(slot, Some(SnapSlot::TopLeft));
}

#[test]
fn snap_slot_for_pointer_recognizes_top_right_from_right_band() {
    let desktop = Rect::from_min_max(pos2(16.0, 16.0), pos2(1016.0, 716.0));

    let slot = desktop_snap_slot_for_pointer(pos2(1008.0, 150.0), desktop);

    assert_eq!(slot, Some(SnapSlot::TopRight));
}

#[test]
fn required_snap_slot_keeps_previous_slot_in_center_dead_zone() {
    let desktop = Rect::from_min_max(pos2(16.0, 16.0), pos2(1016.0, 716.0));

    let slot =
        desktop_required_snap_slot_for_pointer(pos2(520.0, 360.0), desktop, SnapSlot::LeftHalf);

    assert_eq!(slot, SnapSlot::LeftHalf);
}

#[test]
fn required_snap_slot_maps_interior_side_zones_without_waiting_for_edge() {
    let desktop = Rect::from_min_max(pos2(16.0, 16.0), pos2(1016.0, 716.0));

    let left =
        desktop_required_snap_slot_for_pointer(pos2(260.0, 360.0), desktop, SnapSlot::Maximized);
    let right =
        desktop_required_snap_slot_for_pointer(pos2(770.0, 360.0), desktop, SnapSlot::Maximized);

    assert_eq!(left, SnapSlot::LeftHalf);
    assert_eq!(right, SnapSlot::RightHalf);
}

#[test]
fn slot_drag_requires_intent_before_committing_a_slot() {
    assert!(!slot_drag_started(vec2(3.0, 3.0)));
    assert!(slot_drag_started(vec2(6.0, 0.0)));
}

#[test]
fn split_resize_hit_detects_vertical_sash_between_two_halves() {
    let mut workspace = Workspace::new("Desktop", None);
    let left = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
    let left_id = left.id;
    let right = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::LIGHT_BLUE, 1);
    let right_id = right.id;
    workspace.add_restored_terminal(left);
    workspace.add_restored_terminal(right);
    let desktop = Rect::from_min_max(pos2(0.0, 0.0), pos2(1200.0, 800.0));
    workspace.snap_panel(left_id, SnapSlot::LeftHalf, desktop);
    workspace.snap_panel(right_id, SnapSlot::RightHalf, desktop);

    let hit = split_resize_hit(&workspace, pos2(600.0, 300.0)).expect("split hit");

    assert_eq!(hit.axis, SplitResizeAxis::Vertical);
    assert_eq!(hit.boundary, 600.0);
}

#[test]
fn split_resize_hit_detects_horizontal_sash_between_top_and_bottom() {
    let mut workspace = Workspace::new("Desktop", None);
    let top = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
    let top_id = top.id;
    let bottom = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::LIGHT_BLUE, 1);
    let bottom_id = bottom.id;
    workspace.add_restored_terminal(top);
    workspace.add_restored_terminal(bottom);
    let desktop = Rect::from_min_max(pos2(0.0, 0.0), pos2(1200.0, 800.0));
    workspace.snap_panel(top_id, SnapSlot::TopHalf, desktop);
    workspace.snap_panel(bottom_id, SnapSlot::BottomHalf, desktop);

    let hit = split_resize_hit(&workspace, pos2(700.0, 400.0)).expect("split hit");

    assert_eq!(hit.axis, SplitResizeAxis::Horizontal);
    assert_eq!(hit.boundary, 400.0);
}

#[test]
fn window_transition_progress_is_clamped() {
    let transition = WindowTransition {
        kind: WindowTransitionKind::Minimizing,
        from_rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 80.0)),
        to_rect: Rect::from_min_size(pos2(40.0, 20.0), vec2(30.0, 20.0)),
        started_at: 10.0,
        duration: 0.14,
    };

    assert_eq!(transition.progress(9.0), 0.0);
    assert_eq!(transition.progress(10.14), 1.0);
    assert!(transition.finished(10.2));
}

#[test]
fn window_transition_rect_interpolates_between_endpoints() {
    let transition = WindowTransition {
        kind: WindowTransitionKind::Restoring,
        from_rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 24.0)),
        to_rect: Rect::from_min_size(pos2(200.0, 120.0), vec2(480.0, 320.0)),
        started_at: 0.0,
        duration: 0.14,
    };

    assert_eq!(transition.current_rect(0.0), transition.from_rect);
    assert_eq!(transition.current_rect(0.14), transition.to_rect);
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

            let mut drawn_panel = TerminalPanel::new(panel_pos, panel_size, Color32::LIGHT_BLUE, 1);
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
fn closing_the_last_project_leaves_one_empty_workspace() {
    let project = Workspace::from_folder(unique_temp_dir("close-last-project"));
    let project_id = project.id;
    let mut workspaces = vec![project];
    let mut active_ws = 0;

    let closed_terminals =
        close_workspace_for_good(&mut workspaces, &mut active_ws, project_id).unwrap();

    assert_eq!(closed_terminals, 0);
    assert_eq!(active_ws, 0);
    assert_eq!(workspaces.len(), 1);
    assert!(workspaces[0].cwd().is_none());
    assert!(workspaces[0].panels.is_empty());
}

#[test]
fn closing_a_workspace_before_the_active_one_keeps_the_same_project_selected() {
    let first = Workspace::from_folder(unique_temp_dir("close-first"));
    let first_id = first.id;
    let second = Workspace::from_folder(unique_temp_dir("keep-second"));
    let second_id = second.id;
    let mut workspaces = vec![first, second];
    let mut active_ws = 1;

    close_workspace_for_good(&mut workspaces, &mut active_ws, first_id).unwrap();

    assert_eq!(active_ws, 0);
    assert_eq!(workspaces[active_ws].id, second_id);
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

#[test]
fn panel_id_for_hit_returns_none_for_stale_index() {
    let workspace = Workspace::new("Default", None);
    let hit = super::desktop::PanelHit {
        index: 0,
        area: PanelHitArea::TitleBar,
        pointer: pos2(0.0, 0.0),
    };

    assert_eq!(super::panel_id_for_hit(&workspace, &hit), None);
}

#[test]
fn split_resize_ids_return_none_for_stale_indexes() {
    let workspace = Workspace::new("Default", None);
    let hit = super::desktop::SplitResizeHit {
        leading_index: 0,
        trailing_index: 1,
        axis: SplitResizeAxis::Vertical,
        boundary: 100.0,
        hit_rect: Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 10.0)),
    };

    assert_eq!(super::split_resize_panel_ids(&workspace, &hit), None);
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
        .next_back()
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

#[test]
fn session_frames_replay_when_generation_matches() {
    use crate::state::scrollback_log::{encode_frame, FrameKind};
    use crate::state::scrollback_store::scrollback_log_file_name;
    let dir = std::env::temp_dir().join(format!("tc-frames-ok-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let panel = Uuid::new_v4();

    let mut bytes = crate::state::scrollback_log::encode_header(0);
    bytes.extend_from_slice(&encode_frame(1, FrameKind::Output, b"hola\r\n"));
    std::fs::write(dir.join(scrollback_log_file_name(panel)), &bytes).unwrap();

    let frames = super::load_session_frames(&dir, panel);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(frames.len(), 1, "gen 0 == gen 0 por defecto");
}

#[test]
fn session_frames_ignored_on_generation_mismatch() {
    use crate::state::scrollback_log::{encode_frame, FrameKind};
    use crate::state::scrollback_store::scrollback_log_file_name;
    let dir = std::env::temp_dir().join(format!("tc-frames-mismatch-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let panel = Uuid::new_v4();

    // Checkpoint de generation 3 pero log viejo de generation 2: se ignora.
    super::write_generation(&dir, panel, 3).unwrap();
    let mut bytes = crate::state::scrollback_log::encode_header(2);
    bytes.extend_from_slice(&encode_frame(1, FrameKind::Output, b"viejo\r\n"));
    std::fs::write(dir.join(scrollback_log_file_name(panel)), &bytes).unwrap();

    let frames = super::load_session_frames(&dir, panel);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(frames.is_empty(), "mismatch de generation descarta el log");
}

#[test]
fn atomic_checkpoint_generation_ignores_an_old_log_left_by_a_crash() {
    use crate::state::scrollback_log::{encode_frame, FrameKind};
    use crate::state::scrollback_store::scrollback_log_file_name;
    let dir = unique_temp_dir("tc-atomic-checkpoint");
    let panel = Uuid::new_v4();

    crate::state::scrollback_store::save_leaf_scrollback_versioned(
        &dir,
        panel,
        None,
        3,
        "checkpoint nuevo\r\n",
    )
    .unwrap();
    let mut stale_log = crate::state::scrollback_log::encode_header(2);
    stale_log.extend_from_slice(&encode_frame(7, FrameKind::Output, b"duplicado\r\n"));
    std::fs::write(dir.join(scrollback_log_file_name(panel)), stale_log).unwrap();

    assert!(super::load_session_frames(&dir, panel).is_empty());
    assert_eq!(
        crate::state::scrollback_store::load_leaf_scrollback(&dir, panel, None).as_deref(),
        Some("checkpoint nuevo\r\n")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn session_frames_survive_a_truncated_tail() {
    use crate::state::scrollback_log::{encode_frame, FrameKind};
    use crate::state::scrollback_store::scrollback_log_file_name;
    let dir = std::env::temp_dir().join(format!("tc-frames-trunc-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let panel = Uuid::new_v4();

    let mut bytes = crate::state::scrollback_log::encode_header(0);
    bytes.extend_from_slice(&encode_frame(1, FrameKind::Output, b"completo\r\n"));
    bytes.push(FrameKind::Output as u8);
    bytes.extend_from_slice(&999u32.to_le_bytes());
    bytes.extend_from_slice(b"trun"); // crash a mitad de frame
    std::fs::write(dir.join(scrollback_log_file_name(panel)), &bytes).unwrap();

    let frames = super::load_session_frames(&dir, panel);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(frames.len(), 1, "solo llega el frame completo");
    assert_eq!(frames[0].payload, b"completo\r\n".to_vec());
}

#[test]
fn split_logs_restore_without_a_prior_checkpoint_and_never_mix_leaves() {
    use crate::state::scrollback_log::{encode_frame, FrameKind};

    let dir = std::env::temp_dir().join(format!("tc-leaf-logs-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let panel = Uuid::new_v4();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();

    assert_eq!(
        super::persist_incremental_frames(
            &dir,
            panel,
            Some(first),
            &encode_frame(1, FrameKind::Output, b"primera\r\n"),
        ),
        Some(false)
    );
    assert_eq!(
        super::persist_incremental_frames(
            &dir,
            panel,
            Some(second),
            &encode_frame(1, FrameKind::Output, b"segunda\r\n"),
        ),
        Some(false)
    );

    let histories = super::collect_leaf_histories(&dir, panel);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(histories.len(), 2);
    assert_eq!(histories[0].0, Some(first.min(second)));
    for (leaf, checkpoint, frames) in histories {
        assert!(checkpoint.is_empty(), "el test no creó checkpoints");
        assert_eq!(frames.len(), 1);
        let expected = if leaf == Some(first) {
            b"primera\r\n".as_slice()
        } else {
            b"segunda\r\n".as_slice()
        };
        assert_eq!(frames[0].payload, expected);
    }
}

#[test]
fn empty_canonical_root_marker_suppresses_stale_legacy_history() {
    let dir = unique_temp_dir("tc-empty-root-marker");
    let panel = Uuid::new_v4();
    let root = Uuid::new_v4();

    crate::state::scrollback_store::save_leaf_scrollback(
        &dir,
        panel,
        None,
        "historial legado que ya no debe volver\n",
    )
    .unwrap();
    super::write_leaf_generation(&dir, panel, Some(root), 1).unwrap();

    let histories = super::collect_leaf_histories(&dir, panel);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(histories.iter().any(|(leaf, text, frames)| {
        *leaf == Some(root) && text.is_empty() && frames.is_empty()
    }));
    assert!(histories.iter().any(|(leaf, _, _)| leaf.is_none()));
    // `TerminalPanel::restore_leaf_histories` da precedencia a Some(root),
    // por lo que el alias legado se omite y no resucita contenido obsoleto.
}
