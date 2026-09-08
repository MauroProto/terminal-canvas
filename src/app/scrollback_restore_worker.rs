use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;

use uuid::Uuid;

use super::LeafHistories;

struct Job {
    dir: PathBuf,
    panel_id: Uuid,
}

pub(super) struct Completion {
    pub(super) panel_id: Uuid,
    pub(super) histories: LeafHistories,
}

pub(super) struct ScrollbackRestoreWorker {
    jobs: SyncSender<Job>,
    completions: Receiver<Completion>,
    in_flight: bool,
}

impl ScrollbackRestoreWorker {
    pub(super) fn new() -> Self {
        let (jobs_tx, jobs_rx) = mpsc::sync_channel::<Job>(1);
        let (completion_tx, completion_rx) = mpsc::channel();
        thread::Builder::new()
            .name("scrollback-restore-reader".to_owned())
            .spawn(move || {
                while let Ok(job) = jobs_rx.recv() {
                    let histories = super::collect_leaf_histories(&job.dir, job.panel_id);
                    if completion_tx
                        .send(Completion {
                            panel_id: job.panel_id,
                            histories,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("scrollback restore worker");
        Self {
            jobs: jobs_tx,
            completions: completion_rx,
            in_flight: false,
        }
    }

    pub(super) fn submit(&mut self, dir: PathBuf, panel_id: Uuid) -> bool {
        if self.in_flight {
            return false;
        }
        match self.jobs.try_send(Job { dir, panel_id }) {
            Ok(()) => {
                self.in_flight = true;
                true
            }
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub(super) fn poll(&mut self) -> Vec<Completion> {
        let mut completions = Vec::new();
        while let Ok(completion) = self.completions.try_recv() {
            self.in_flight = false;
            completions.push(completion);
        }
        completions
    }

    pub(super) fn in_flight(&self) -> bool {
        self.in_flight
    }
}
