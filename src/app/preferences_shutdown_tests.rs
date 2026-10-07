//! Closing barriers must apply read/import completions and flush any saves
//! derived from them, including when a previous settings write refused closure.

use std::path::PathBuf;
use std::sync::mpsc;

use uuid::Uuid;

use super::code_review_ui::CodeReviewState;
use super::preferences_worker::{Completion, Job, PreferencesWorker};
use super::TerminalApp;
use crate::orchestration::{save_notes_to_path, DiffNotes};
use crate::state::Workspace;

struct TemporaryNotes(PathBuf);

impl TemporaryNotes {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "terminal-canvas-preferences-shutdown-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }

    fn path(&self) -> PathBuf {
        self.0.join("review-notes.json")
    }
}

impl Drop for TemporaryNotes {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn detached_app() -> TerminalApp {
    let ctx = egui::Context::default();
    let workspace = Workspace::new("Preferences shutdown regression", None);
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
    let app = TerminalApp::build(&ctx, None, Some(state), None, false, false);
    assert!(app
        .workspaces
        .iter()
        .all(|workspace| workspace.panels.is_empty()));
    app
}

#[test]
fn refused_preferences_shutdown_applies_loaded_and_imported_notes_and_flushes_the_merge() {
    let temporary = TemporaryNotes::new();
    let path = temporary.path();
    let root = PathBuf::from("controlled-shutdown-repository");
    let key = Uuid::new_v4();
    let mut disk_notes = DiffNotes::default();
    disk_notes.add(
        "src/á.rs",
        None,
        7,
        "Existing disk note: 👩🏽‍💻\r\nkeep its bytes",
    );
    save_notes_to_path(&path, &disk_notes).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut imported = disk_notes.clone();
    imported.add("src/otro.rs", None, 12, "Nueva nota importada: 日本語");
    let expected = imported.clone();
    let expected_bytes = serde_json::to_vec_pretty(&expected).unwrap();
    assert_ne!(before, expected_bytes);

    let writer_path = path.clone();
    let writer_root = root.clone();
    let loaded = disk_notes.clone();
    let (accepted, observed) = mpsc::channel();
    let worker = PreferencesWorker::with_processor_for_tests(move |job| match job {
        Job::SaveSettings(_) => {
            let _ = accepted.send("settings-error");
            Completion::SettingsSaved(Err(anyhow::anyhow!("controlled retained settings failure")))
        }
        Job::LoadNotes(actual_key, request, actual_root) => {
            assert_eq!(*actual_key, key);
            assert_eq!(actual_root, &writer_root);
            let _ = accepted.send("load");
            Completion::NotesLoaded {
                key: *actual_key,
                request: *request,
                repo_root: actual_root.clone(),
                result: Ok((loaded.clone(), true)),
            }
        }
        Job::ImportNotes(actual_key, request, actual_root) => {
            assert_eq!(*actual_key, key);
            assert_eq!(actual_root, &writer_root);
            let _ = accepted.send("import");
            Completion::NotesImported {
                key: *actual_key,
                request: *request,
                repo_root: actual_root.clone(),
                result: Ok(imported.clone()),
            }
        }
        Job::SaveNotes(actual_root, notes) => {
            assert_eq!(actual_root, &writer_root);
            let result = save_notes_to_path(&writer_path, notes);
            // This notification and Completion both follow the durable write.
            let _ = accepted.send("notes-save");
            Completion::NotesSaved(result)
        }
    });
    let mut app = detached_app();
    app.preferences_worker = worker;
    app.preferences_worker.save_settings(Default::default());
    let previous_error = app.preferences_worker.drain().unwrap_err().to_string();
    assert!(previous_error.contains("controlled retained settings failure"));
    app.poll_preferences_worker();
    assert!(!app.preferences_worker.busy());
    assert!(app.preferences_worker.settings_pending());
    assert_eq!(observed.try_recv().unwrap(), "settings-error");

    let notes_request = app.preferences_worker.load_notes(key, root.clone());
    let import_request = app.preferences_worker.import_notes(key, root.clone());
    app.code_review = Some(CodeReviewState {
        key,
        repo_root: root,
        label: "Controlled shutdown review".to_owned(),
        loading: false,
        branch: String::new(),
        files: Vec::new(),
        selected: 0,
        failed: false,
        target_panel: None,
        feedback: String::new(),
        feedback_sent: false,
        worktrees: Vec::new(),
        show_worktrees: false,
        worktree_error: None,
        notes: Default::default(),
        notes_loading: true,
        notes_ready: false,
        notes_error: Some("previous notes read failure".to_owned()),
        notes_request,
        import_request: Some(import_request),
        legacy_notes_available: true,
        importing_notes: true,
        show_all_notes: false,
        editing_note: None,
        note_target_picker: false,
    });

    // There is no UI pass between submitting either read and draining. An
    // import applied by the barrier itself schedules a second writer phase.
    let refused = app
        .drain_preferences_for_shutdown()
        .unwrap_err()
        .to_string();
    assert_eq!(refused, previous_error);
    let state = app.code_review.as_ref().unwrap();
    assert!(
        !state.notes_loading,
        "a refused barrier must release the notes spinner"
    );
    assert!(state.notes_ready && state.notes_error.is_none());
    assert!(!state.importing_notes && state.import_request.is_none());
    assert!(!state.legacy_notes_available);
    assert_eq!(
        state.notes, expected,
        "the repeated imported note must not be duplicated"
    );
    assert!(
        !app.preferences_worker.busy(),
        "the derived save needs its ACK before returning"
    );
    assert!(app.preferences_worker.settings_pending());
    assert!(app
        .preferences_worker
        .warning()
        .unwrap()
        .contains("controlled retained settings failure"));
    assert_eq!(std::fs::read(&path).unwrap(), expected_bytes);
    assert_eq!(
        observed.try_iter().collect::<Vec<_>>(),
        vec!["load", "import", "notes-save"],
        "the import must be applied and saved exactly once before closure is refused"
    );
    app.poll_preferences_worker();
    assert!(
        observed.try_recv().is_err(),
        "polling again must not repeat the derived save"
    );
    assert!(!app.preferences_worker.busy());
}
