use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(3);

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("tc-preferences-recovery-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct ReleaseOnDrop(Option<Sender<()>>);

impl ReleaseOnDrop {
    fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.release();
    }
}

fn settings(font_size: f32) -> crate::config::AppConfig {
    crate::config::AppConfig {
        font_size,
        ..Default::default()
    }
}

fn notes(body: &str) -> DiffNotes {
    let mut notes = DiffNotes::default();
    notes.add("synthetic.rs", None, 1, body);
    notes
}

fn read_notes(path: &std::path::Path) -> anyhow::Result<DiffNotes> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn poll_until(
    worker: &mut PreferencesWorker,
    done: impl Fn(&PreferencesWorker) -> bool,
) -> Vec<Completion> {
    let deadline = Instant::now() + DEADLINE;
    let mut completions = Vec::new();
    while !done(worker) {
        completions.extend(worker.poll());
        assert!(
            done(worker) || Instant::now() < deadline,
            "preference worker did not reach the expected state within three seconds"
        );
        thread::yield_now();
    }
    completions
}

// Synthetic channels have no I/O processor. Remove every runnable job before
// asserting so even a failed assertion cannot leave Drop awaiting a fake ACK.
fn retire_synthetic(worker: &mut PreferencesWorker) {
    worker.active = None;
    worker.failed_active = None;
    worker.pending.clear();
    worker.retire_worker();
}

#[test]
fn lazy_spawn_failure_retains_snapshot_until_explicit_durable_retry() {
    let directory = TemporaryDirectory::new();
    let config_path = directory.path("config.toml");
    let destination = config_path.clone();
    let starts = Arc::new(AtomicUsize::new(0));
    let start_count = Arc::clone(&starts);
    let writes = Arc::new(AtomicUsize::new(0));
    let write_count = Arc::clone(&writes);
    let mut worker = PreferencesWorker::with_spawner(
        move |job| match job {
            Job::SaveSettings(config) => {
                write_count.fetch_add(1, Ordering::SeqCst);
                Completion::SettingsSaved(crate::config::save_to_path(config, &destination))
            }
            _ => panic!("fixture only accepts settings"),
        },
        move |task| {
            if start_count.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(std::io::Error::other("synthetic spawn failure"))
            } else {
                spawn(task)
            }
        },
    );
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert!(!worker.busy());
    worker.save_settings(settings(18.5));
    let retained = Arc::clone(worker.pending.front().unwrap());
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(!worker.busy());
    assert!(worker.settings_pending());
    assert!(worker
        .warning()
        .unwrap()
        .contains("synthetic spawn failure"));
    assert!(!config_path.exists());
    assert!(worker.drain().is_err());
    let interrupted = worker.poll();
    assert!(matches!(
        interrupted.as_slice(),
        [Completion::SettingsSaved(Err(_))]
    ));
    for _ in 0..8 {
        assert!(worker.poll().is_empty());
    }
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    assert!(Arc::ptr_eq(worker.pending.front().unwrap(), &retained));
    worker.retry();
    assert!(Arc::ptr_eq(worker.active.as_ref().unwrap(), &retained));
    assert!(worker.drain().is_ok());
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert_eq!(crate::config::load_from_path(&config_path).font_size, 18.5);
    assert!(!worker.settings_pending());
    assert!(worker.warning().is_none());
    assert!(matches!(
        worker.poll().as_slice(),
        [Completion::SettingsSaved(Ok(()))]
    ));
}

#[test]
fn disconnected_send_retains_arc_and_recovers_same_settings_snapshot() {
    let directory = TemporaryDirectory::new();
    let config_path = directory.path("config.toml");
    let destination = config_path.clone();
    let mut worker = PreferencesWorker::with_processor(move |job| match job {
        Job::SaveSettings(config) => {
            Completion::SettingsSaved(crate::config::save_to_path(config, &destination))
        }
        _ => panic!("fixture only accepts settings"),
    });
    let (jobs, incoming) = mpsc::sync_channel(1);
    drop(incoming);
    let (_completed, results) = mpsc::channel();
    worker.worker = Some(StartedWorker {
        jobs,
        results,
        thread: thread::spawn(|| {}),
    });
    let retained = Arc::new(Job::SaveSettings(settings(19.0)));
    worker.pending.push_back(Arc::clone(&retained));
    worker.schedule();
    assert!(worker.worker.is_none());
    assert!(worker.active.is_none());
    assert!(worker.failed_active.is_none());
    assert_eq!(worker.pending.len(), 1);
    assert!(Arc::ptr_eq(worker.pending.front().unwrap(), &retained));
    assert!(!worker.busy());
    assert!(worker.settings_pending());
    assert!(!config_path.exists());
    assert!(matches!(
        worker.poll().as_slice(),
        [Completion::SettingsSaved(Err(_))]
    ));
    worker.retry();
    assert!(Arc::ptr_eq(worker.active.as_ref().unwrap(), &retained));
    assert!(worker.drain().is_ok());
    assert_eq!(crate::config::load_from_path(&config_path).font_size, 19.0);
    assert!(worker.warning().is_none());
}

#[test]
fn full_channel_preserves_pending_and_failed_active_arc_identity() {
    let mut worker =
        PreferencesWorker::with_processor(|_| panic!("synthetic channel has no reader"));
    let (jobs, incoming) = mpsc::sync_channel(1);
    let occupied = Arc::new(Job::SaveSettings(settings(12.0)));
    assert!(jobs.try_send(Arc::clone(&occupied)).is_ok());
    let (_completed, results) = mpsc::channel();
    worker.worker = Some(StartedWorker {
        jobs,
        results,
        thread: thread::spawn(|| {}),
    });
    let retained = Arc::new(Job::SaveSettings(settings(20.0)));
    worker.pending.push_back(Arc::clone(&retained));
    worker.schedule();
    let pending_identity = worker
        .pending
        .front()
        .is_some_and(|job| Arc::ptr_eq(job, &retained));
    let pending_count = worker.pending.len();
    let pending_active = worker.active.is_some();
    worker.pending.clear();
    worker.failed_active = Some(Arc::clone(&retained));
    worker.schedule();
    let failed_identity = worker
        .failed_active
        .as_ref()
        .is_some_and(|job| Arc::ptr_eq(job, &retained));
    let failed_queue_empty = worker.pending.is_empty();
    let failed_active = worker.active.is_some();
    let warning = worker.warning();
    let accepted = incoming.try_recv();
    retire_synthetic(&mut worker);
    assert!(pending_identity);
    assert_eq!(pending_count, 1);
    assert!(!pending_active);
    assert!(failed_identity);
    assert!(failed_queue_empty);
    assert!(!failed_active);
    assert!(warning.is_none());
    assert!(Arc::ptr_eq(&accepted.unwrap(), &occupied));
}

#[test]
fn panicked_active_save_retries_before_load_and_newer_save_without_poison_loop() {
    let directory = TemporaryDirectory::new();
    let root = directory.path("repository");
    let notes_path = directory.path("notes.json");
    let destination = notes_path.clone();
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = Arc::clone(&events);
    let (started, began) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let mut failed_once = false;
    let mut worker = PreferencesWorker::with_processor(move |job| match job {
        Job::SaveNotes(_, value) if !failed_once => {
            failed_once = true;
            observed
                .lock()
                .unwrap()
                .push(format!("interrupted:{}", value.notes[0].body));
            started.send(()).unwrap();
            released.recv_timeout(DEADLINE).unwrap();
            panic!("synthetic active write panic");
        }
        Job::SaveNotes(_, value) => {
            let result = crate::orchestration::save_notes_to_path(&destination, value);
            observed
                .lock()
                .unwrap()
                .push(format!("saved:{}", value.notes[0].body));
            Completion::NotesSaved(result)
        }
        Job::LoadNotes(key, request, repo_root) => {
            let result = read_notes(&destination);
            if let Ok(value) = &result {
                observed
                    .lock()
                    .unwrap()
                    .push(format!("loaded:{}", value.notes[0].body));
            }
            Completion::NotesLoaded {
                key: *key,
                request: *request,
                repo_root: repo_root.clone(),
                result: result.map(|value| (value, false)),
            }
        }
        _ => panic!("fixture only accepts notes writes and loads"),
    });
    let mut release = ReleaseOnDrop(Some(release));
    worker.save_notes(root.clone(), notes("A1"));
    began.recv_timeout(DEADLINE).unwrap();
    let retained = Arc::clone(worker.active.as_ref().unwrap());
    let key = Uuid::new_v4();
    let request = worker.load_notes(key, root.clone());
    worker.save_notes(root.clone(), notes("A2"));
    release.release();
    let interrupted = poll_until(&mut worker, |worker| worker.unavailable.is_some());
    assert!(!worker.busy());
    assert!(worker.processor.is_poisoned());
    assert!(Arc::ptr_eq(
        worker.failed_active.as_ref().unwrap(),
        &retained
    ));
    assert_eq!(worker.pending.len(), 2);
    assert!(
        matches!(worker.pending[0].as_ref(), Job::LoadNotes(actual_key, actual_request, actual_root)
        if *actual_key == key && *actual_request == request && actual_root == &root)
    );
    assert!(
        matches!(worker.pending[1].as_ref(), Job::SaveNotes(_, value) if value.notes[0].body == "A2")
    );
    assert!(interrupted.iter().any(|completion| matches!(completion,
        Completion::NotesLoaded { key: actual_key, request: actual_request, repo_root, result: Err(_) }
        if *actual_key == key && *actual_request == request && repo_root == &root)));
    worker.retry();
    assert!(Arc::ptr_eq(worker.active.as_ref().unwrap(), &retained));
    assert!(worker.drain().is_ok());
    let completed = worker.poll();
    assert_eq!(
        *events.lock().unwrap(),
        ["interrupted:A1", "saved:A1", "saved:A2"]
    );
    assert_eq!(read_notes(&notes_path).unwrap().notes[0].body, "A2");
    assert_eq!(completed.len(), 3);
    assert!(matches!(&completed[0], Completion::NotesSaved(Ok(()))));
    // A2 is still retained when this queued read reaches the head. Preserve
    // write order, but refuse an editable A1 snapshot instead of reading disk.
    assert!(matches!(&completed[1],
        Completion::NotesLoaded { key: actual_key, request: actual_request, repo_root, result: Err(_) }
        if *actual_key == key && *actual_request == request && repo_root == &root));
    assert!(matches!(&completed[2], Completion::NotesSaved(Ok(()))));
    assert!(worker.failed_active.is_none());
    assert!(!worker.busy());
    assert!(worker.warning().is_none());
    let latest_request = worker.load_notes(key, root.clone());
    assert_ne!(request, latest_request);
    assert!(worker.drain().is_ok());
    let loaded = worker.poll();
    assert!(matches!(loaded.as_slice(),
        [Completion::NotesLoaded { key: actual_key, request: actual_request, repo_root, result: Ok((value, false)) }]
        if *actual_key == key && *actual_request == latest_request && repo_root == &root && value.notes[0].body == "A2"));
    assert_eq!(
        *events.lock().unwrap(),
        ["interrupted:A1", "saved:A1", "saved:A2", "loaded:A2"]
    );
}

#[test]
fn empty_result_channel_keeps_active_writer_without_respawning() {
    let directory = TemporaryDirectory::new();
    let config_path = directory.path("config.toml");
    let destination = config_path.clone();
    let starts = Arc::new(AtomicUsize::new(0));
    let start_count = Arc::clone(&starts);
    let (started, began) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let mut worker = PreferencesWorker::with_spawner(
        move |job| match job {
            Job::SaveSettings(config) => {
                started.send(()).unwrap();
                released.recv_timeout(DEADLINE).unwrap();
                Completion::SettingsSaved(crate::config::save_to_path(config, &destination))
            }
            _ => panic!("fixture only accepts settings"),
        },
        move |task| {
            start_count.fetch_add(1, Ordering::SeqCst);
            spawn(task)
        },
    );
    let mut release = ReleaseOnDrop(Some(release));
    worker.save_settings(settings(21.0));
    began.recv_timeout(DEADLINE).unwrap();
    let retained = Arc::clone(worker.active.as_ref().unwrap());
    for _ in 0..16 {
        assert!(worker.poll().is_empty());
        assert!(worker.busy());
        assert!(worker.warning().is_none());
        assert!(Arc::ptr_eq(worker.active.as_ref().unwrap(), &retained));
    }
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(!config_path.exists());
    release.release();
    let completed = poll_until(&mut worker, |worker| !worker.busy());
    assert!(matches!(
        completed.as_slice(),
        [Completion::SettingsSaved(Ok(()))]
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(crate::config::load_from_path(&config_path).font_size, 21.0);
}

#[test]
fn acknowledged_save_before_disconnect_is_not_replayed() {
    let directory = TemporaryDirectory::new();
    let config_path = directory.path("config.toml");
    let destination = config_path.clone();
    let writes = Arc::new(AtomicUsize::new(0));
    let write_count = Arc::clone(&writes);
    let mut worker = PreferencesWorker::with_processor(move |job| match job {
        Job::SaveSettings(config) => {
            write_count.fetch_add(1, Ordering::SeqCst);
            Completion::SettingsSaved(crate::config::save_to_path(config, &destination))
        }
        _ => panic!("fixture only accepts settings"),
    });
    let (jobs, _incoming) = mpsc::sync_channel(1);
    let (completed, results) = mpsc::channel();
    assert!(completed.send(Completion::SettingsSaved(Ok(()))).is_ok());
    drop(completed);
    worker.worker = Some(StartedWorker {
        jobs,
        results,
        thread: thread::spawn(|| {}),
    });
    worker.active = Some(Arc::new(Job::SaveSettings(settings(16.0))));
    worker.settings_write_error = Some(FailedSave {
        job: Arc::new(Job::SaveSettings(settings(14.0))),
        reason: "older rejected snapshot".to_owned(),
    });
    let completed = worker.poll();
    let active_absent = worker.active.is_none();
    let failed_absent = worker.failed_active.is_none();
    let pending_empty = worker.pending.is_empty();
    let saved_error_absent = worker.settings_write_error.is_none();
    retire_synthetic(&mut worker);
    assert!(matches!(
        completed.as_slice(),
        [Completion::SettingsSaved(Ok(()))]
    ));
    assert!(active_absent);
    assert!(failed_absent);
    assert!(pending_empty);
    assert!(saved_error_absent);
    assert!(worker.poll().is_empty());
    worker.retry();
    assert!(!worker.busy());
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    worker.save_settings(settings(22.0));
    assert!(worker.drain().is_ok());
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert_eq!(crate::config::load_from_path(&config_path).font_size, 22.0);
}

#[test]
fn drain_retains_read_and_import_completions_after_settings_failure() {
    let directory = TemporaryDirectory::new();
    let root = directory.path("repository");
    let config_path = directory.path("config.toml");
    let destination = config_path.clone();
    let mut worker = PreferencesWorker::with_processor(move |job| match job {
        Job::LoadNotes(key, request, repo_root) => Completion::NotesLoaded {
            key: *key,
            request: *request,
            repo_root: repo_root.clone(),
            result: Ok((notes("loaded"), true)),
        },
        Job::ImportNotes(key, request, repo_root) => Completion::NotesImported {
            key: *key,
            request: *request,
            repo_root: repo_root.clone(),
            result: Ok(notes("imported")),
        },
        Job::SaveSettings(config) if config.font_size < 18.0 => {
            Completion::SettingsSaved(Err(anyhow::anyhow!("synthetic disk full")))
        }
        Job::SaveSettings(config) => {
            Completion::SettingsSaved(crate::config::save_to_path(config, &destination))
        }
        _ => panic!("fixture only accepts reads, import, and settings"),
    });
    let key = Uuid::new_v4();
    let load_request = worker.load_notes(key, root.clone());
    let import_request = worker.import_notes(key, root.clone());
    assert_ne!(load_request, import_request);
    worker.save_settings(settings(10.0));
    assert!(worker
        .drain()
        .unwrap_err()
        .to_string()
        .contains("synthetic disk full"));
    let completed = worker.poll();
    assert_eq!(completed.len(), 3);
    assert!(matches!(&completed[0], Completion::NotesLoaded {
        key: actual_key, request, repo_root, result: Ok((value, true))
    } if *actual_key == key && *request == load_request && repo_root == &root && value.notes[0].body == "loaded"));
    assert!(matches!(&completed[1], Completion::NotesImported {
        key: actual_key, request, repo_root, result: Ok(value)
    } if *actual_key == key && *request == import_request && repo_root == &root && value.notes[0].body == "imported"));
    assert!(matches!(&completed[2], Completion::SettingsSaved(Err(_))));
    assert!(worker.poll().is_empty());
    assert!(worker.settings_pending());
    assert!(!worker.busy());
    worker.save_settings(settings(23.0));
    assert!(worker.drain().is_ok());
    assert!(matches!(
        worker.poll().as_slice(),
        [Completion::SettingsSaved(Ok(()))]
    ));
    assert_eq!(crate::config::load_from_path(&config_path).font_size, 23.0);
    assert!(!worker.settings_pending());
}

#[test]
fn errors_clear_only_for_successful_destinations_and_retry_keeps_latest_snapshots() {
    let directory = TemporaryDirectory::new();
    let root_a = directory.path("repository-a");
    let root_b = directory.path("repository-b");
    let notes_a = directory.path("notes-a.json");
    let notes_b = directory.path("notes-b.json");
    let config_path = directory.path("config.toml");
    let failing_root = root_a.clone();
    let destination_a = notes_a.clone();
    let destination_b = notes_b.clone();
    let destination_config = config_path.clone();
    let writable_a = Arc::new(AtomicBool::new(false));
    let can_write_a = Arc::clone(&writable_a);
    let attempts = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = Arc::clone(&attempts);
    let mut worker = PreferencesWorker::with_processor(move |job| match job {
        Job::SaveNotes(root, value) if root == &failing_root => {
            observed
                .lock()
                .unwrap()
                .push(format!("A:{}", value.notes[0].body));
            Completion::NotesSaved(if can_write_a.load(Ordering::SeqCst) {
                crate::orchestration::save_notes_to_path(&destination_a, value)
            } else {
                Err(anyhow::anyhow!("repository A denied"))
            })
        }
        Job::SaveNotes(_, value) => Completion::NotesSaved(
            crate::orchestration::save_notes_to_path(&destination_b, value),
        ),
        Job::SaveSettings(config) => {
            observed
                .lock()
                .unwrap()
                .push(format!("settings:{}", config.font_size));
            Completion::SettingsSaved(if config.font_size < 18.0 {
                Err(anyhow::anyhow!("settings denied"))
            } else {
                crate::config::save_to_path(config, &destination_config)
            })
        }
        Job::LoadNotes(key, request, repo_root) => Completion::NotesLoaded {
            key: *key,
            request: *request,
            repo_root: repo_root.clone(),
            result: Ok((DiffNotes::default(), false)),
        },
        _ => panic!("fixture only accepts writes and loads"),
    });
    worker.save_notes(root_a.clone(), notes("old A"));
    worker.save_settings(settings(10.0));
    worker.save_notes(root_b.clone(), notes("B"));
    assert!(worker.drain().is_err());
    assert!(worker.notes_write_errors.contains_key(&root_a));
    assert!(!worker.notes_write_errors.contains_key(&root_b));
    assert!(worker.settings_write_error.is_some());
    assert_eq!(read_notes(&notes_b).unwrap().notes[0].body, "B");
    worker.load_notes(Uuid::new_v4(), root_b.clone());
    assert!(worker.drain().is_err());
    assert!(worker.notes_write_errors.contains_key(&root_a));
    assert!(worker.settings_write_error.is_some());
    writable_a.store(true, Ordering::SeqCst);
    worker.save_notes(root_a, notes("new A"));
    assert!(worker.drain().is_err());
    assert!(worker.notes_write_errors.is_empty());
    assert!(worker.settings_write_error.is_some());
    worker.save_settings(settings(24.0));
    // Retrying while the newer settings snapshot is already active must not
    // place the failed old snapshot behind it and overwrite the user's edit.
    worker.retry();
    assert!(worker.drain().is_ok());
    assert_eq!(read_notes(&notes_a).unwrap().notes[0].body, "new A");
    assert_eq!(crate::config::load_from_path(&config_path).font_size, 24.0);
    assert_eq!(
        *attempts.lock().unwrap(),
        ["A:old A", "settings:10", "A:new A", "settings:24"]
    );
    assert!(worker.notes_write_errors.is_empty());
    assert!(worker.settings_write_error.is_none());
    assert!(!worker.settings_pending());
    assert!(!worker.busy());
    assert!(worker.warning().is_none());
}

#[test]
fn reopened_notes_do_not_expose_disk_before_failed_snapshot_is_saved() {
    let directory = TemporaryDirectory::new();
    let root = directory.path("repository");
    let notes_path = directory.path("notes.json");
    crate::orchestration::save_notes_to_path(&notes_path, &notes("N0")).unwrap();
    let destination = notes_path.clone();
    let writable = Arc::new(AtomicBool::new(false));
    let can_write = Arc::clone(&writable);
    let attempts = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = Arc::clone(&attempts);
    let loads = Arc::new(AtomicUsize::new(0));
    let load_count = Arc::clone(&loads);
    let mut worker = PreferencesWorker::with_processor(move |job| match job {
        Job::SaveNotes(_, value) => {
            observed.lock().unwrap().push(value.notes[0].body.clone());
            Completion::NotesSaved(if can_write.load(Ordering::SeqCst) {
                crate::orchestration::save_notes_to_path(&destination, value)
            } else {
                Err(anyhow::anyhow!("synthetic notes permission denied"))
            })
        }
        Job::LoadNotes(key, request, repo_root) => {
            load_count.fetch_add(1, Ordering::SeqCst);
            Completion::NotesLoaded {
                key: *key,
                request: *request,
                repo_root: repo_root.clone(),
                result: read_notes(&destination).map(|value| (value, false)),
            }
        }
        _ => panic!("fixture only accepts notes writes and loads"),
    });
    worker.save_notes(root.clone(), notes("N1"));
    assert!(worker.drain().is_err());
    assert_eq!(read_notes(&notes_path).unwrap().notes[0].body, "N0");
    let key = Uuid::new_v4();
    let failed_request = worker.load_notes(key, root.clone());
    assert!(worker.drain().is_err());
    let refused = worker.poll();
    assert!(refused.iter().any(|completion| matches!(completion,
        Completion::NotesLoaded { key: actual_key, request, repo_root, result: Err(_) }
        if *actual_key == key && *request == failed_request && repo_root == &root)));
    assert!(!refused
        .iter()
        .any(|completion| matches!(completion, Completion::NotesLoaded { result: Ok(_), .. })));
    assert_eq!(loads.load(Ordering::SeqCst), 0);
    assert_eq!(read_notes(&notes_path).unwrap().notes[0].body, "N0");
    assert_eq!(*attempts.lock().unwrap(), ["N1", "N1"]);

    writable.store(true, Ordering::SeqCst);
    worker.retry();
    let repaired_request = worker.load_notes(key, root.clone());
    assert_ne!(failed_request, repaired_request);
    assert!(worker.drain().is_ok());
    let repaired = worker.poll();
    assert!(repaired.iter().any(|completion| matches!(completion,
        Completion::NotesLoaded { key: actual_key, request, repo_root, result: Ok((value, false)) }
        if *actual_key == key && *request == repaired_request && repo_root == &root && value.notes[0].body == "N1")));
    assert_eq!(read_notes(&notes_path).unwrap().notes[0].body, "N1");
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert!(worker.warning().is_none());

    // A retained old failure must not be retried behind a newer queued save.
    // Place N2 in the ordinary FIFO before requesting this repository again.
    writable.store(false, Ordering::SeqCst);
    worker.save_notes(root.clone(), notes("N1"));
    assert!(worker.drain().is_err());
    writable.store(true, Ordering::SeqCst);
    worker.queue(Arc::new(Job::SaveNotes(root.clone(), notes("N2"))));
    let latest_request = worker.load_notes(key, root.clone());
    assert_ne!(repaired_request, latest_request);
    assert!(worker.drain().is_ok());
    let latest = worker.poll();
    assert!(latest.iter().any(|completion| matches!(completion,
        Completion::NotesLoaded { key: actual_key, request, repo_root, result: Ok((value, false)) }
        if *actual_key == key && *request == latest_request && repo_root == &root && value.notes[0].body == "N2")));
    assert_eq!(*attempts.lock().unwrap(), ["N1", "N1", "N1", "N1", "N2"]);
    assert_eq!(read_notes(&notes_path).unwrap().notes[0].body, "N2");
    assert_eq!(loads.load(Ordering::SeqCst), 2);
    assert!(worker.notes_write_errors.is_empty());
    assert!(worker.warning().is_none());
    assert!(!worker.busy());
}

fn snapshot_only_worker() -> PreferencesWorker {
    PreferencesWorker::with_spawner(
        |job| job.interrupted("snapshot-only fixture"),
        |_| Err(std::io::ErrorKind::PermissionDenied.into()),
    )
}

fn notes_in_job(job: &Arc<Job>) -> &DiffNotes {
    let Job::SaveNotes(_, notes) = job.as_ref() else {
        panic!("fixture expected a notes save");
    };
    notes
}

#[test]
fn retained_notes_snapshot_prefers_the_last_pending_payload_without_side_effects() {
    let root = PathBuf::from("retained-a");
    let mut worker = snapshot_only_worker();
    let known = Arc::new(Job::SaveNotes(root.clone(), notes("known failed")));
    let failed = Arc::new(Job::SaveNotes(root.clone(), notes("failed active")));
    let active = Arc::new(Job::SaveNotes(root.clone(), notes("active")));
    let oldest_pending = Arc::new(Job::SaveNotes(root.clone(), notes("older pending")));
    let latest = Arc::new(Job::SaveNotes(root.clone(), notes("latest pending")));
    worker.notes_write_errors.insert(
        root.clone(),
        FailedSave {
            job: known,
            reason: "permanent synthetic cap".to_owned(),
        },
    );
    worker.failed_active = Some(failed);
    worker.active = Some(active);
    worker.pending.push_back(oldest_pending);
    worker.pending.push_back(Arc::clone(&latest));
    worker
        .pending
        .push_back(Arc::new(Job::SaveSettings(settings(16.0))));
    worker.pending.push_back(Arc::new(Job::SaveNotes(
        PathBuf::from("retained-b"),
        notes("other root"),
    )));
    let warning = worker.warning();
    let count = Arc::strong_count(&latest);
    for _ in 0..3 {
        let selected = worker.retained_notes_for_repository(&root).unwrap();
        assert!(std::ptr::eq(selected, notes_in_job(&latest)));
        assert_eq!(selected, notes_in_job(&latest));
    }
    assert_eq!(Arc::strong_count(&latest), count);
    assert_eq!(worker.pending.len(), 4);
    assert!(worker.active.is_some() && worker.failed_active.is_some());
    assert_eq!(worker.warning(), warning);
    assert!(worker.worker.is_none());
    retire_synthetic(&mut worker);
}

#[test]
fn retained_notes_snapshot_falls_back_from_active_to_failed_active_to_known_failure() {
    let root = PathBuf::from("retained-a");
    let mut worker = snapshot_only_worker();
    let known = Arc::new(Job::SaveNotes(root.clone(), notes("known failed")));
    let failed = Arc::new(Job::SaveNotes(root.clone(), notes("failed active")));
    let active = Arc::new(Job::SaveNotes(root.clone(), notes("active")));
    worker.notes_write_errors.insert(
        root.clone(),
        FailedSave {
            job: Arc::clone(&known),
            reason: "permanent synthetic cap".to_owned(),
        },
    );
    worker.failed_active = Some(Arc::clone(&failed));
    worker.active = Some(Arc::clone(&active));
    assert!(std::ptr::eq(
        worker.retained_notes_for_repository(&root).unwrap(),
        notes_in_job(&active)
    ));
    worker.active = None;
    assert!(std::ptr::eq(
        worker.retained_notes_for_repository(&root).unwrap(),
        notes_in_job(&failed)
    ));
    worker.failed_active = None;
    assert!(std::ptr::eq(
        worker.retained_notes_for_repository(&root).unwrap(),
        notes_in_job(&known)
    ));
    assert!(worker.notes_write_errors.contains_key(&root));
}

#[test]
fn retained_notes_snapshot_never_returns_other_roots_or_non_notes_jobs() {
    let root = PathBuf::from("retained-a");
    let other = PathBuf::from("retained-b");
    let mut worker = snapshot_only_worker();
    worker.active = Some(Arc::new(Job::SaveNotes(other.clone(), notes("other root"))));
    worker.failed_active = Some(Arc::new(Job::SaveSettings(settings(16.0))));
    worker.pending.push_back(Arc::new(Job::LoadNotes(
        Uuid::new_v4(),
        Uuid::new_v4(),
        root.clone(),
    )));
    worker.pending.push_back(Arc::new(Job::ImportNotes(
        Uuid::new_v4(),
        Uuid::new_v4(),
        root.clone(),
    )));
    worker.notes_write_errors.insert(
        other.clone(),
        FailedSave {
            job: Arc::new(Job::SaveNotes(other.clone(), notes("other failure"))),
            reason: "other root denied".to_owned(),
        },
    );
    assert!(worker.retained_notes_for_repository(&root).is_none());
    assert!(worker
        .retained_notes_for_repository(Path::new("missing-root"))
        .is_none());
    assert_eq!(
        worker.retained_notes_for_repository(&other).unwrap().notes[0].body,
        "other root"
    );
    retire_synthetic(&mut worker);
}

#[test]
fn retained_notes_snapshot_tracks_the_latest_coalesced_pending_save() {
    let root = PathBuf::from("retained-a");
    let mut worker = snapshot_only_worker();
    worker.queue(Arc::new(Job::SaveNotes(root.clone(), notes("older"))));
    let latest = Arc::new(Job::SaveNotes(root.clone(), notes("newer")));
    worker.queue(Arc::clone(&latest));
    assert_eq!(worker.pending.len(), 1);
    assert!(std::ptr::eq(
        worker.retained_notes_for_repository(&root).unwrap(),
        notes_in_job(&latest)
    ));
    assert!(worker.worker.is_none());
    retire_synthetic(&mut worker);
}
