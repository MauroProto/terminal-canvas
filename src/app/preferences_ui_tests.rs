//! Headless preferences UI regressions. The empty saved workspace starts no
//! terminal, and the injected writer only touches an explicit temporary file.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use egui::{vec2, CentralPanel};
use egui_kittest::{kittest::Queryable, Harness};
use uuid::Uuid;

use super::code_review_ui::{CodeReviewState, NoteAction, NoteEditState};
use super::preferences_worker::{Completion, Job, PreferencesWorker};
use super::TerminalApp;
use crate::config::AppConfig;
use crate::orchestration::{DiffLine, DiffLineKind, DiffNotes, FileDiff};
use crate::state::Workspace;

const DEADLINE: Duration = Duration::from_secs(3);
const SAVED_MESSAGE: &str = "Configuración guardada en config.toml";

#[derive(Debug)]
enum AcceptedJob {
    Load(Uuid, Uuid, PathBuf),
    Import(Uuid, Uuid, PathBuf),
    Notes(PathBuf, DiffNotes),
    Settings(AppConfig),
}

struct WriterControl {
    accepted: Receiver<AcceptedJob>,
    replies: Sender<Completion>,
}

impl WriterControl {
    fn respond(&self, completion: Completion) {
        assert!(
            self.replies.send(completion).is_ok(),
            "controlled writer stopped before its reply"
        );
    }
}

fn controlled_writer(config_path: Option<PathBuf>) -> (PreferencesWorker, WriterControl) {
    let (accepted, observed) = mpsc::channel();
    let (replies, resume) = mpsc::channel();
    let worker = PreferencesWorker::with_processor_for_tests(move |job| {
        let snapshot = match job {
            Job::LoadNotes(key, request, root) => AcceptedJob::Load(*key, *request, root.clone()),
            Job::ImportNotes(key, request, root) => {
                AcceptedJob::Import(*key, *request, root.clone())
            }
            Job::SaveNotes(root, notes) => AcceptedJob::Notes(root.clone(), notes.clone()),
            Job::SaveSettings(config) => AcceptedJob::Settings(config.clone()),
        };
        let _ = accepted.send(snapshot);
        // A failed assertion must not leave Drop waiting forever on this
        // fixture. Timeout/disconnect becomes a normal failed completion.
        let completion = resume.recv_timeout(DEADLINE).unwrap_or_else(|error| {
            let reason = format!("controlled writer was not released: {error}");
            match job {
                Job::LoadNotes(key, request, root) => Completion::NotesLoaded {
                    key: *key,
                    request: *request,
                    repo_root: root.clone(),
                    result: Err(anyhow::anyhow!(reason)),
                },
                Job::ImportNotes(key, request, root) => Completion::NotesImported {
                    key: *key,
                    request: *request,
                    repo_root: root.clone(),
                    result: Err(anyhow::anyhow!(reason)),
                },
                Job::SaveNotes(_, _) => Completion::NotesSaved(Err(anyhow::anyhow!(reason))),
                Job::SaveSettings(_) => Completion::SettingsSaved(Err(anyhow::anyhow!(reason))),
            }
        });
        if let (Job::SaveSettings(config), Completion::SettingsSaved(Ok(())), Some(path)) =
            (job, &completion, &config_path)
        {
            return Completion::SettingsSaved(crate::config::save_to_path(config, path));
        }
        completion
    });
    (
        worker,
        WriterControl {
            accepted: observed,
            replies,
        },
    )
}

fn detached_app() -> TerminalApp {
    let ctx = egui::Context::default();
    let workspace = Workspace::new("Preferences regression", None);
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

fn preferences_harness(app: TerminalApp) -> Harness<'static, TerminalApp> {
    Harness::builder()
        .with_size(vec2(1100.0, 760.0))
        .build_ui_state(
            |ui, app: &mut TerminalApp| {
                app.ctx = Some(ui.ctx().clone());
                app.poll_preferences_worker();
                app.show_preferences_warning(ui);
                CentralPanel::default().show(ui, |_| {});
                app.show_code_review(ui.ctx());
                app.show_settings(ui.ctx());
                app.show_toasts(ui.ctx());
            },
            app,
        )
}

fn next_job(harness: &mut Harness<'_, TerminalApp>, control: &WriterControl) -> AcceptedJob {
    let deadline = Instant::now() + DEADLINE;
    loop {
        harness.state_mut().poll_preferences_worker();
        match control.accepted.try_recv() {
            Ok(job) => return job,
            Err(TryRecvError::Disconnected) => panic!("controlled writer disconnected"),
            Err(TryRecvError::Empty) => {}
        }
        assert!(
            Instant::now() < deadline,
            "writer did not accept its next job; busy={}, warning={:?}",
            harness.state().preferences_worker.busy(),
            harness.state().preferences_worker.warning()
        );
        std::thread::yield_now();
    }
}

fn wait_for_idle(harness: &mut Harness<'_, TerminalApp>) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        harness.state_mut().poll_preferences_worker();
        if !harness.state().preferences_worker.busy() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "controlled writer did not finish"
        );
        std::thread::yield_now();
    }
    // Areas get a sizing pass; two finite frames also render the actual toast
    // and warning. Harness::run would wait for transient toasts to stop repainting.
    harness.run_steps(2);
}

fn painted_contains(output: &egui::FullOutput, needle: &str) -> bool {
    fn contains(shape: &egui::epaint::Shape, clip: egui::Rect, needle: &str) -> bool {
        match shape {
            egui::epaint::Shape::Text(text) => {
                text.galley.job.text.contains(needle)
                    && clip.intersects(shape.visual_bounding_rect())
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().any(|shape| contains(shape, clip, needle))
            }
            _ => false,
        }
    }
    output
        .shapes
        .iter()
        .any(|shape| contains(&shape.shape, shape.clip_rect, needle))
}

fn notes_with(body: &str) -> DiffNotes {
    let mut notes = DiffNotes::default();
    notes.add("review.rs", None, 1, body);
    notes
}

fn review_state(key: Uuid, request: Uuid, root: PathBuf, notes: DiffNotes) -> CodeReviewState {
    CodeReviewState {
        key,
        repo_root: root,
        label: "Controlled review".to_owned(),
        loading: false,
        branch: "fixture".to_owned(),
        files: vec![FileDiff {
            path: "review.rs".to_owned(),
            additions: 1,
            lines: vec![DiffLine {
                kind: DiffLineKind::Added,
                old_ln: None,
                new_ln: Some(1),
                text: "let unicode = \"á👩🏽‍💻\";".to_owned(),
            }],
            ..Default::default()
        }],
        selected: 0,
        failed: false,
        target_panel: None,
        feedback: String::new(),
        feedback_sent: false,
        worktrees: Vec::new(),
        show_worktrees: false,
        worktree_error: None,
        notes,
        notes_loading: true,
        notes_ready: false,
        notes_error: None,
        notes_request: request,
        import_request: None,
        legacy_notes_available: false,
        importing_notes: false,
        show_all_notes: false,
        editing_note: None,
        note_target_picker: false,
    }
}

struct TemporaryConfig(PathBuf);

impl TemporaryConfig {
    fn new() -> Self {
        Self(
            std::env::temp_dir().join(format!("terminal-canvas-preferences-ui-{}", Uuid::new_v4())),
        )
    }

    fn path(&self) -> PathBuf {
        self.0.join("config.toml")
    }
}

impl Drop for TemporaryConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn notes_read_failure_preserves_diff_and_blocks_mutations_until_real_retry_succeeds() {
    let root = PathBuf::from("controlled-repository-a");
    let key = Uuid::new_v4();
    let preserved = notes_with("Never replace this note with an empty failed read");
    let (worker, control) = controlled_writer(None);
    let mut app = detached_app();
    app.preferences_worker = worker;
    let request = app.preferences_worker.load_notes(key, root.clone());
    app.code_review = Some(review_state(key, request, root.clone(), preserved.clone()));
    let mut harness = preferences_harness(app);
    let AcceptedJob::Load(actual_key, actual_request, actual_root) =
        next_job(&mut harness, &control)
    else {
        panic!("expected the initial notes load");
    };
    assert_eq!(
        (actual_key, actual_request, &actual_root),
        (key, request, &root)
    );
    control.respond(Completion::NotesLoaded {
        key,
        request,
        repo_root: root.clone(),
        result: Err(anyhow::anyhow!("controlled notes read denied")),
    });
    wait_for_idle(&mut harness);
    let state = harness.state().code_review.as_ref().unwrap();
    assert!(!state.notes_loading && !state.notes_ready);
    assert!(state
        .notes_error
        .as_deref()
        .unwrap()
        .contains("controlled notes read denied"));
    assert_eq!(state.notes, preserved);
    assert!(
        painted_contains(harness.output(), "let unicode ="),
        "notes failure must leave the diff visible"
    );
    assert!(painted_contains(
        harness.output(),
        "controlled notes read denied"
    ));

    let app = harness.state_mut();
    let note_id = preserved.notes[0].id;
    app.apply_note_action(NoteAction::Create {
        file_path: "review.rs".to_owned(),
        line: 1,
        old_side: false,
    });
    assert!(app.code_review.as_ref().unwrap().editing_note.is_none());
    app.apply_note_action(NoteAction::Edit(note_id));
    assert!(app.code_review.as_ref().unwrap().editing_note.is_none());
    app.apply_note_action(NoteAction::Delete(note_id));
    let dirty_editor = NoteEditState {
        note_id: Some(note_id),
        file_path: "review.rs".to_owned(),
        line: 1,
        old_side: false,
        body: "This must not overwrite unread disk notes".to_owned(),
    };
    app.code_review.as_mut().unwrap().editing_note = Some(dirty_editor.clone());
    app.apply_note_action(NoteAction::SaveEditor);
    assert_eq!(app.code_review.as_ref().unwrap().notes, preserved);
    assert_eq!(
        app.code_review.as_ref().unwrap().editing_note,
        Some(dirty_editor)
    );
    assert!(
        !app.preferences_worker.busy(),
        "blocked mutations must not submit any save"
    );
    app.apply_note_action(NoteAction::CancelEditor);
    assert!(app.code_review.as_ref().unwrap().editing_note.is_none());

    harness.run_steps(2);
    harness.get_by_label("Reintentar lectura de notas").click();
    harness.step();
    let AcceptedJob::Load(_, retry_request, retry_root) = next_job(&mut harness, &control) else {
        panic!("retry must submit a notes read");
    };
    assert_ne!(retry_request, request);
    assert_eq!(retry_root, root);
    assert_eq!(
        harness.state().code_review.as_ref().unwrap().notes_request,
        retry_request
    );
    assert!(harness.state().code_review.as_ref().unwrap().notes_loading);
    harness.run_steps(2);
    assert!(
        painted_contains(harness.output(), "let unicode ="),
        "retrying notes must not hide the already loaded diff"
    );

    // Keep the retry in flight while a newer request for this same review is
    // accepted. Its key alone cannot authorize the older result.
    let current_request = harness
        .state_mut()
        .preferences_worker
        .load_notes(key, root.clone());
    harness
        .state_mut()
        .code_review
        .as_mut()
        .unwrap()
        .notes_request = current_request;
    control.respond(Completion::NotesLoaded {
        key,
        request: retry_request,
        repo_root: root.clone(),
        result: Ok((notes_with("stale same-review result"), true)),
    });
    let AcceptedJob::Load(_, actual_request, _) = next_job(&mut harness, &control) else {
        panic!("expected the newer notes read");
    };
    assert_eq!(actual_request, current_request);
    let state = harness.state().code_review.as_ref().unwrap();
    assert!(state.notes_loading && !state.notes_ready);
    assert_eq!(state.notes, preserved);
    assert!(state.notes_error.is_some());

    control.respond(Completion::NotesLoaded {
        key,
        request: current_request,
        repo_root: PathBuf::from("controlled-repository-b"),
        result: Ok((notes_with("wrong repository result"), true)),
    });
    wait_for_idle(&mut harness);
    let state = harness.state().code_review.as_ref().unwrap();
    assert!(state.notes_loading && !state.notes_ready);
    assert_eq!(state.notes, preserved);
    assert!(state.notes_error.is_some());

    let final_request = harness
        .state_mut()
        .preferences_worker
        .load_notes(key, root.clone());
    harness
        .state_mut()
        .code_review
        .as_mut()
        .unwrap()
        .notes_request = final_request;
    let AcceptedJob::Load(_, actual_request, _) = next_job(&mut harness, &control) else {
        panic!("expected the current notes read");
    };
    assert_eq!(actual_request, final_request);
    let recovered = notes_with("Recovered disk note");
    control.respond(Completion::NotesLoaded {
        key,
        request: final_request,
        repo_root: root.clone(),
        result: Ok((recovered.clone(), true)),
    });
    wait_for_idle(&mut harness);
    let state = harness.state().code_review.as_ref().unwrap();
    assert!(state.notes_ready && !state.notes_loading && state.notes_error.is_none());
    assert!(state.legacy_notes_available);
    assert_eq!(state.notes, recovered);

    // Import results have their own request identity and the same root guard.
    let old_import = harness
        .state_mut()
        .preferences_worker
        .import_notes(key, root.clone());
    {
        let state = harness.state_mut().code_review.as_mut().unwrap();
        state.import_request = Some(old_import);
        state.importing_notes = true;
    }
    let AcceptedJob::Import(import_key, actual_request, import_root) =
        next_job(&mut harness, &control)
    else {
        panic!("expected the first import");
    };
    assert_eq!(import_key, key);
    assert_eq!(import_root, root);
    assert_eq!(actual_request, old_import);
    let current_import = harness
        .state_mut()
        .preferences_worker
        .import_notes(key, root.clone());
    harness
        .state_mut()
        .code_review
        .as_mut()
        .unwrap()
        .import_request = Some(current_import);
    control.respond(Completion::NotesImported {
        key,
        request: old_import,
        repo_root: root.clone(),
        result: Ok(notes_with("stale import")),
    });
    let AcceptedJob::Import(_, actual_request, _) = next_job(&mut harness, &control) else {
        panic!("expected the current import");
    };
    assert_eq!(actual_request, current_import);
    assert!(
        harness
            .state()
            .code_review
            .as_ref()
            .unwrap()
            .importing_notes
    );
    assert_eq!(
        harness.state().code_review.as_ref().unwrap().notes,
        recovered
    );
    control.respond(Completion::NotesImported {
        key,
        request: current_import,
        repo_root: PathBuf::from("controlled-repository-b"),
        result: Ok(notes_with("wrong repository import")),
    });
    wait_for_idle(&mut harness);
    assert!(
        harness
            .state()
            .code_review
            .as_ref()
            .unwrap()
            .importing_notes
    );
    assert_eq!(
        harness.state().code_review.as_ref().unwrap().notes,
        recovered
    );
    let final_import = harness
        .state_mut()
        .preferences_worker
        .import_notes(key, root.clone());
    harness
        .state_mut()
        .code_review
        .as_mut()
        .unwrap()
        .import_request = Some(final_import);
    let AcceptedJob::Import(_, actual_request, _) = next_job(&mut harness, &control) else {
        panic!("expected a matching import");
    };
    assert_eq!(actual_request, final_import);
    let imported = notes_with("Matching imported note");
    control.respond(Completion::NotesImported {
        key,
        request: final_import,
        repo_root: root.clone(),
        result: Ok(imported.clone()),
    });
    let AcceptedJob::Notes(saved_root, saved_notes) = next_job(&mut harness, &control) else {
        panic!("a matching import must persist its merged notes");
    };
    assert_eq!(saved_root, root);
    assert_eq!(
        saved_notes.notes,
        vec![recovered.notes[0].clone(), imported.notes[0].clone()]
    );
    control.respond(Completion::NotesSaved(Ok(())));
    wait_for_idle(&mut harness);
    let state = harness.state().code_review.as_ref().unwrap();
    assert!(!state.importing_notes && state.import_request.is_none());
    assert!(!state.legacy_notes_available);

    let app = harness.state_mut();
    app.apply_note_action(NoteAction::Create {
        file_path: "review.rs".to_owned(),
        line: 1,
        old_side: false,
    });
    app.code_review
        .as_mut()
        .unwrap()
        .editing_note
        .as_mut()
        .unwrap()
        .body = "Allowed after a successful read".to_owned();
    app.apply_note_action(NoteAction::SaveEditor);
    let AcceptedJob::Notes(saved_root, saved_notes) = next_job(&mut harness, &control) else {
        panic!("a ready review must save its edited notes");
    };
    assert_eq!(saved_root, root);
    assert_eq!(saved_notes.notes.len(), 3);
    assert_eq!(saved_notes.notes[0], recovered.notes[0]);
    assert_eq!(saved_notes.notes[1], imported.notes[0]);
    assert_eq!(saved_notes.notes[2].body, "Allowed after a successful read");
    control.respond(Completion::NotesSaved(Ok(())));
    wait_for_idle(&mut harness);
    assert!(harness
        .state()
        .code_review
        .as_ref()
        .unwrap()
        .editing_note
        .is_none());
    assert!(harness.state().preferences_worker.warning().is_none());
}

#[test]
fn closed_settings_keeps_a_persistent_warning_and_retries_the_latest_snapshot() {
    let temporary = TemporaryConfig::new();
    let path = temporary.path();
    let old = AppConfig {
        font_size: 12.0,
        linear_token: Some("old-token".to_owned()),
        ..Default::default()
    };
    let latest = AppConfig {
        font_size: 18.0,
        scrollback_lines: 42_000,
        allow_osc52: true,
        audio_bell: true,
        copy_on_select: true,
        agent_notifications: false,
        shell: Some("controlled-shell".to_owned()),
        linear_token: Some("retained-token-á".to_owned()),
        onboarding_dismissed: true,
    };
    let (worker, control) = controlled_writer(Some(path.clone()));
    let mut app = detached_app();
    app.preferences_worker = worker;
    app.settings_open = false;
    app.settings_draft = None;
    let mut harness = preferences_harness(app);
    harness
        .state_mut()
        .preferences_worker
        .save_settings(old.clone());
    let AcceptedJob::Settings(accepted) = next_job(&mut harness, &control) else {
        panic!("expected the old settings save");
    };
    assert_eq!(accepted, old);
    harness
        .state_mut()
        .preferences_worker
        .save_settings(latest.clone());
    control.respond(Completion::SettingsSaved(Err(anyhow::anyhow!(
        "controlled disk full"
    ))));
    let AcceptedJob::Settings(accepted) = next_job(&mut harness, &control) else {
        panic!("expected the latest settings save");
    };
    assert_eq!(accepted, latest);
    control.respond(Completion::SettingsSaved(Err(anyhow::anyhow!(
        "controlled disk full"
    ))));
    wait_for_idle(&mut harness);
    assert!(harness.state().preferences_worker.settings_pending());
    assert!(harness
        .state()
        .preferences_worker
        .warning()
        .unwrap()
        .contains("controlled disk full"));
    assert!(!harness.state().settings_open && harness.state().settings_draft.is_none());
    assert!(
        !path.exists(),
        "failed writes must not create a config file"
    );

    // The warning survives dismissal/expiry of transient error feedback.
    harness.state_mut().toasts = Default::default();
    harness.run_steps(2);
    assert!(harness.state().toasts.is_empty());
    assert!(painted_contains(
        harness.output(),
        "El guardado de preferencias requiere atención"
    ));
    assert!(painted_contains(harness.output(), "controlled disk full"));
    assert!(!painted_contains(harness.output(), SAVED_MESSAGE));
    harness.get_by_label("Reintentar guardado").click();
    harness.step();
    let AcceptedJob::Settings(accepted) = next_job(&mut harness, &control) else {
        panic!("the real retry button must reuse the retained settings snapshot");
    };
    assert_eq!(accepted, latest);
    assert!(harness.state().preferences_worker.busy());
    assert!(harness.state().preferences_worker.settings_pending());
    assert!(!path.exists());
    harness.run_steps(2);
    assert!(
        !painted_contains(harness.output(), SAVED_MESSAGE),
        "submission is not a durable ACK"
    );
    control.respond(Completion::SettingsSaved(Ok(())));
    wait_for_idle(&mut harness);
    assert_eq!(crate::config::load_from_path(&path), latest);
    assert!(harness.state().preferences_worker.warning().is_none());
    assert!(!harness.state().preferences_worker.settings_pending());
    assert!(painted_contains(harness.output(), SAVED_MESSAGE));
    assert!(!painted_contains(
        harness.output(),
        "El guardado de preferencias requiere atención"
    ));
    assert!(!harness.state().settings_open && harness.state().settings_draft.is_none());
}

#[test]
fn an_older_settings_ack_does_not_announce_success_while_the_latest_save_is_pending() {
    let temporary = TemporaryConfig::new();
    let path = temporary.path();
    let old = AppConfig {
        font_size: 13.0,
        ..Default::default()
    };
    let skipped = AppConfig {
        font_size: 15.0,
        ..Default::default()
    };
    let latest = AppConfig {
        font_size: 19.0,
        linear_token: Some("latest-token".to_owned()),
        onboarding_dismissed: true,
        ..Default::default()
    };
    let (worker, control) = controlled_writer(Some(path.clone()));
    let mut app = detached_app();
    app.preferences_worker = worker;
    let mut harness = preferences_harness(app);
    harness
        .state_mut()
        .preferences_worker
        .save_settings(old.clone());
    let AcceptedJob::Settings(accepted) = next_job(&mut harness, &control) else {
        panic!("expected the first accepted save");
    };
    assert_eq!(accepted, old);
    harness
        .state_mut()
        .preferences_worker
        .save_settings(skipped);
    harness
        .state_mut()
        .preferences_worker
        .save_settings(latest.clone());
    control.respond(Completion::SettingsSaved(Ok(())));
    let AcceptedJob::Settings(accepted) = next_job(&mut harness, &control) else {
        panic!("expected the coalesced latest save");
    };
    assert_eq!(accepted, latest);
    assert_eq!(crate::config::load_from_path(&path), old);
    assert!(harness.state().preferences_worker.busy());
    assert!(harness.state().preferences_worker.settings_pending());
    assert!(
        harness.state().toasts.is_empty(),
        "the earlier ACK cannot confirm the newer snapshot"
    );
    harness.run_steps(2);
    assert!(!painted_contains(harness.output(), SAVED_MESSAGE));
    control.respond(Completion::SettingsSaved(Ok(())));
    wait_for_idle(&mut harness);
    assert_eq!(crate::config::load_from_path(&path), latest);
    assert!(!harness.state().preferences_worker.settings_pending());
    assert!(painted_contains(harness.output(), SAVED_MESSAGE));
    assert!(
        control.accepted.try_recv().is_err(),
        "superseded pending settings must not be replayed"
    );
}

#[test]
fn explicit_retained_notes_recovery_after_reopen_restores_latest_ram_and_saves_reduction() {
    let root = PathBuf::from("retained-repository-a");
    let other_root = PathBuf::from("retained-repository-b");
    let old = notes_with("Original pending payload that is too long for the small fake policy");
    let mut latest = notes_with(
        "Latest pending edit that must be reduced before the small fake policy can save it",
    );
    latest.notes[0].old_side = true;
    latest.notes[0].start_line = Some(1);
    latest.notes[0].review_identity = Some("retained-review-identity".to_owned());
    latest.notes[0].sent_at = Some(chrono::Utc::now());
    latest.add("outside-current-diff.rs", None, 9, "pending imported note");
    let other = notes_with("Repository B keeps an independent write error");
    let (worker, control) = controlled_writer(None);
    let mut app = detached_app();
    app.preferences_worker = worker;
    let mut state = review_state(Uuid::new_v4(), Uuid::new_v4(), root.clone(), old.clone());
    state.notes_ready = true;
    state.notes_loading = false;
    app.code_review = Some(state);
    app.preferences_worker.save_notes(root.clone(), old.clone());
    let mut harness = preferences_harness(app);
    let AcceptedJob::Notes(saved_root, value) = next_job(&mut harness, &control) else {
        panic!("expected the original notes save");
    };
    assert_eq!(saved_root, root);
    assert_eq!(value, old);
    control.respond(Completion::NotesSaved(Err(anyhow::anyhow!(
        "synthetic permanent notes size limit"
    ))));
    wait_for_idle(&mut harness);

    harness.state_mut().code_review.as_mut().unwrap().notes = latest.clone();
    harness
        .state_mut()
        .preferences_worker
        .save_notes(root.clone(), latest.clone());
    let AcceptedJob::Notes(saved_root, value) = next_job(&mut harness, &control) else {
        panic!("expected the latest notes save");
    };
    assert_eq!(saved_root, root);
    assert_eq!(value, latest);
    assert!(
        value
            .notes
            .iter()
            .map(|note| note.body.len())
            .sum::<usize>()
            > 48
    );
    control.respond(Completion::NotesSaved(Err(anyhow::anyhow!(
        "synthetic permanent notes size limit"
    ))));
    wait_for_idle(&mut harness);
    harness
        .state_mut()
        .preferences_worker
        .save_notes(other_root.clone(), other.clone());
    let AcceptedJob::Notes(saved_root, value) = next_job(&mut harness, &control) else {
        panic!("expected repository B's save");
    };
    assert_eq!(saved_root, other_root);
    assert_eq!(value, other);
    control.respond(Completion::NotesSaved(Err(anyhow::anyhow!(
        "repository B remains read-only"
    ))));
    wait_for_idle(&mut harness);

    // Closing removes the view, not the writer's retained accepted payload.
    harness.state_mut().code_review = None;
    let key = Uuid::new_v4();
    let request = harness
        .state_mut()
        .preferences_worker
        .load_notes(key, root.clone());
    harness.state_mut().code_review = Some(review_state(
        key,
        request,
        root.clone(),
        DiffNotes::default(),
    ));
    let AcceptedJob::Notes(saved_root, value) = next_job(&mut harness, &control) else {
        panic!("ordinary reopening must retry the retained write before reading disk");
    };
    assert_eq!(saved_root, root);
    assert_eq!(value, latest);
    control.respond(Completion::NotesSaved(Err(anyhow::anyhow!(
        "synthetic permanent notes size limit"
    ))));
    wait_for_idle(&mut harness);
    let state = harness.state().code_review.as_ref().unwrap();
    assert!(!state.notes_ready && !state.notes_loading && state.notes_error.is_some());
    assert_eq!(state.notes, DiffNotes::default());
    assert!(matches!(
        control.accepted.try_recv(),
        Err(TryRecvError::Empty)
    ));
    let warning = harness.state().preferences_worker.warning();
    assert!(warning
        .as_deref()
        .unwrap()
        .contains("synthetic permanent notes size limit"));
    {
        let state = harness.state_mut().code_review.as_mut().unwrap();
        state.import_request = Some(Uuid::new_v4());
        state.importing_notes = true;
    }
    harness.run_steps(2);
    harness.get_by_label("Editar notas pendientes").click();
    harness.step();
    let state = harness.state().code_review.as_ref().unwrap();
    assert_eq!(state.notes, latest);
    assert!(state.notes_ready && state.show_all_notes && !state.notes_loading);
    assert!(
        state.notes_error.is_none() && state.import_request.is_none() && !state.importing_notes
    );
    assert_ne!(state.notes_request, request);
    assert_eq!(harness.state().preferences_worker.warning(), warning);
    assert!(!harness.state().preferences_worker.busy());
    assert!(
        matches!(control.accepted.try_recv(), Err(TryRecvError::Empty)),
        "recovery must not submit a save or retry"
    );

    let id = latest.notes[0].id;
    harness.state_mut().apply_note_action(NoteAction::Edit(id));
    harness
        .state_mut()
        .code_review
        .as_mut()
        .unwrap()
        .editing_note
        .as_mut()
        .unwrap()
        .body = "small".to_owned();
    harness
        .state_mut()
        .apply_note_action(NoteAction::SaveEditor);
    let AcceptedJob::Notes(saved_root, value) = next_job(&mut harness, &control) else {
        panic!("the explicit reduction must submit the corrected notes");
    };
    assert_eq!(saved_root, root);
    assert_eq!(value.notes[0].id, id);
    assert_eq!(value.notes[0].body, "small");
    assert!(value.notes[0].sent_at.is_none());
    assert_eq!(value.notes[1], latest.notes[1]);
    assert!(
        value
            .notes
            .iter()
            .map(|note| note.body.len())
            .sum::<usize>()
            <= 48
    );
    assert!(
        harness.state().preferences_worker.warning().is_some(),
        "submission is not an ACK"
    );
    control.respond(Completion::NotesSaved(Ok(())));
    wait_for_idle(&mut harness);
    assert!(harness
        .state()
        .preferences_worker
        .retained_notes_for_repository(&root)
        .is_none());
    assert_eq!(
        harness
            .state()
            .preferences_worker
            .retained_notes_for_repository(&other_root),
        Some(&other)
    );
    let remaining = harness.state().preferences_worker.warning().unwrap();
    assert!(remaining.contains("retained-repository-b"));
    assert!(!remaining.contains("retained-repository-a"));
}

#[test]
fn explicit_retained_notes_recovery_ignores_stale_load_and_import_completions() {
    for successful_old_read in [false, true] {
        let root = PathBuf::from("retained-stale-repository");
        let key = Uuid::new_v4();
        let retained = notes_with("exact accepted RAM payload");
        let (worker, control) = controlled_writer(None);
        let mut app = detached_app();
        app.preferences_worker = worker;
        let old_request = app.preferences_worker.load_notes(key, root.clone());
        app.preferences_worker
            .save_notes(root.clone(), retained.clone());
        let old_import = app.preferences_worker.import_notes(key, root.clone());
        let mut state = review_state(key, old_request, root.clone(), DiffNotes::default());
        // Stage an older in-flight request while this view still records its
        // preceding read error; the explicit handler must invalidate both IDs.
        state.notes_loading = false;
        state.notes_error = Some("preceding controlled notes read failed".to_owned());
        state.import_request = Some(old_import);
        state.importing_notes = true;
        app.code_review = Some(state);
        let mut harness = preferences_harness(app);
        let AcceptedJob::Load(actual_key, actual_request, actual_root) =
            next_job(&mut harness, &control)
        else {
            panic!("expected the old in-flight read");
        };
        assert_eq!(
            (actual_key, actual_request, actual_root),
            (key, old_request, root.clone())
        );
        assert!(harness
            .state_mut()
            .recover_retained_review_notes(key, &root));
        let current_request = harness.state().code_review.as_ref().unwrap().notes_request;
        assert_ne!(current_request, old_request);
        control.respond(Completion::NotesLoaded {
            key,
            request: old_request,
            repo_root: root.clone(),
            result: if successful_old_read {
                Ok((notes_with("stale disk payload"), true))
            } else {
                Err(anyhow::anyhow!("stale read error"))
            },
        });
        let AcceptedJob::Notes(saved_root, value) = next_job(&mut harness, &control) else {
            panic!("expected the already accepted pending save");
        };
        assert_eq!(saved_root, root);
        assert_eq!(value, retained);
        let state = harness.state().code_review.as_ref().unwrap();
        assert_eq!(state.notes, retained);
        assert!(state.notes_ready && state.notes_error.is_none());
        assert_eq!(state.notes_request, current_request);
        control.respond(Completion::NotesSaved(Err(anyhow::anyhow!(
            "synthetic permanent notes size limit"
        ))));
        let AcceptedJob::Import(actual_key, request, actual_root) =
            next_job(&mut harness, &control)
        else {
            panic!("expected the old queued import");
        };
        assert_eq!(
            (actual_key, request, actual_root),
            (key, old_import, root.clone())
        );
        control.respond(Completion::NotesImported {
            key,
            request: old_import,
            repo_root: root.clone(),
            result: Ok(notes_with("stale imported payload")),
        });
        wait_for_idle(&mut harness);
        let state = harness.state().code_review.as_ref().unwrap();
        assert_eq!(state.notes, retained);
        assert!(state.notes_ready && state.notes_error.is_none());
        assert_eq!(state.notes_request, current_request);
        assert!(state.import_request.is_none() && !state.importing_notes);
        assert!(harness.state().preferences_worker.warning().is_some());
        assert!(matches!(
            control.accepted.try_recv(),
            Err(TryRecvError::Empty)
        ));
    }
}

#[test]
fn retained_notes_recovery_requires_failed_matching_review_and_same_root_snapshot() {
    let root = PathBuf::from("retained-guard-repository");
    let other = PathBuf::from("retained-other-repository");
    let key = Uuid::new_v4();
    let (worker, control) = controlled_writer(None);
    let mut app = detached_app();
    app.preferences_worker = worker;
    let mut state = review_state(key, Uuid::new_v4(), root.clone(), DiffNotes::default());
    state.notes_loading = false;
    state.notes_error = Some("corrupt disk notes without any accepted save".to_owned());
    app.code_review = Some(state);
    let mut harness = preferences_harness(app);
    assert!(!harness
        .state_mut()
        .recover_retained_review_notes(key, &root));
    harness.run_steps(2);
    assert!(!painted_contains(
        harness.output(),
        "Editar notas pendientes"
    ));
    assert!(matches!(
        control.accepted.try_recv(),
        Err(TryRecvError::Empty)
    ));

    harness
        .state_mut()
        .preferences_worker
        .save_notes(other.clone(), notes_with("other root"));
    let AcceptedJob::Notes(saved_root, _) = next_job(&mut harness, &control) else {
        panic!("expected the other repository save");
    };
    assert_eq!(saved_root, other);
    assert!(!harness
        .state_mut()
        .recover_retained_review_notes(key, &root));
    harness
        .state_mut()
        .preferences_worker
        .save_notes(root.clone(), notes_with("accepted same root"));
    assert!(!harness
        .state_mut()
        .recover_retained_review_notes(Uuid::new_v4(), &root));
    assert!(!harness
        .state_mut()
        .recover_retained_review_notes(key, &other));
    harness
        .state_mut()
        .code_review
        .as_mut()
        .unwrap()
        .notes_ready = true;
    assert!(!harness
        .state_mut()
        .recover_retained_review_notes(key, &root));
    {
        let state = harness.state_mut().code_review.as_mut().unwrap();
        state.notes_ready = false;
        state.notes_loading = true;
    }
    assert!(!harness
        .state_mut()
        .recover_retained_review_notes(key, &root));
    {
        let state = harness.state_mut().code_review.as_mut().unwrap();
        state.notes_loading = false;
        state.notes_error = None;
    }
    assert!(!harness
        .state_mut()
        .recover_retained_review_notes(key, &root));
    assert_eq!(
        harness.state().code_review.as_ref().unwrap().notes,
        DiffNotes::default()
    );
    control.respond(Completion::NotesSaved(Err(anyhow::anyhow!(
        "other root denied"
    ))));
    let AcceptedJob::Notes(saved_root, _) = next_job(&mut harness, &control) else {
        panic!("expected the accepted same-root pending save");
    };
    assert_eq!(saved_root, root);
    control.respond(Completion::NotesSaved(Err(anyhow::anyhow!(
        "synthetic permanent notes size limit"
    ))));
    wait_for_idle(&mut harness);
}
