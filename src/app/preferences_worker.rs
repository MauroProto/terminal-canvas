//! Serial preference I/O. Pending saves coalesce by destination; the worker
//! drains accepted writes on shutdown so the last edit survives a quick exit.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::orchestration::DiffNotes;
use uuid::Uuid;

enum Job {
    SaveNotes(PathBuf, DiffNotes),
    LoadNotes(Uuid, PathBuf),
    ImportNotes(Uuid, PathBuf),
    SaveSettings(crate::config::AppConfig),
}

pub(super) enum Completion {
    NotesSaved(anyhow::Result<()>),
    NotesLoaded {
        key: Uuid,
        notes: DiffNotes,
        legacy_available: bool,
    },
    NotesImported {
        key: Uuid,
        result: anyhow::Result<DiffNotes>,
    },
    SettingsSaved(anyhow::Result<()>),
}

pub(super) struct PreferencesWorker {
    jobs: Option<SyncSender<Arc<Job>>>,
    results: Receiver<(Option<PathBuf>, Completion)>,
    thread: Option<JoinHandle<()>>,
    pending: VecDeque<Arc<Job>>,
    active: Option<Arc<Job>>,
    busy: bool,
    notes_write_errors: BTreeMap<PathBuf, String>,
    settings_write_error: Option<String>,
}

impl Default for PreferencesWorker {
    fn default() -> Self {
        Self::with_processor(run_job)
    }
}

fn run_job(job: &Job) -> Completion {
    match job {
        Job::SaveNotes(root, notes) => {
            Completion::NotesSaved(crate::orchestration::save_notes(root, notes))
        }
        Job::LoadNotes(key, root) => Completion::NotesLoaded {
            key: *key,
            notes: crate::orchestration::load_notes(root),
            legacy_available: crate::orchestration::legacy_notes_available(root),
        },
        Job::ImportNotes(key, root) => Completion::NotesImported {
            key: *key,
            result: crate::orchestration::load_legacy_notes(root),
        },
        Job::SaveSettings(config) => Completion::SettingsSaved(crate::config::save(config)),
    }
}

impl PreferencesWorker {
    fn with_processor(mut process: impl FnMut(&Job) -> Completion + Send + 'static) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Arc<Job>>(1);
        let (result_tx, results) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("preferences-writer".to_owned())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let notes_root = match job.as_ref() {
                        Job::SaveNotes(root, _) => Some(root.clone()),
                        _ => None,
                    };
                    let completion = process(job.as_ref());
                    if let Completion::NotesSaved(Err(error))
                    | Completion::SettingsSaved(Err(error)) = &completion
                    {
                        log::error!("No se pudo persistir una preferencia: {error}");
                    }
                    // An absent UI must not abandon writes already accepted.
                    let _ = result_tx.send((notes_root, completion));
                }
            })
            .expect("preferences writer thread");
        Self {
            jobs: Some(tx),
            results,
            thread: Some(worker),
            pending: VecDeque::new(),
            active: None,
            busy: false,
            notes_write_errors: BTreeMap::new(),
            settings_write_error: None,
        }
    }
}

impl PreferencesWorker {
    fn submit(&mut self, job: Job) {
        let job = Arc::new(job);
        let existing =
            self.pending
                .iter_mut()
                .find(|pending| match (job.as_ref(), pending.as_ref()) {
                    (Job::SaveNotes(root, _), Job::SaveNotes(other, _)) => root == other,
                    (Job::SaveSettings(_), Job::SaveSettings(_)) => true,
                    _ => false,
                });
        if let Some(existing) = existing {
            *existing = job;
        } else {
            // Only the most recent review can receive a load completion.
            if matches!(job.as_ref(), Job::LoadNotes(..)) {
                self.pending
                    .retain(|pending| !matches!(pending.as_ref(), Job::LoadNotes(..)));
            }
            self.pending.push_back(job);
        }
        self.schedule();
    }

    fn schedule(&mut self) {
        if self.busy {
            return;
        }
        if let Some(job) = self.pending.pop_front() {
            self.busy = self
                .jobs
                .as_ref()
                .is_some_and(|jobs| jobs.send(Arc::clone(&job)).is_ok());
            if self.busy {
                self.active = Some(job);
            }
        }
    }

    pub(super) fn save_notes(&mut self, root: PathBuf, notes: DiffNotes) {
        self.submit(Job::SaveNotes(root, notes));
    }
    pub(super) fn load_notes(&mut self, key: Uuid, root: PathBuf) {
        self.submit(Job::LoadNotes(key, root));
    }
    pub(super) fn import_notes(&mut self, key: Uuid, root: PathBuf) {
        self.submit(Job::ImportNotes(key, root));
    }
    pub(super) fn save_settings(&mut self, config: crate::config::AppConfig) {
        self.submit(Job::SaveSettings(config));
    }
    pub(super) fn busy(&self) -> bool {
        self.busy || self.active.is_some() || !self.pending.is_empty()
    }

    pub(super) fn poll(&mut self) -> Vec<Completion> {
        let results = self.results.try_iter().collect::<Vec<_>>();
        for (notes_root, completion) in &results {
            self.note_write_result(notes_root.as_ref(), completion);
        }
        if !results.is_empty() {
            self.busy = false;
            self.active = None;
        }
        self.schedule();
        results
            .into_iter()
            .map(|(_, completion)| completion)
            .collect()
    }

    fn note_write_result(&mut self, notes_root: Option<&PathBuf>, completion: &Completion) {
        match (notes_root, completion) {
            (Some(root), Completion::NotesSaved(result)) => match result {
                Ok(()) => {
                    self.notes_write_errors.remove(root);
                }
                Err(error) => {
                    self.notes_write_errors
                        .insert(root.clone(), error.to_string());
                }
            },
            (_, Completion::SettingsSaved(result)) => {
                self.settings_write_error = result.as_ref().err().map(ToString::to_string)
            }
            _ => {}
        }
    }

    /// Update installation must verify accepted preference writes before asking
    /// the native window to close. Drop alone cannot report a failed write.
    pub(super) fn drain(&mut self) -> anyhow::Result<()> {
        while self.busy() {
            self.schedule();
            if self.busy {
                let (notes_root, completion) = self.results.recv().map_err(|error| {
                    anyhow::anyhow!("Preference writer stopped before finishing: {error}")
                })?;
                self.note_write_result(notes_root.as_ref(), &completion);
                self.busy = false;
                self.active = None;
            }
        }
        if let Some((root, error)) = self.notes_write_errors.iter().next() {
            anyhow::bail!(
                "An accepted notes save failed for {}: {error}",
                root.display()
            );
        }
        if let Some(error) = &self.settings_write_error {
            anyhow::bail!("An accepted preference save failed: {error}");
        }
        Ok(())
    }
}

impl Drop for PreferencesWorker {
    fn drop(&mut self) {
        if let Some(jobs) = self.jobs.take() {
            for job in self.pending.drain(..) {
                if jobs.send(job).is_err() {
                    break;
                }
            }
            drop(jobs);
        }
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_drain_reports_a_failed_save_and_allows_a_successful_retry() {
        let mut worker = PreferencesWorker::with_processor(|job| match job {
            Job::SaveSettings(config) if config.font_size < 14.0 => {
                Completion::SettingsSaved(Err(anyhow::anyhow!("disk is full")))
            }
            Job::SaveSettings(_) => Completion::SettingsSaved(Ok(())),
            _ => panic!("fixture only accepts settings"),
        });
        worker.save_settings(crate::config::AppConfig {
            font_size: 12.0,
            ..Default::default()
        });
        assert!(worker
            .drain()
            .unwrap_err()
            .to_string()
            .contains("disk is full"));
        worker.save_settings(crate::config::AppConfig {
            font_size: 14.0,
            ..Default::default()
        });
        assert!(worker.drain().is_ok());
        assert!(!worker.busy());
    }

    #[test]
    fn successful_notes_save_in_another_repository_does_not_clear_a_failed_destination() {
        let repo_a = PathBuf::from("repo-a");
        let repo_b = PathBuf::from("repo-b");
        let failed_root = repo_a.clone();
        let mut failed_once = false;
        let mut worker = PreferencesWorker::with_processor(move |job| match job {
            Job::SaveNotes(root, _) if root == &failed_root && !failed_once => {
                failed_once = true;
                Completion::NotesSaved(Err(anyhow::anyhow!("repository A is read-only")))
            }
            Job::SaveNotes(_, _) => Completion::NotesSaved(Ok(())),
            _ => panic!("fixture only accepts notes"),
        });

        worker.save_notes(repo_a.clone(), DiffNotes::default());
        worker.save_notes(repo_b.clone(), DiffNotes::default());
        let error = worker.drain().unwrap_err().to_string();
        assert!(error.contains("repo-a"));
        assert!(error.contains("repository A is read-only"));
        assert!(worker.notes_write_errors.contains_key(&repo_a));
        assert!(!worker.notes_write_errors.contains_key(&repo_b));

        worker.save_notes(repo_b, DiffNotes::default());
        assert!(worker.drain().is_err());
        worker.save_notes(repo_a, DiffNotes::default());
        assert!(worker.drain().is_ok());
        assert!(worker.notes_write_errors.is_empty());
    }

    #[test]
    fn shutdown_drains_the_latest_notes_and_settings_to_real_temporary_files() {
        let directory =
            std::env::temp_dir().join(format!("tc-preferences-drain-{}", Uuid::new_v4()));
        let notes_path = directory.join("notes.json");
        let config_path = directory.join("config.toml");
        let worker_notes = notes_path.clone();
        let worker_config = config_path.clone();
        let mut worker = PreferencesWorker::with_processor(move |job| match job {
            Job::SaveNotes(_, notes) => Completion::NotesSaved(
                crate::orchestration::save_notes_to_path(&worker_notes, notes),
            ),
            Job::SaveSettings(config) => {
                Completion::SettingsSaved(crate::config::save_to_path(config, &worker_config))
            }
            _ => panic!("fixture only accepts writes"),
        });
        for index in 0..100 {
            let mut notes = DiffNotes::default();
            notes.add("a.rs", None, 1, &format!("edit {index}"));
            worker.save_notes(PathBuf::from("synthetic-repository"), notes);
            worker.save_settings(crate::config::AppConfig {
                font_size: 12.0 + index as f32 / 100.0,
                ..Default::default()
            });
        }
        // No poll/wait calls: dropping is exactly what a quick app exit does.
        drop(worker);
        let notes: DiffNotes =
            serde_json::from_slice(&std::fs::read(&notes_path).unwrap()).unwrap();
        let config = crate::config::load_from_path(&config_path);
        assert_eq!(notes.notes[0].body, "edit 99");
        assert!((config.font_size - 12.99).abs() < 0.001);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)] // Drop prevents struct-update syntax.
    fn rapid_edits_coalesce_before_a_queued_reload() {
        let mut worker = PreferencesWorker::default();
        worker.busy = true;
        let root = PathBuf::from("synthetic-repository");
        let key = Uuid::new_v4();
        worker.save_notes(root.clone(), DiffNotes::default());
        worker.load_notes(key, root.clone());
        for index in 0..100 {
            let mut notes = DiffNotes::default();
            notes.add("a.rs", None, 1, &format!("edit {index}"));
            worker.save_notes(root.clone(), notes);
        }
        // Take the queue before assertions: the fixture must never write a
        // user preference file, even if an assertion panics.
        let pending = std::mem::take(&mut worker.pending);
        assert_eq!(pending.len(), 2);
        assert!(
            matches!(pending[0].as_ref(), Job::SaveNotes(_, notes) if notes.notes[0].body == "edit 99")
        );
        assert!(matches!(pending[1].as_ref(), Job::LoadNotes(actual, _) if *actual == key));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)] // Drop prevents struct-update syntax.
    fn abandoned_review_loads_do_not_accumulate() {
        let mut worker = PreferencesWorker::default();
        worker.busy = true;
        let last = Uuid::new_v4();
        for _ in 0..100 {
            worker.load_notes(Uuid::new_v4(), PathBuf::from("old"));
        }
        worker.load_notes(last, PathBuf::from("current"));
        let pending = std::mem::take(&mut worker.pending);
        assert_eq!(pending.len(), 1);
        assert!(matches!(pending[0].as_ref(), Job::LoadNotes(key, _) if *key == last));
    }
}
