//! Exercise review handlers and real runtime receipts with a sink PTY.
//! No native terminal, provider, user profile, or filesystem writer is started.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use super::{PendingReviewDelivery, ReviewDeliveryContent, TerminalApp};
use crate::app::preferences_ui_tests::{detached_app, review_state};
use crate::app::preferences_worker::{Completion, Job, PreferencesWorker};
use crate::orchestration::DiffNotes;
use crate::terminal::panel::TerminalPanel;
use crate::terminal::pty::PtyHandle;

fn fixture() -> (TerminalApp, Uuid, Uuid, Arc<Mutex<PtyHandle>>) {
    let mut app = detached_app();
    app.preferences_worker = PreferencesWorker::with_processor_for_tests(|job| match job {
        Job::SaveNotes(_, _) => Completion::NotesSaved(Ok(())),
        _ => panic!("unexpected preference job in prompt delivery fixture"),
    });
    let manager = app.workspaces[0].pty_manager();
    manager
        .lock()
        .unwrap()
        .set_remote_spawner(Box::new(|_, _, _, _, wanted| {
            Ok((
                wanted.unwrap_or_else(Uuid::new_v4),
                PtyHandle::for_persistence_tests(b""),
            ))
        }));
    let mut panel = TerminalPanel::new(
        egui::pos2(0.0, 0.0),
        egui::vec2(400.0, 240.0),
        egui::Color32::WHITE,
        0,
    );
    panel.attach_session_with_spec(
        Arc::clone(&manager),
        None,
        crate::runtime::SessionSpec::default(),
    );
    let panel_id = panel.id;
    let runtime_id = panel.runtime_session_id().unwrap();
    let handle = manager.lock().unwrap().handle(runtime_id).unwrap();
    app.workspaces[0].add_restored_terminal(panel);
    let mut notes = DiffNotes::default();
    notes.add("review.rs", None, 1, "original note");
    let mut review = review_state(
        Uuid::new_v4(),
        Uuid::new_v4(),
        PathBuf::from("prompt-delivery-fixture"),
        notes,
    );
    review.notes_ready = true;
    review.notes_loading = false;
    review.target_panel = Some(panel_id);
    review.feedback = "original feedback".to_owned();
    app.code_review = Some(review);
    (app, panel_id, runtime_id, handle)
}

fn defer_feedback(app: &mut TerminalApp, runtime_id: Uuid) {
    let text = app.code_review.as_ref().unwrap().feedback.clone();
    let receipt = app.workspaces[0]
        .pty_manager()
        .lock()
        .unwrap()
        .queue_prompt_tracked(runtime_id, &text)
        .unwrap();
    app.code_review.as_mut().unwrap().pending_delivery = Some(PendingReviewDelivery {
        receipt,
        content: ReviewDeliveryContent::Feedback(text),
    });
}

fn restored_fixture() -> (TerminalApp, Uuid, Uuid) {
    let (mut app, panel_id, _, handle) = fixture();
    let saved = app.workspaces[0].panels[0].to_saved();
    let manager = app.workspaces[0].pty_manager();
    app.workspaces[0].panels.clear();
    drop(handle);
    let panel = TerminalPanel::from_saved(
        saved,
        &egui::Context::default(),
        None,
        app.workspaces[0].id,
        Arc::clone(&manager),
    );
    let runtime_id = panel.runtime_session_id().unwrap();
    assert!(!manager.lock().unwrap().is_attached(runtime_id));
    app.workspaces[0].add_restored_terminal(panel);
    (app, panel_id, runtime_id)
}

#[test]
fn prompt_delivery_review_restored_terminal_defers_feedback_and_ready_send_does_not_spawn() {
    let (mut app, panel_id, runtime_id) = restored_fixture();
    let manager = app.workspaces[0].pty_manager();
    assert_eq!(
        app.workspaces[0]
            .submit_prompt_to_ready_panel(panel_id, "capture")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotConnected
    );
    assert!(!manager.lock().unwrap().is_attached(runtime_id));

    app.send_code_review_feedback();
    let review = app.code_review.as_ref().unwrap();
    assert_eq!(review.feedback, "original feedback");
    assert!(review.pending_delivery.is_some());
    let handle = manager.lock().unwrap().handle(runtime_id).unwrap();
    assert_eq!(handle.lock().unwrap().activity_revision(), 0);
    app.workspaces[0].flush_pending_inputs();
    app.poll_review_prompt_delivery();
    assert!(!app.code_review.as_ref().unwrap().feedback_sent);
    handle.lock().unwrap().mark_render_dirty();
    app.workspaces[0].flush_pending_inputs();
    app.poll_review_prompt_delivery();
    let review = app.code_review.as_ref().unwrap();
    assert!(review.feedback.is_empty());
    assert!(review.feedback_sent);
    assert_eq!(review.notes.pending().len(), 1);
    assert_eq!(handle.lock().unwrap().activity_revision(), 1);
}

#[test]
fn prompt_delivery_review_restored_terminal_acknowledges_notes_after_startup() {
    let (mut app, panel_id, runtime_id) = restored_fixture();
    app.send_pending_notes(panel_id);
    let review = app.code_review.as_ref().unwrap();
    assert!(review.pending_delivery.is_some());
    assert_eq!(review.notes.pending().len(), 1);
    assert!(!app.preferences_worker.busy());
    let handle = app.workspaces[0]
        .pty_manager()
        .lock()
        .unwrap()
        .handle(runtime_id)
        .unwrap();
    handle.lock().unwrap().mark_render_dirty();
    app.workspaces[0].flush_pending_inputs();
    app.poll_review_prompt_delivery();
    let review = app.code_review.as_ref().unwrap();
    assert!(review.notes.pending().is_empty());
    assert!(review.pending_delivery.is_none());
    assert_eq!(review.feedback, "original feedback");
    assert_eq!(handle.lock().unwrap().activity_revision(), 1);
}

#[test]
fn prompt_delivery_review_keeps_feedback_after_the_input_channel_closes() {
    let (mut app, _, _, handle) = fixture();
    handle.lock().unwrap().close_input_for_prompt_tests();
    app.send_code_review_feedback();
    let review = app.code_review.as_ref().unwrap();
    assert_eq!(review.feedback, "original feedback");
    assert!(!review.feedback_sent);
    assert!(review.pending_delivery.is_none());
}

#[test]
fn prompt_delivery_review_keeps_notes_unsent_on_submission_failure() {
    let (mut app, panel_id, _, handle) = fixture();
    handle.lock().unwrap().close_input_for_prompt_tests();
    app.send_pending_notes(panel_id);
    assert_eq!(app.code_review.as_ref().unwrap().notes.pending().len(), 1);
    assert!(
        !app.preferences_worker.busy(),
        "failed delivery must not persist sent notes"
    );
}

#[test]
fn prompt_delivery_review_handles_a_missing_target_without_clearing_the_draft() {
    let (mut app, _, _, _) = fixture();
    let missing = Uuid::new_v4();
    app.code_review.as_mut().unwrap().target_panel = Some(missing);
    app.send_code_review_feedback();
    app.send_pending_notes(missing);
    let review = app.code_review.as_ref().unwrap();
    assert_eq!(review.feedback, "original feedback");
    assert!(!review.feedback_sent);
    assert_eq!(review.notes.pending().len(), 1);
    assert!(!app.preferences_worker.busy());
}

#[test]
fn prompt_delivery_review_only_clears_feedback_after_the_receipt() {
    let (mut app, _, runtime_id, handle) = fixture();
    defer_feedback(&mut app, runtime_id);
    app.poll_review_prompt_delivery();
    app.send_code_review_feedback();
    assert_eq!(
        app.code_review.as_ref().unwrap().feedback,
        "original feedback"
    );
    assert_eq!(handle.lock().unwrap().activity_revision(), 0);
    handle.lock().unwrap().mark_render_dirty();
    app.workspaces[0].flush_pending_inputs();
    app.poll_review_prompt_delivery();
    let review = app.code_review.as_ref().unwrap();
    assert!(review.feedback.is_empty());
    assert!(review.feedback_sent);
    assert!(review.pending_delivery.is_none());
    assert_eq!(
        handle.lock().unwrap().activity_revision(),
        1,
        "no duplicate submission"
    );
}

#[test]
fn prompt_delivery_review_does_not_clear_a_newer_draft_after_a_delayed_receipt() {
    let (mut app, _, runtime_id, handle) = fixture();
    defer_feedback(&mut app, runtime_id);
    app.code_review.as_mut().unwrap().feedback = "newer user draft".to_owned();
    handle.lock().unwrap().mark_render_dirty();
    app.workspaces[0].flush_pending_inputs();
    app.poll_review_prompt_delivery();
    let review = app.code_review.as_ref().unwrap();
    assert_eq!(review.feedback, "newer user draft");
    assert!(!review.feedback_sent);
    assert!(review.pending_delivery.is_none());
}

#[test]
fn prompt_delivery_review_resolves_lost_sessions_without_losing_feedback() {
    let (mut app, _, runtime_id, _) = fixture();
    defer_feedback(&mut app, runtime_id);
    app.workspaces[0]
        .pty_manager()
        .lock()
        .unwrap()
        .close(runtime_id);
    app.poll_review_prompt_delivery();
    let review = app.code_review.as_ref().unwrap();
    assert_eq!(review.feedback, "original feedback");
    assert!(!review.feedback_sent);
    assert!(review.pending_delivery.is_none());
}

#[test]
fn prompt_delivery_review_acknowledges_only_the_exact_submitted_note_versions() {
    let (mut app, _, runtime_id, handle) = fixture();
    let review = app.code_review.as_mut().unwrap();
    let edited = review.notes.notes[0].id;
    let unchanged = review.notes.add("review.rs", None, 2, "unchanged note");
    let snapshots = review.notes.notes.clone();
    let receipt = app.workspaces[0]
        .pty_manager()
        .lock()
        .unwrap()
        .queue_prompt_tracked(runtime_id, "formatted note snapshots")
        .unwrap();
    let review = app.code_review.as_mut().unwrap();
    review.pending_delivery = Some(PendingReviewDelivery {
        receipt,
        content: ReviewDeliveryContent::Notes(snapshots),
    });
    review.notes.edit(edited, "edited during delivery");
    let newer = review
        .notes
        .add("review.rs", None, 3, "added during delivery");
    handle.lock().unwrap().mark_render_dirty();
    app.workspaces[0].flush_pending_inputs();
    app.poll_review_prompt_delivery();
    let review = app.code_review.as_ref().unwrap();
    assert!(review
        .notes
        .notes
        .iter()
        .find(|note| note.id == unchanged)
        .unwrap()
        .sent_at
        .is_some());
    assert!(review
        .notes
        .notes
        .iter()
        .find(|note| note.id == edited)
        .unwrap()
        .sent_at
        .is_none());
    assert!(review
        .notes
        .notes
        .iter()
        .find(|note| note.id == newer)
        .unwrap()
        .sent_at
        .is_none());
    assert_eq!(review.notes.pending().len(), 2);
}

#[test]
fn prompt_delivery_review_closing_the_view_cancels_unaccepted_input() {
    let (mut app, _, runtime_id, handle) = fixture();
    defer_feedback(&mut app, runtime_id);
    app.code_review = None;
    handle.lock().unwrap().mark_render_dirty();
    app.workspaces[0].flush_pending_inputs();
    assert_eq!(handle.lock().unwrap().activity_revision(), 0);
    assert!(!app.workspaces[0].has_pending_inputs());
}

#[test]
fn prompt_delivery_review_handles_duplicate_note_ids_without_acknowledging_edits() {
    let (mut app, _, runtime_id, handle) = fixture();
    let review = app.code_review.as_mut().unwrap();
    let mut duplicate = review.notes.notes[0].clone();
    duplicate.body = "second note with an imported duplicate id".to_owned();
    review.notes.notes.push(duplicate);
    let submitted = review.notes.notes.clone();
    let receipt = app.workspaces[0]
        .pty_manager()
        .lock()
        .unwrap()
        .queue_prompt_tracked(runtime_id, "formatted note snapshots")
        .unwrap();
    let review = app.code_review.as_mut().unwrap();
    review.pending_delivery = Some(PendingReviewDelivery {
        receipt,
        content: ReviewDeliveryContent::Notes(submitted),
    });
    review.notes.notes[0].body = "edited after submission".to_owned();
    handle.lock().unwrap().mark_render_dirty();
    app.workspaces[0].flush_pending_inputs();
    app.poll_review_prompt_delivery();
    let notes = &app.code_review.as_ref().unwrap().notes.notes;
    assert!(
        notes[0].sent_at.is_none(),
        "same id does not acknowledge an edited version"
    );
    assert!(notes[1].sent_at.is_some());
}
