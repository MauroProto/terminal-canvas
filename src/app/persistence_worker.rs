//! I/O durable fuera del frame de egui.
//!
//! `sync_data`/`fsync` puede tardar decenas o cientos de milisegundos bajo
//! carga. Hacerlo desde `update()` congela input y render aunque el scheduler
//! de PTYs sea rápido. Este worker serializa layout y scrollback en un único
//! hilo acotado: como mucho hay un job de cada clase en vuelo.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;

use uuid::Uuid;

use crate::state::AppState;

pub(super) struct IncrementalEntry {
    pub(super) dir: PathBuf,
    pub(super) panel_id: Uuid,
    pub(super) leaf_id: Option<Uuid>,
    pub(super) frames: Vec<u8>,
}

pub(super) struct IncrementalBatch {
    pub(super) dir: PathBuf,
    pub(super) entries: Vec<IncrementalEntry>,
    pub(super) live_panels: Vec<(Uuid, Vec<Uuid>)>,
    pub(super) known_panels: Vec<(Uuid, Vec<Uuid>)>,
}

pub(super) struct IncrementalAck {
    pub(super) panel_id: Uuid,
    pub(super) leaf_id: Option<Uuid>,
    pub(super) written_bytes: usize,
}

pub(super) struct FullEntry {
    pub(super) dir: PathBuf,
    pub(super) panel_id: Uuid,
    pub(super) leaf_id: Option<Uuid>,
    pub(super) text: String,
    pub(super) pending_bytes: usize,
}

enum Job {
    State(AppState),
    Incremental(IncrementalBatch),
    Full(Vec<FullEntry>),
}

pub(super) enum Completion {
    State {
        snapshot: AppState,
        result: anyhow::Result<()>,
    },
    Incremental {
        rollover_panels: Vec<Uuid>,
        acknowledgements: Vec<IncrementalAck>,
    },
    Full {
        acknowledgements: Vec<IncrementalAck>,
    },
}

pub(super) struct PersistenceWorker {
    jobs: SyncSender<Job>,
    completions: Receiver<Completion>,
    state_in_flight: bool,
    scrollback_in_flight: bool,
}

impl PersistenceWorker {
    pub(super) fn new() -> Self {
        // Capacidad dos: uno de layout y uno de scrollback. Nunca se acumula
        // una cola de snapshots stale si el disco está lento.
        let (jobs_tx, jobs_rx) = mpsc::sync_channel::<Job>(2);
        let (completion_tx, completion_rx) = mpsc::channel::<Completion>();
        thread::Builder::new()
            .name("persistence-writer".to_owned())
            .spawn(move || {
                let mut next_sequences = HashMap::new();
                while let Ok(job) = jobs_rx.recv() {
                    let completion = match job {
                        Job::State(snapshot) => {
                            let result = crate::state::persistence::try_save_state(&snapshot);
                            Completion::State { snapshot, result }
                        }
                        Job::Incremental(batch) => {
                            let (rollover_panels, acknowledgements) = if crate::state::run_marker::current_process_may_write() {
                                persist_incremental_batch(batch, &mut next_sequences)
                            } else {
                                (Vec::new(), Vec::new())
                            };
                            Completion::Incremental {
                                rollover_panels,
                                acknowledgements,
                            }
                        }
                        Job::Full(entries) => Completion::Full {
                            acknowledgements: if crate::state::run_marker::current_process_may_write() {
                                persist_full_entries(entries)
                            } else {
                                Vec::new()
                            },
                        },
                    };
                    if completion_tx.send(completion).is_err() {
                        break;
                    }
                }
            })
            .expect("persistence worker thread");
        Self {
            jobs: jobs_tx,
            completions: completion_rx,
            state_in_flight: false,
            scrollback_in_flight: false,
        }
    }

    pub(super) fn state_in_flight(&self) -> bool {
        self.state_in_flight
    }

    pub(super) fn scrollback_in_flight(&self) -> bool {
        self.scrollback_in_flight
    }

    pub(super) fn submit_state(&mut self, snapshot: AppState) -> bool {
        if self.state_in_flight {
            return false;
        }
        match self.jobs.try_send(Job::State(snapshot)) {
            Ok(()) => {
                self.state_in_flight = true;
                true
            }
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => {
                log::error!("el worker de persistencia se cerró");
                false
            }
        }
    }

    pub(super) fn submit_incremental(&mut self, batch: IncrementalBatch) -> bool {
        if self.scrollback_in_flight {
            return false;
        }
        match self.jobs.try_send(Job::Incremental(batch)) {
            Ok(()) => {
                self.scrollback_in_flight = true;
                true
            }
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => {
                log::error!("el worker de persistencia se cerró");
                false
            }
        }
    }

    pub(super) fn submit_full(&mut self, entries: Vec<FullEntry>) -> bool {
        if self.scrollback_in_flight {
            return false;
        }
        match self.jobs.try_send(Job::Full(entries)) {
            Ok(()) => {
                self.scrollback_in_flight = true;
                true
            }
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => {
                log::error!("el worker de persistencia se cerró");
                false
            }
        }
    }

    pub(super) fn poll(&mut self) -> Vec<Completion> {
        let mut out = Vec::new();
        while let Ok(completion) = self.completions.try_recv() {
            self.mark_complete(&completion);
            out.push(completion);
        }
        out
    }

    /// El cierre limpio espera los jobs ya aceptados antes del checkpoint
    /// final. No se pierde el último lote por abandonar el worker.
    pub(super) fn wait_until_idle(&mut self) {
        while self.state_in_flight || self.scrollback_in_flight {
            match self.completions.recv() {
                Ok(completion) => self.mark_complete(&completion),
                Err(_) => {
                    self.state_in_flight = false;
                    self.scrollback_in_flight = false;
                }
            }
        }
    }

    fn mark_complete(&mut self, completion: &Completion) {
        match completion {
            Completion::State { .. } => self.state_in_flight = false,
            Completion::Incremental { .. } | Completion::Full { .. } => {
                self.scrollback_in_flight = false
            }
        }
    }
}

pub(super) fn persist_full_entries(entries: Vec<FullEntry>) -> Vec<IncrementalAck> {
    let mut acknowledgements = Vec::new();
    for entry in entries {
        let generation = super::read_leaf_generation(&entry.dir, entry.panel_id, entry.leaf_id)
            .saturating_add(1);
        if let Err(err) = crate::state::scrollback_store::save_leaf_scrollback_versioned(
            &entry.dir,
            entry.panel_id,
            entry.leaf_id,
            generation,
            &entry.text,
        ) {
            log::warn!("no se pudo guardar el scrollback de una hoja: {err}");
            continue;
        }
        // Checkpoint y generation se publican en el mismo rename atómico. Si
        // el proceso cae antes de este remove, el log viejo queda presente
        // pero su generation ya no coincide y el restore no lo reaplica.
        let _ = std::fs::remove_file(entry.dir.join(
            crate::state::scrollback_store::scrollback_leaf_log_file_name(
                entry.panel_id,
                entry.leaf_id,
            ),
        ));
        let _ = std::fs::remove_file(entry.dir.join(
            crate::state::scrollback_store::scrollback_leaf_gen_file_name(
                entry.panel_id,
                entry.leaf_id,
            ),
        ));
        acknowledgements.push(IncrementalAck {
            panel_id: entry.panel_id,
            leaf_id: entry.leaf_id,
            written_bytes: entry.pending_bytes,
        });
    }
    acknowledgements
}

fn persist_incremental_batch(
    batch: IncrementalBatch,
    next_sequences: &mut HashMap<(PathBuf, Uuid, Option<Uuid>), u64>,
) -> (Vec<Uuid>, Vec<IncrementalAck>) {
    let mut rollover = HashSet::new();
    let mut acknowledgements = Vec::new();
    for entry in batch.entries {
        let log_path = entry.dir.join(
            crate::state::scrollback_store::scrollback_leaf_log_file_name(
                entry.panel_id,
                entry.leaf_id,
            ),
        );
        let key = (entry.dir.clone(), entry.panel_id, entry.leaf_id);
        let first_seq = if log_path.exists() {
            *next_sequences.entry(key.clone()).or_insert_with(|| {
                std::fs::read(&log_path)
                    .ok()
                    .and_then(|bytes| crate::state::scrollback_log::read_frames(&bytes))
                    .and_then(|(_, frames)| frames.last().map(|frame| frame.seq.saturating_add(1)))
                    .unwrap_or(1)
            })
        } else {
            next_sequences.insert(key.clone(), 1);
            1
        };
        if first_seq == u64::MAX {
            log::warn!("secuencia durable agotada; se fuerza checkpoint completo");
            rollover.insert(entry.panel_id);
            next_sequences.remove(&key);
            continue;
        }
        let Some((durable_frames, next_seq)) =
            crate::state::scrollback_log::renumber_frames(&entry.frames, first_seq)
        else {
            log::warn!("lote incremental inválido; se conserva en RAM para reintento");
            continue;
        };
        if let Some(needs_rollover) = super::persist_incremental_frames(
            &entry.dir,
            entry.panel_id,
            entry.leaf_id,
            &durable_frames,
        ) {
            if needs_rollover {
                rollover.insert(entry.panel_id);
            }
            acknowledgements.push(IncrementalAck {
                panel_id: entry.panel_id,
                leaf_id: entry.leaf_id,
                written_bytes: entry.frames.len(),
            });
            next_sequences.insert(key, next_seq);
        }
    }
    let live_ids = batch
        .live_panels
        .iter()
        .map(|(panel, _)| *panel)
        .collect::<Vec<_>>();
    let known_ids = batch
        .known_panels
        .iter()
        .map(|(panel, _)| *panel)
        .collect::<Vec<_>>();
    let known_leaves = batch
        .known_panels
        .iter()
        .map(|(panel, leaves)| (*panel, leaves.as_slice()))
        .collect::<HashMap<_, _>>();
    for (panel, leaves) in &batch.live_panels {
        crate::state::scrollback_store::prune_panel_leaf_scrollback(
            &batch.dir,
            *panel,
            leaves,
            known_leaves.get(panel).copied().unwrap_or_default(),
        );
    }
    crate::state::scrollback_store::prune_scrollback(&batch.dir, &live_ids, &known_ids);
    let live_leaves = batch
        .live_panels
        .iter()
        .map(|(panel, leaves)| (*panel, leaves.iter().copied().collect::<HashSet<_>>()))
        .collect::<HashMap<_, _>>();
    next_sequences.retain(|(dir, panel, leaf), _| {
        if dir != &batch.dir {
            return true;
        }
        live_leaves
            .get(panel)
            .is_some_and(|leaves| leaf.is_none_or(|leaf| leaves.contains(&leaf)))
    });
    (rollover.into_iter().collect(), acknowledgements)
}

#[cfg(test)]
mod tests {
    use super::{
        persist_full_entries, persist_incremental_batch, FullEntry, IncrementalBatch,
        IncrementalEntry,
    };
    use crate::state::scrollback_log::{encode_frame, FrameKind};

    #[test]
    fn failed_append_does_not_acknowledge_the_in_memory_batch() {
        let root = std::env::temp_dir().join(format!("tc-failed-append-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let invalid_dir = root.join("not-a-directory");
        std::fs::write(&invalid_dir, b"file").unwrap();
        let panel_id = uuid::Uuid::new_v4();
        let leaf_id = uuid::Uuid::new_v4();
        let frames = encode_frame(1, FrameKind::Output, b"must retry");

        let (_, acknowledgements) = persist_incremental_batch(
            IncrementalBatch {
                dir: root.clone(),
                entries: vec![IncrementalEntry {
                    dir: invalid_dir,
                    panel_id,
                    leaf_id: Some(leaf_id),
                    frames,
                }],
                live_panels: Vec::new(),
                known_panels: vec![(panel_id, vec![leaf_id])],
            },
            &mut std::collections::HashMap::new(),
        );

        let _ = std::fs::remove_dir_all(root);
        assert!(
            acknowledgements.is_empty(),
            "sin append durable el app debe conservar el prefijo pendiente"
        );
    }

    #[test]
    fn exhausted_durable_sequence_requests_full_checkpoint_without_ack() {
        let root =
            std::env::temp_dir().join(format!("tc-exhausted-durable-seq-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let panel_id = uuid::Uuid::new_v4();
        let log_path = root.join(crate::state::scrollback_store::scrollback_log_file_name(
            panel_id,
        ));
        crate::state::scrollback_log::reset_log(&log_path, 0).unwrap();
        crate::state::scrollback_log::append_frames(
            &log_path,
            &encode_frame(u64::MAX, FrameKind::Output, b"last possible frame"),
        )
        .unwrap();

        let (rollover, acknowledgements) = persist_incremental_batch(
            IncrementalBatch {
                dir: root.clone(),
                entries: vec![IncrementalEntry {
                    dir: root.clone(),
                    panel_id,
                    leaf_id: None,
                    frames: encode_frame(1, FrameKind::Output, b"must survive in memory"),
                }],
                live_panels: vec![(panel_id, Vec::new())],
                known_panels: vec![(panel_id, Vec::new())],
            },
            &mut std::collections::HashMap::new(),
        );

        assert_eq!(rollover, vec![panel_id]);
        assert!(acknowledgements.is_empty());
        let (_, frames) =
            crate::state::scrollback_log::read_frames(&std::fs::read(&log_path).unwrap()).unwrap();
        assert_eq!(frames.len(), 1, "the saturated log must remain untouched");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn incremental_flush_does_not_delete_another_instances_scrollback() {
        let root =
            std::env::temp_dir().join(format!("tc-foreign-scrollback-{}", uuid::Uuid::new_v4()));
        let local_panel = uuid::Uuid::new_v4();
        let foreign_panel = uuid::Uuid::new_v4();
        crate::state::scrollback_store::save_scrollback(&root, foreign_panel, "still active")
            .unwrap();

        persist_incremental_batch(
            IncrementalBatch {
                dir: root.clone(),
                entries: Vec::new(),
                live_panels: vec![(local_panel, Vec::new())],
                known_panels: vec![(local_panel, Vec::new())],
            },
            &mut std::collections::HashMap::new(),
        );

        assert_eq!(
            crate::state::scrollback_store::load_scrollback(&root, foreign_panel).as_deref(),
            Some("still active"),
            "una instancia no debe podar artefactos pertenecientes a otra instancia viva"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn incremental_sequence_cache_retires_closed_panels_and_leaves() {
        let root =
            std::env::temp_dir().join(format!("tc-sequence-retire-{}", uuid::Uuid::new_v4()));
        let live_panel = uuid::Uuid::new_v4();
        let live_leaf = uuid::Uuid::new_v4();
        let closed_panel = uuid::Uuid::new_v4();
        let closed_leaf = uuid::Uuid::new_v4();
        let mut sequences = std::collections::HashMap::from([
            ((root.clone(), live_panel, Some(live_leaf)), 5),
            ((root.clone(), live_panel, Some(closed_leaf)), 9),
            ((root.clone(), closed_panel, Some(closed_leaf)), 12),
        ]);

        persist_incremental_batch(
            IncrementalBatch {
                dir: root.clone(),
                entries: Vec::new(),
                live_panels: vec![(live_panel, vec![live_leaf])],
                known_panels: vec![
                    (live_panel, vec![live_leaf, closed_leaf]),
                    (closed_panel, vec![closed_leaf]),
                ],
            },
            &mut sequences,
        );

        assert_eq!(sequences.len(), 1);
        assert_eq!(
            sequences
                .get(&(root.clone(), live_panel, Some(live_leaf)))
                .copied(),
            Some(5)
        );
    }

    #[test]
    fn full_checkpoint_acknowledges_only_the_captured_prefix() {
        let root = std::env::temp_dir().join(format!("tc-full-ack-{}", uuid::Uuid::new_v4()));
        let panel_id = uuid::Uuid::new_v4();
        let leaf_id = uuid::Uuid::new_v4();
        let acknowledgements = persist_full_entries(vec![FullEntry {
            dir: root.clone(),
            panel_id,
            leaf_id: Some(leaf_id),
            text: "snapshot durable".to_owned(),
            pending_bytes: 123,
        }]);

        assert_eq!(acknowledgements.len(), 1);
        assert_eq!(acknowledgements[0].written_bytes, 123);
        assert_eq!(
            crate::state::scrollback_store::load_leaf_scrollback(&root, panel_id, Some(leaf_id))
                .as_deref(),
            Some("snapshot durable")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn full_checkpoint_tolerates_a_max_generation_without_overflow() {
        let root = std::env::temp_dir().join(format!("tc-max-generation-{}", uuid::Uuid::new_v4()));
        let panel_id = uuid::Uuid::new_v4();
        crate::state::scrollback_store::save_leaf_scrollback_versioned(
            &root,
            panel_id,
            None,
            u32::MAX,
            "anterior",
        )
        .unwrap();

        let acknowledgements = persist_full_entries(vec![FullEntry {
            dir: root.clone(),
            panel_id,
            leaf_id: None,
            text: "nuevo".to_owned(),
            pending_bytes: 5,
        }]);

        assert_eq!(acknowledgements.len(), 1);
        let (generation, text) =
            crate::state::scrollback_store::load_leaf_scrollback_checkpoint(&root, panel_id, None)
                .unwrap();
        assert_eq!(generation, Some(u32::MAX));
        assert_eq!(text, "nuevo");
        let _ = std::fs::remove_dir_all(root);
    }
}
