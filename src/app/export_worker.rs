//! A single lazy export worker keeps disk I/O and diagnostic compression off
//! the UI thread. Accepted exports are never replaced by a later click.

use std::fmt;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};

pub(super) enum Job {
    Text {
        path: PathBuf,
        text: String,
        lines: usize,
    },
    Diagnostics {
        version: &'static str,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Text { lines: usize },
    Diagnostics,
}

impl Job {
    fn kind(&self) -> Kind {
        match self {
            Self::Text { lines, .. } => Kind::Text { lines: *lines },
            Self::Diagnostics { .. } => Kind::Diagnostics,
        }
    }
}

#[derive(Debug)]
pub(super) struct Completion {
    pub(super) kind: Kind,
    pub(super) result: Result<PathBuf, String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SubmitError {
    Busy,
    Unavailable(String),
}

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("Ya hay una exportación en curso"),
            Self::Unavailable(reason) => write!(f, "No se pudo iniciar la exportación: {reason}"),
        }
    }
}

type Processor = Box<dyn FnMut(Job) -> Completion + Send>;

struct StartedWorker {
    jobs: Option<SyncSender<Job>>,
    results: Receiver<Completion>,
    thread: Option<JoinHandle<()>>,
}

pub(super) struct ExportWorker {
    worker: Option<StartedWorker>,
    processor: Option<Processor>,
    active: Option<Kind>,
    unavailable: Option<String>,
}

impl Default for ExportWorker {
    fn default() -> Self {
        Self::with_processor(run_job)
    }
}

fn run_job(job: Job) -> Completion {
    let kind = job.kind();
    let result = match job {
        Job::Text { path, text, .. } => {
            crate::state::durable_write::write_atomic(&path, text.as_bytes())
                .map(|_| path)
                .map_err(|error| error.to_string())
        }
        Job::Diagnostics { version } => {
            crate::utils::diagnostics::export(version).map_err(|error| error.to_string())
        }
    };
    Completion { kind, result }
}

impl ExportWorker {
    pub(super) fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(super) fn with_processor_for_tests(
        process: impl FnMut(Job) -> Completion + Send + 'static,
    ) -> Self {
        Self::with_processor(process)
    }

    fn with_processor(process: impl FnMut(Job) -> Completion + Send + 'static) -> Self {
        Self {
            worker: None,
            processor: Some(Box::new(process)),
            active: None,
            unavailable: None,
        }
    }

    fn start(&mut self) -> Result<(), SubmitError> {
        if let Some(reason) = &self.unavailable {
            return Err(SubmitError::Unavailable(reason.clone()));
        }
        if self.worker.is_some() {
            return Ok(());
        }
        let Some(mut process) = self.processor.take() else {
            return Err(self.mark_unavailable("El worker de exportación no está disponible"));
        };
        let (jobs, incoming) = mpsc::sync_channel::<Job>(1);
        let (completed, results) = mpsc::sync_channel::<Completion>(1);
        let spawned = thread::Builder::new()
            .name("export-writer".to_owned())
            .spawn(move || {
                while let Ok(job) = incoming.recv() {
                    let completion = process(job);
                    if let Err(error) = &completion.result {
                        log::warn!("No se pudo completar una exportación: {error}");
                    }
                    // There is only one accepted export until its completion
                    // is consumed, so this bounded slot cannot grow a queue.
                    let _ = completed.send(completion);
                }
            });
        match spawned {
            Ok(thread) => {
                self.worker = Some(StartedWorker {
                    jobs: Some(jobs),
                    results,
                    thread: Some(thread),
                });
                Ok(())
            }
            Err(error) => Err(self.mark_unavailable(&error.to_string())),
        }
    }

    fn mark_unavailable(&mut self, reason: &str) -> SubmitError {
        self.unavailable = Some(reason.to_owned());
        SubmitError::Unavailable(reason.to_owned())
    }

    /// Reject a second click without queuing, cloning or replacing its payload.
    /// The caller can check `busy` before capturing a terminal snapshot.
    pub(super) fn try_submit(&mut self, job: Job) -> Result<(), SubmitError> {
        if self.busy() {
            return Err(SubmitError::Busy);
        }
        self.start()?;
        let kind = job.kind();
        let sent = self
            .worker
            .as_ref()
            .and_then(|worker| worker.jobs.as_ref())
            .ok_or_else(|| SubmitError::Unavailable("El worker está cerrado".to_owned()))?
            .try_send(job);
        match sent {
            Ok(()) => {
                self.active = Some(kind);
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(SubmitError::Busy),
            Err(TrySendError::Disconnected(_)) => {
                Err(self.mark_unavailable("El worker de exportación se detuvo"))
            }
        }
    }

    /// Includes a completed export until the UI has consumed its result.
    pub(super) fn busy(&self) -> bool {
        self.active.is_some()
    }

    pub(super) fn poll(&mut self) -> Option<Completion> {
        let kind = self.active?;
        let received = self.worker.as_ref()?.results.try_recv();
        match received {
            Ok(completion) => {
                self.active = None;
                Some(completion)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(self.interrupted(kind)),
        }
    }

    /// Explicit close/update barrier: finish and report an accepted export.
    /// Normal UI frames use `poll`, which never waits for disk I/O.
    pub(super) fn drain(&mut self) -> Option<Completion> {
        let kind = self.active?;
        let received = self.worker.as_ref()?.results.recv();
        match received {
            Ok(completion) => {
                self.active = None;
                Some(completion)
            }
            Err(_) => Some(self.interrupted(kind)),
        }
    }

    fn interrupted(&mut self, kind: Kind) -> Completion {
        self.active = None;
        let reason = "El worker se detuvo antes de completar la exportación";
        let _ = self.mark_unavailable(reason);
        Completion {
            kind,
            result: Err(reason.to_owned()),
        }
    }
}

impl Drop for ExportWorker {
    fn drop(&mut self) {
        let Some(mut worker) = self.worker.take() else {
            return;
        };
        // Closing the sender lets the worker finish its accepted job before
        // exiting; there is no coalescing or cancellation at application exit.
        drop(worker.jobs.take());
        if let Some(thread) = worker.thread.take() {
            if thread.join().is_err() {
                log::warn!("El worker de exportación se detuvo inesperadamente al cerrar");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Completion, ExportWorker, Job, Kind, SubmitError};
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    const DEADLINE: Duration = Duration::from_secs(3);

    fn text_job(name: &str) -> Job {
        Job::Text {
            path: PathBuf::from(name),
            text: "first\nsecond\n".to_owned(),
            lines: 2,
        }
    }

    fn success(job: Job) -> Completion {
        let kind = job.kind();
        let path = match job {
            Job::Text { path, .. } => path,
            Job::Diagnostics { .. } => PathBuf::from("support.zip"),
        };
        Completion {
            kind,
            result: Ok(path),
        }
    }

    #[test]
    fn an_unused_worker_does_not_spawn_a_thread() {
        let worker = ExportWorker::with_processor(|_| panic!("unused processor"));
        assert!(worker.worker.is_none());
        assert!(!worker.busy());
    }

    #[test]
    fn a_busy_worker_rejects_a_second_export_without_replacing_the_first() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut worker = ExportWorker::with_processor(move |job| {
            entered_tx.send(()).expect("worker entered");
            release_rx.recv_timeout(DEADLINE).expect("release worker");
            success(job)
        });
        worker
            .try_submit(text_job("first.txt"))
            .expect("accept first");
        entered_rx.recv_timeout(DEADLINE).expect("first processing");
        assert!(worker.busy());
        assert!(worker.poll().is_none());
        assert_eq!(
            worker.try_submit(text_job("replacement.txt")),
            Err(SubmitError::Busy)
        );
        release_tx.send(()).expect("release first");
        let completion = worker.drain().expect("first completion");
        assert_eq!(completion.kind, Kind::Text { lines: 2 });
        assert_eq!(completion.result.unwrap(), PathBuf::from("first.txt"));
        assert!(!worker.busy());
        assert!(entered_rx.try_recv().is_err());
    }

    #[test]
    fn successive_exports_reuse_the_same_thread() {
        let (processed_tx, processed_rx) = mpsc::channel();
        let mut worker = ExportWorker::with_processor(move |job| {
            processed_tx
                .send(std::thread::current().id())
                .expect("observe processor");
            success(job)
        });
        worker
            .try_submit(text_job("first.txt"))
            .expect("accept first");
        let first_thread = processed_rx
            .recv_timeout(DEADLINE)
            .expect("first processed");
        assert!(worker.busy());
        let completion = worker.drain().expect("published completion");
        assert_eq!(completion.result.unwrap(), PathBuf::from("first.txt"));
        worker
            .try_submit(Job::Diagnostics { version: "test" })
            .expect("accept diagnostics");
        let second_thread = processed_rx
            .recv_timeout(DEADLINE)
            .expect("second processed");
        assert_eq!(first_thread, second_thread);
        let completion = worker.drain().expect("second completion");
        assert_eq!(completion.kind, Kind::Diagnostics);
        assert_eq!(completion.result.unwrap(), PathBuf::from("support.zip"));
        assert!(worker.poll().is_none());
    }

    #[test]
    fn poll_reports_a_published_result_and_releases_capacity() {
        let (jobs, incoming) = mpsc::sync_channel(1);
        let (completed, results) = mpsc::sync_channel(1);
        let first = text_job("first.txt");
        let mut worker = ExportWorker {
            worker: Some(super::StartedWorker {
                jobs: Some(jobs),
                results,
                thread: None,
            }),
            processor: None,
            active: Some(first.kind()),
            unavailable: None,
        };
        assert!(worker.poll().is_none(), "pending completion must not wait");
        completed.send(success(first)).expect("publish completion");
        let completion = worker.poll().expect("public poll result");
        assert_eq!(completion.kind, Kind::Text { lines: 2 });
        assert_eq!(completion.result.unwrap(), PathBuf::from("first.txt"));
        assert!(!worker.busy());
        worker
            .try_submit(text_job("second.txt"))
            .expect("capacity released");
        let accepted = incoming.recv_timeout(DEADLINE).expect("second accepted");
        completed.send(success(accepted)).expect("complete second");
        assert!(worker.poll().is_some());
        assert!(!worker.busy());
    }

    #[test]
    fn drain_reports_a_processing_error_without_losing_its_kind() {
        let mut worker = ExportWorker::with_processor(|job| Completion {
            kind: job.kind(),
            result: Err("disk full".to_owned()),
        });
        worker
            .try_submit(text_job("failed.txt"))
            .expect("accept job");
        let completion = worker.drain().expect("reported completion");
        assert_eq!(completion.kind, Kind::Text { lines: 2 });
        assert_eq!(completion.result, Err("disk full".to_owned()));
        assert!(!worker.busy());
        assert!(worker.drain().is_none());
    }

    #[test]
    fn drop_finishes_an_accepted_export_before_returning() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let mut worker = ExportWorker::with_processor(move |job| {
            entered_tx.send(()).expect("worker entered");
            release_rx.recv_timeout(DEADLINE).expect("release worker");
            finished_tx.send(()).expect("export finished");
            success(job)
        });
        worker
            .try_submit(text_job("accepted.txt"))
            .expect("accept export");
        entered_rx
            .recv_timeout(DEADLINE)
            .expect("export processing");
        let (dropping_tx, dropping_rx) = mpsc::channel();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let closer = std::thread::spawn(move || {
            dropping_tx.send(()).expect("observe close");
            drop(worker);
            dropped_tx.send(()).expect("worker dropped");
        });
        dropping_rx.recv_timeout(DEADLINE).expect("close started");
        assert!(dropped_rx.try_recv().is_err());
        release_tx.send(()).expect("release accepted export");
        finished_rx.recv_timeout(DEADLINE).expect("export finished");
        dropped_rx.recv_timeout(DEADLINE).expect("close completed");
        closer.join().expect("close thread");
    }

    #[test]
    fn a_panicked_processor_is_reported_and_future_submissions_are_rejected() {
        let mut worker = ExportWorker::with_processor(|_| panic!("simulated worker failure"));
        worker
            .try_submit(text_job("interrupted.txt"))
            .expect("accept job");
        let completion = worker.drain().expect("failure completion");
        assert_eq!(completion.kind, Kind::Text { lines: 2 });
        assert!(completion.result.is_err());
        assert!(!worker.busy());
        assert!(matches!(
            worker.try_submit(text_job("later.txt")),
            Err(SubmitError::Unavailable(_))
        ));
    }
}
