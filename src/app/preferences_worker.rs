//! Serial preference I/O. Pending saves coalesce by destination; the worker
//! drains accepted writes on shutdown so the last edit survives a quick exit.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};
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
    jobs: Option<SyncSender<Job>>,
    results: Receiver<Completion>,
    thread: Option<JoinHandle<()>>,
    pending: VecDeque<Job>,
    busy: bool,
}

impl Default for PreferencesWorker {
    fn default() -> Self {
        Self::with_processor(run_job)
    }
}

fn run_job(job: Job) -> Completion {
    match job {
        Job::SaveNotes(root, notes) => {
            Completion::NotesSaved(crate::orchestration::save_notes(&root, &notes))
        }
        Job::LoadNotes(key, root) => Completion::NotesLoaded {
            key,
            notes: crate::orchestration::load_notes(&root),
            legacy_available: crate::orchestration::legacy_notes_available(&root),
        },
        Job::ImportNotes(key, root) => Completion::NotesImported {
            key,
            result: crate::orchestration::load_legacy_notes(&root),
        },
        Job::SaveSettings(config) => Completion::SettingsSaved(crate::config::save(&config)),
    }
}

impl PreferencesWorker {
    fn with_processor(mut process: impl FnMut(Job) -> Completion + Send + 'static) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Job>(1);
        let (result_tx, results) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("preferences-writer".to_owned())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let completion = process(job);
                    if let Completion::NotesSaved(Err(error))
                    | Completion::SettingsSaved(Err(error)) = &completion
                    {
                        log::error!("No se pudo persistir una preferencia: {error}");
                    }
                    // An absent UI must not abandon writes already accepted.
                    let _ = result_tx.send(completion);
                }
            })
            .expect("preferences writer thread");
        Self {
            jobs: Some(tx),
            results,
            thread: Some(worker),
            pending: VecDeque::new(),
            busy: false,
        }
    }
}

impl PreferencesWorker {
    fn submit(&mut self, job: Job) {
        let existing = self
            .pending
            .iter_mut()
            .find(|pending| match (&job, pending) {
                (Job::SaveNotes(root, _), Job::SaveNotes(other, _)) => root == other,
                (Job::SaveSettings(_), Job::SaveSettings(_)) => true,
                _ => false,
            });
        if let Some(existing) = existing {
            *existing = job;
        } else {
            // Only the most recent review can receive a load completion.
            if matches!(job, Job::LoadNotes(..)) {
                self.pending
                    .retain(|pending| !matches!(pending, Job::LoadNotes(..)));
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
                .is_some_and(|jobs| jobs.send(job).is_ok());
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
        self.busy || !self.pending.is_empty()
    }

    pub(super) fn poll(&mut self) -> Vec<Completion> {
        let results = self.results.try_iter().collect::<Vec<_>>();
        if !results.is_empty() {
            self.busy = false;
        }
        self.schedule();
        results
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
    fn shutdown_drains_the_latest_notes_and_settings_to_real_temporary_files() {
        let directory =
            std::env::temp_dir().join(format!("tc-preferences-drain-{}", Uuid::new_v4()));
        let notes_path = directory.join("notes.json");
        let config_path = directory.join("config.toml");
        let worker_notes = notes_path.clone();
        let worker_config = config_path.clone();
        let mut worker = PreferencesWorker::with_processor(move |job| match job {
            Job::SaveNotes(_, notes) => Completion::NotesSaved(
                crate::orchestration::save_notes_to_path(&worker_notes, &notes),
            ),
            Job::SaveSettings(config) => {
                Completion::SettingsSaved(crate::config::save_to_path(&config, &worker_config))
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
            matches!(&pending[0], Job::SaveNotes(_, notes) if notes.notes[0].body == "edit 99")
        );
        assert!(matches!(&pending[1], Job::LoadNotes(actual, _) if *actual == key));
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
        assert!(matches!(&pending[0], Job::LoadNotes(key, _) if *key == last));
    }
}
