//! Serial preference I/O with retained payloads and explicit recovery.
//! Poll never waits for I/O; shutdown/update drain accepted writes and report
//! every unsaved destination. Only an explicit retry or new request restarts
//! a worker that has stopped.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::orchestration::DiffNotes;
use uuid::Uuid;

pub(super) enum Job {
    SaveNotes(PathBuf, DiffNotes),
    LoadNotes(Uuid, Uuid, PathBuf),
    ImportNotes(Uuid, Uuid, PathBuf),
    SaveSettings(crate::config::AppConfig),
}

pub(super) enum Completion {
    NotesSaved(anyhow::Result<()>),
    NotesLoaded {
        key: Uuid,
        request: Uuid,
        repo_root: PathBuf,
        result: anyhow::Result<(DiffNotes, bool)>,
    },
    NotesImported {
        key: Uuid,
        request: Uuid,
        repo_root: PathBuf,
        result: anyhow::Result<DiffNotes>,
    },
    SettingsSaved(anyhow::Result<()>),
}

impl Job {
    fn same_write_destination(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::SaveNotes(a, _), Self::SaveNotes(b, _)) => a == b,
            (Self::SaveSettings(_), Self::SaveSettings(_)) => true,
            _ => false,
        }
    }

    fn interrupted(&self, reason: &str) -> Completion {
        match self {
            Self::SaveNotes(root, _) => Completion::NotesSaved(Err(anyhow::anyhow!(
                "No se guardaron las notas de {}: {reason}",
                root.display()
            ))),
            Self::SaveSettings(_) => {
                Completion::SettingsSaved(Err(anyhow::anyhow!(reason.to_owned())))
            }
            Self::LoadNotes(key, request, root) => Completion::NotesLoaded {
                key: *key,
                request: *request,
                repo_root: root.clone(),
                result: Err(anyhow::anyhow!(
                    "No se cargaron las notas de {}: {reason}",
                    root.display()
                )),
            },
            Self::ImportNotes(key, request, root) => Completion::NotesImported {
                key: *key,
                request: *request,
                repo_root: root.clone(),
                result: Err(anyhow::anyhow!(
                    "No se importaron las notas de {}: {reason}",
                    root.display()
                )),
            },
        }
    }
}

type Processor = Box<dyn FnMut(&Job) -> Completion + Send>;
type WorkerTask = Box<dyn FnOnce() + Send>;
type Spawner = Box<dyn FnMut(WorkerTask) -> std::io::Result<JoinHandle<()>> + Send>;

struct StartedWorker {
    jobs: SyncSender<Arc<Job>>,
    results: Receiver<Completion>,
    thread: JoinHandle<()>,
}

struct FailedSave {
    job: Arc<Job>,
    reason: String,
}

pub(super) struct PreferencesWorker {
    worker: Option<StartedWorker>,
    processor: Arc<Mutex<Processor>>,
    spawner: Spawner,
    retired: Vec<JoinHandle<()>>,
    pending: VecDeque<Arc<Job>>,
    active: Option<Arc<Job>>,
    // A job without an ACK precedes the FIFO and never enters coalescing.
    failed_active: Option<Arc<Job>>,
    completions: VecDeque<Completion>,
    unavailable: Option<String>,
    notes_write_errors: BTreeMap<PathBuf, FailedSave>,
    settings_write_error: Option<FailedSave>,
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
        Job::LoadNotes(key, request, root) => Completion::NotesLoaded {
            key: *key,
            request: *request,
            repo_root: root.clone(),
            result: crate::orchestration::load_notes(root)
                .map(|notes| (notes, crate::orchestration::legacy_notes_available(root))),
        },
        Job::ImportNotes(key, request, root) => Completion::NotesImported {
            key: *key,
            request: *request,
            repo_root: root.clone(),
            result: crate::orchestration::load_legacy_notes(root),
        },
        Job::SaveSettings(config) => Completion::SettingsSaved(crate::config::save(config)),
    }
}

fn spawn(task: WorkerTask) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("preferences-writer".to_owned())
        .spawn(task)
}

impl PreferencesWorker {
    fn with_processor(process: impl FnMut(&Job) -> Completion + Send + 'static) -> Self {
        Self::with_spawner(process, spawn)
    }

    fn with_spawner(
        process: impl FnMut(&Job) -> Completion + Send + 'static,
        spawner: impl FnMut(WorkerTask) -> std::io::Result<JoinHandle<()>> + Send + 'static,
    ) -> Self {
        Self {
            worker: None,
            processor: Arc::new(Mutex::new(Box::new(process))),
            spawner: Box::new(spawner),
            retired: Vec::new(),
            pending: VecDeque::new(),
            active: None,
            failed_active: None,
            completions: VecDeque::new(),
            unavailable: None,
            notes_write_errors: BTreeMap::new(),
            settings_write_error: None,
        }
    }

    #[cfg(test)]
    pub(super) fn with_processor_for_tests(
        process: impl FnMut(&Job) -> Completion + Send + 'static,
    ) -> Self {
        Self::with_processor(process)
    }

    fn start(&mut self) -> std::io::Result<()> {
        if self.worker.is_some() {
            return Ok(());
        }
        let (jobs, incoming) = mpsc::sync_channel::<Arc<Job>>(1);
        // Drop drains a FIFO. The worker must never block waiting for the UI
        // to consume a bounded result slot during shutdown.
        let (completed, results) = mpsc::channel();
        let processor = Arc::clone(&self.processor);
        let task: WorkerTask = Box::new(move || {
            while let Ok(job) = incoming.recv() {
                // A previous callback can unwind. The production processor is
                // stateless; retain the callback and its payload for retry.
                let completion = processor.lock().unwrap_or_else(|e| e.into_inner())(job.as_ref());
                let _ = completed.send(completion);
            }
        });
        let thread = (self.spawner)(task)?;
        self.worker = Some(StartedWorker {
            jobs,
            results,
            thread,
        });
        Ok(())
    }

    fn queue(&mut self, job: Arc<Job>) {
        if let Some(existing) = self
            .pending
            .iter_mut()
            .find(|pending| job.same_write_destination(pending.as_ref()))
        {
            *existing = job;
        } else {
            if matches!(job.as_ref(), Job::LoadNotes(..)) {
                self.pending
                    .retain(|pending| !matches!(pending.as_ref(), Job::LoadNotes(..)));
            }
            self.pending.push_back(job);
        }
    }

    fn submit(&mut self, job: Job) {
        self.queue(Arc::new(job));
        if self.unavailable.is_some() {
            self.retry();
        } else {
            self.schedule();
        }
    }

    fn schedule(&mut self) {
        if self.active.is_some()
            || self.unavailable.is_some()
            || (self.failed_active.is_none() && self.pending.is_empty())
        {
            return;
        }
        if let Err(error) = self.start() {
            self.mark_unavailable(format!("No se pudo iniciar el guardado: {error}"));
            return;
        }
        let was_active = self.failed_active.is_some();
        let Some(job) = self
            .failed_active
            .take()
            .or_else(|| self.pending.pop_front())
        else {
            return;
        };
        let sent = self
            .worker
            .as_ref()
            .map(|w| w.jobs.try_send(Arc::clone(&job)));
        match sent {
            Some(Ok(())) => self.active = Some(job),
            Some(Err(TrySendError::Full(returned))) => {
                if was_active {
                    self.failed_active = Some(returned);
                } else {
                    self.pending.push_front(returned);
                }
            }
            _ => {
                if was_active {
                    self.failed_active = Some(job);
                } else {
                    self.pending.push_front(job);
                }
                self.mark_unavailable(
                    "El guardado se interrumpió antes de aceptar el trabajo".to_owned(),
                );
            }
        }
    }

    fn retire_worker(&mut self) {
        if let Some(worker) = self.worker.take() {
            drop(worker.jobs);
            self.retired.push(worker.thread);
        }
        self.reap_finished();
    }

    fn reap_finished(&mut self) {
        let mut index = 0;
        while index < self.retired.len() {
            if self.retired[index].is_finished() {
                let _ = self.retired.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
    }

    fn mark_unavailable(&mut self, reason: String) {
        if self.unavailable.is_some() {
            return;
        }
        self.unavailable = Some(reason.clone());
        self.retire_worker();
        if let Some(active) = self.active.take() {
            self.failed_active = Some(active);
        }
        let jobs: Vec<_> = self
            .failed_active
            .iter()
            .chain(self.pending.iter())
            .cloned()
            .collect();
        for job in jobs {
            let completion = job.interrupted(&reason);
            self.note_write_result(&job, &completion);
            self.completions.push_back(completion);
        }
    }

    fn write_waiting(&self, job: &Job) -> bool {
        self.active
            .iter()
            .chain(self.failed_active.iter())
            .chain(self.pending.iter())
            .any(|other| job.same_write_destination(other.as_ref()))
    }

    fn note_write_result(&mut self, job: &Arc<Job>, completion: &Completion) {
        match (job.as_ref(), completion) {
            (Job::SaveNotes(root, _), Completion::NotesSaved(Err(error))) => {
                self.notes_write_errors.insert(
                    root.clone(),
                    FailedSave {
                        job: Arc::clone(job),
                        reason: format!("{error:#}"),
                    },
                );
            }
            (Job::SaveNotes(root, _), Completion::NotesSaved(Ok(()))) => {
                if !self.write_waiting(job.as_ref()) {
                    self.notes_write_errors.remove(root);
                }
            }
            (Job::SaveSettings(_), Completion::SettingsSaved(Err(error))) => {
                self.settings_write_error = Some(FailedSave {
                    job: Arc::clone(job),
                    reason: format!("{error:#}"),
                });
            }
            (Job::SaveSettings(_), Completion::SettingsSaved(Ok(()))) => {
                if !self.write_waiting(job.as_ref()) {
                    self.settings_write_error = None;
                }
            }
            _ => {}
        }
    }

    fn finish(&mut self, completion: Completion) {
        if let Some(job) = self.active.take() {
            self.note_write_result(&job, &completion);
            self.completions.push_back(completion);
        } else {
            self.mark_unavailable(
                "El guardado devolvió una respuesta sin un trabajo aceptado".to_owned(),
            );
        }
    }

    pub(super) fn save_notes(&mut self, root: PathBuf, notes: DiffNotes) {
        self.submit(Job::SaveNotes(root, notes));
    }
    pub(super) fn load_notes(&mut self, key: Uuid, root: PathBuf) -> Uuid {
        let request = Uuid::new_v4();
        self.submit(Job::LoadNotes(key, request, root));
        request
    }
    pub(super) fn import_notes(&mut self, key: Uuid, root: PathBuf) -> Uuid {
        let request = Uuid::new_v4();
        self.submit(Job::ImportNotes(key, request, root));
        request
    }
    pub(super) fn save_settings(&mut self, config: crate::config::AppConfig) {
        self.submit(Job::SaveSettings(config));
    }

    /// In-flight or runnable work. An unavailable writer is an error, never
    /// an endless request to repaint/wait for an update.
    pub(super) fn busy(&self) -> bool {
        self.active.is_some()
            || (self.unavailable.is_none()
                && (self.failed_active.is_some() || !self.pending.is_empty()))
    }

    pub(super) fn settings_pending(&self) -> bool {
        self.active
            .iter()
            .chain(self.failed_active.iter())
            .chain(self.pending.iter())
            .any(|job| matches!(job.as_ref(), Job::SaveSettings(_)))
            || self.settings_write_error.is_some()
    }

    pub(super) fn warning(&self) -> Option<String> {
        if let Some(reason) = &self.unavailable {
            let context = self
                .failed_active
                .as_ref()
                .or_else(|| self.pending.front())
                .map(|job| match job.as_ref() {
                    Job::SaveNotes(root, _) => format!("Notas sin guardar en {}", root.display()),
                    Job::LoadNotes(_, _, root) => format!("Notas sin cargar de {}", root.display()),
                    Job::ImportNotes(_, _, root) => {
                        format!("Notas sin importar de {}", root.display())
                    }
                    Job::SaveSettings(_) => "Configuración sin guardar".to_owned(),
                });
            return Some(
                context.map_or_else(|| reason.clone(), |context| format!("{context}: {reason}")),
            );
        }
        if let Some((root, failure)) = self.notes_write_errors.iter().next() {
            return Some(format!(
                "Notas sin guardar en {}: {}",
                root.display(),
                failure.reason
            ));
        }
        self.settings_write_error.as_ref().map(|failure| {
            format!(
                "Configuración aplicada, pero sin guardar: {}",
                failure.reason
            )
        })
    }

    /// Explicit retry uses retained snapshots, never defaults or a closed
    /// settings draft. Known failed saves with a newer active/pending save are
    /// superseded. A job without an ACK stays separately ahead of the FIFO.
    pub(super) fn retry(&mut self) {
        self.unavailable = None;
        let retries: Vec<_> = self
            .notes_write_errors
            .values()
            .chain(self.settings_write_error.iter())
            .filter(|failure| !self.write_waiting(failure.job.as_ref()))
            .map(|failure| Arc::clone(&failure.job))
            .collect();
        for job in retries.into_iter().rev() {
            self.pending.push_front(job);
        }
        self.schedule();
    }

    pub(super) fn poll(&mut self) -> Vec<Completion> {
        self.reap_finished();
        loop {
            let received = self.worker.as_ref().map(|worker| worker.results.try_recv());
            match received {
                Some(Ok(completion)) => self.finish(completion),
                Some(Err(TryRecvError::Disconnected)) => {
                    self.mark_unavailable(
                        "El hilo de guardado se detuvo antes de confirmar el trabajo".to_owned(),
                    );
                    break;
                }
                _ => break,
            }
        }
        self.schedule();
        self.completions.drain(..).collect()
    }

    /// The barrier retains read/import completions for the next UI poll, even
    /// when an update is refused. A stopped worker returns an error promptly.
    pub(super) fn drain(&mut self) -> anyhow::Result<()> {
        loop {
            self.schedule();
            if let Some(reason) = &self.unavailable {
                anyhow::bail!("{reason}");
            }
            if self.active.is_some() {
                let received = self.worker.as_ref().map(|worker| worker.results.recv());
                match received {
                    Some(Ok(completion)) => self.finish(completion),
                    _ => self.mark_unavailable(
                        "El hilo de guardado se detuvo sin confirmar el trabajo".to_owned(),
                    ),
                }
            } else if self.pending.is_empty() && self.failed_active.is_none() {
                break;
            } else {
                anyhow::bail!(
                    "El canal de guardado está ocupado; se conservaron los trabajos pendientes"
                );
            }
        }
        if let Some((root, failure)) = self.notes_write_errors.iter().next() {
            anyhow::bail!(
                "An accepted notes save failed for {}: {}",
                root.display(),
                failure.reason
            );
        }
        if let Some(failure) = &self.settings_write_error {
            anyhow::bail!("An accepted preference save failed: {}", failure.reason);
        }
        Ok(())
    }
}

impl Drop for PreferencesWorker {
    fn drop(&mut self) {
        if let Err(error) = self.drain() {
            log::error!("Preferencias pendientes al cerrar: {error:#}");
        }
        self.retire_worker();
        for thread in self.retired.drain(..) {
            let _ = thread.join();
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
    fn rapid_edits_coalesce_before_a_queued_reload() {
        let mut worker = PreferencesWorker::default();
        let root = PathBuf::from("synthetic-repository");
        let key = Uuid::new_v4();
        worker.queue(Arc::new(Job::SaveNotes(root.clone(), DiffNotes::default())));
        worker.queue(Arc::new(Job::LoadNotes(key, Uuid::new_v4(), root.clone())));
        for index in 0..100 {
            let mut notes = DiffNotes::default();
            notes.add("a.rs", None, 1, &format!("edit {index}"));
            worker.queue(Arc::new(Job::SaveNotes(root.clone(), notes)));
        }
        // Take the queue before assertions: the fixture must never write a
        // user preference file, even if an assertion panics.
        let pending = std::mem::take(&mut worker.pending);
        assert_eq!(pending.len(), 2);
        assert!(
            matches!(pending[0].as_ref(), Job::SaveNotes(_, notes) if notes.notes[0].body == "edit 99")
        );
        assert!(matches!(pending[1].as_ref(), Job::LoadNotes(actual, _, _) if *actual == key));
    }

    #[test]
    fn abandoned_review_loads_do_not_accumulate() {
        let mut worker = PreferencesWorker::default();
        let last = Uuid::new_v4();
        for _ in 0..100 {
            worker.queue(Arc::new(Job::LoadNotes(
                Uuid::new_v4(),
                Uuid::new_v4(),
                PathBuf::from("old"),
            )));
        }
        worker.queue(Arc::new(Job::LoadNotes(
            last,
            Uuid::new_v4(),
            PathBuf::from("current"),
        )));
        let pending = std::mem::take(&mut worker.pending);
        assert_eq!(pending.len(), 1);
        assert!(matches!(pending[0].as_ref(), Job::LoadNotes(key, _, _) if *key == last));
    }
}
