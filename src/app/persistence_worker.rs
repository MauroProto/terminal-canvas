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
    pub(super) runtime_session_id: Uuid,
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
    pub(super) runtime_session_id: Uuid,
    pub(super) written_bytes: usize,
}

pub(super) struct FullEntry {
    pub(super) dir: PathBuf,
    pub(super) panel_id: Uuid,
    pub(super) leaf_id: Option<Uuid>,
    pub(super) runtime_session_id: Uuid,
    pub(super) content: FullContent,
}

pub(super) enum FullContent {
    Checkpoint { text: String, pending_bytes: usize },
    PendingLog(Vec<u8>),
    Unavailable,
}

enum Job {
    Restore {
        dir: PathBuf,
        panels: Vec<(Uuid, Uuid)>,
    },
    State(AppState),
    Incremental(IncrementalBatch),
    Full(Vec<FullEntry>),
}

pub(super) enum Completion {
    Restore {
        histories: Vec<(Uuid, std::io::Result<super::LeafHistories>)>,
    },
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
    restore_in_flight: bool,
}

impl PersistenceWorker {
    pub(super) fn new() -> Self {
        Self::with_processors(
            crate::state::persistence::try_save_state,
            super::collect_leaf_histories,
        )
    }

    #[cfg(test)]
    pub(super) fn with_state_processor_for_tests(
        process: impl FnMut(&AppState) -> anyhow::Result<()> + Send + 'static,
    ) -> Self {
        Self::with_processors(process, super::collect_leaf_histories)
    }

    #[cfg(test)]
    pub(super) fn with_restore_processor_for_tests(
        process: impl FnMut(&std::path::Path, Uuid) -> std::io::Result<super::LeafHistories>
            + Send
            + 'static,
    ) -> Self {
        Self::with_processors(crate::state::persistence::try_save_state, process)
    }

    fn with_processors(
        mut process_state: impl FnMut(&AppState) -> anyhow::Result<()> + Send + 'static,
        mut process_restore: impl FnMut(&std::path::Path, Uuid) -> std::io::Result<super::LeafHistories>
            + Send
            + 'static,
    ) -> Self {
        // One startup capture, one layout and one history write at most.
        // Capture is queued before any output from the new PTYs is written.
        let (jobs_tx, jobs_rx) = mpsc::sync_channel::<Job>(3);
        let (completion_tx, completion_rx) = mpsc::channel::<Completion>();
        thread::Builder::new()
            .name("persistence-writer".to_owned())
            .spawn(move || {
                let mut next_sequences = HashMap::new();
                let mut legacy_roots = HashMap::new();
                // A failed startup capture blocks this directory for the run.
                // The FIFO installs this guard before already queued writes,
                // even if the app has not consumed the failure completion yet.
                let mut blocked_history_dirs = HashSet::new();
                while let Ok(job) = jobs_rx.recv() {
                    let completion = match job {
                        Job::Restore { dir, panels } => {
                            // The same FIFO as writes establishes an immutable
                            // old-history boundary before new live output can
                            // reach these logs. Read-only apps may still load
                            // history after another instance takes ownership.
                            let _guard = crate::state::run_marker::acquire_write_guard()
                                .ok()
                                .flatten();
                            Completion::Restore {
                                histories: panels
                                    .into_iter()
                                    .map(|(panel, root_leaf)| {
                                        let histories = process_restore(&dir, panel);
                                        match &histories {
                                            Ok(histories) => {
                                                if histories
                                                    .iter()
                                                    .any(|(leaf, _, _)| leaf.is_none())
                                                    && !histories.iter().any(|(leaf, _, _)| {
                                                        *leaf == Some(root_leaf)
                                                    })
                                                {
                                                    legacy_roots
                                                        .insert((dir.clone(), panel), root_leaf);
                                                }
                                            }
                                            Err(_) => {
                                                blocked_history_dirs.insert(dir.clone());
                                            }
                                        }
                                        (panel, histories)
                                    })
                                    .collect(),
                            }
                        }
                        Job::State(snapshot) => {
                            let result = process_state(&snapshot);
                            Completion::State { snapshot, result }
                        }
                        Job::Incremental(batch) => {
                            let (rollover_panels, acknowledgements) =
                                if blocked_history_dirs.contains(&batch.dir) {
                                    (Vec::new(), Vec::new())
                                } else if let Ok(Some(_guard)) =
                                    crate::state::run_marker::acquire_write_guard()
                                {
                                    persist_incremental_batch(
                                        batch,
                                        &mut next_sequences,
                                        &mut legacy_roots,
                                    )
                                } else {
                                    (Vec::new(), Vec::new())
                                };
                            Completion::Incremental {
                                rollover_panels,
                                acknowledgements,
                            }
                        }
                        Job::Full(mut entries) => Completion::Full {
                            acknowledgements: if let Ok(Some(_guard)) =
                                crate::state::run_marker::acquire_write_guard()
                            {
                                entries.retain(|entry| !blocked_history_dirs.contains(&entry.dir));
                                persist_full_entries(
                                    entries,
                                    &mut next_sequences,
                                    &mut legacy_roots,
                                )
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
            restore_in_flight: false,
        }
    }

    pub(super) fn state_in_flight(&self) -> bool {
        self.state_in_flight
    }

    pub(super) fn scrollback_in_flight(&self) -> bool {
        self.scrollback_in_flight
    }

    pub(super) fn restore_in_flight(&self) -> bool {
        self.restore_in_flight
    }

    pub(super) fn submit_restore(&mut self, dir: PathBuf, panels: Vec<(Uuid, Uuid)>) -> bool {
        if self.restore_in_flight {
            return false;
        }
        match self.jobs.try_send(Job::Restore { dir, panels }) {
            Ok(()) => {
                self.restore_in_flight = true;
                true
            }
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => {
                log::error!("el worker de persistencia se cerró");
                false
            }
        }
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
    pub(super) fn wait_until_idle(&mut self) -> Vec<Completion> {
        let mut completions = Vec::new();
        while self.state_in_flight || self.scrollback_in_flight || self.restore_in_flight {
            match self.completions.recv() {
                Ok(completion) => {
                    self.mark_complete(&completion);
                    completions.push(completion);
                }
                Err(_) => {
                    self.state_in_flight = false;
                    self.scrollback_in_flight = false;
                    self.restore_in_flight = false;
                }
            }
        }
        completions
    }

    fn mark_complete(&mut self, completion: &Completion) {
        match completion {
            Completion::Restore { .. } => self.restore_in_flight = false,
            Completion::State { .. } => self.state_in_flight = false,
            Completion::Incremental { .. } | Completion::Full { .. } => {
                self.scrollback_in_flight = false
            }
        }
    }
}

pub(super) fn persist_full_entries(
    entries: Vec<FullEntry>,
    next_sequences: &mut HashMap<(PathBuf, Uuid, Option<Uuid>), u64>,
    legacy_roots: &mut HashMap<(PathBuf, Uuid), Uuid>,
) -> Vec<IncrementalAck> {
    persist_full_entries_with_remove(entries, next_sequences, legacy_roots, |path| {
        std::fs::remove_file(path)
    })
}

fn persist_full_entries_with_remove(
    entries: Vec<FullEntry>,
    next_sequences: &mut HashMap<(PathBuf, Uuid, Option<Uuid>), u64>,
    legacy_roots: &mut HashMap<(PathBuf, Uuid), Uuid>,
    mut remove_file: impl FnMut(&std::path::Path) -> std::io::Result<()>,
) -> Vec<IncrementalAck> {
    let mut acknowledgements = Vec::new();
    for entry in entries {
        let (text, pending_bytes) = match entry.content {
            FullContent::Unavailable => {
                log::warn!("terminal recovery snapshot unavailable; retaining its previous durable history");
                continue;
            }
            FullContent::Checkpoint {
                text,
                pending_bytes,
            } => (text, pending_bytes),
            FullContent::PendingLog(frames) => {
                // Preserve the old checkpoint while replay is still building
                // the grid. Use the same sequence cache as ordinary autosave,
                // including when this rescue happens without app shutdown.
                let (_, mut rescued) = persist_incremental_entries(
                    vec![IncrementalEntry {
                        dir: entry.dir,
                        panel_id: entry.panel_id,
                        leaf_id: entry.leaf_id,
                        runtime_session_id: entry.runtime_session_id,
                        frames,
                    }],
                    next_sequences,
                    legacy_roots,
                );
                acknowledgements.append(&mut rescued);
                continue;
            }
        };
        let checkpoint_generation =
            match super::read_leaf_generation(&entry.dir, entry.panel_id, entry.leaf_id) {
                Ok(generation) => generation,
                Err(error) => {
                    log::warn!("no se pudo leer la generación antes del checkpoint: {error}");
                    continue;
                }
            };
        let log_path = entry.dir.join(
            crate::state::scrollback_store::scrollback_leaf_log_file_name(
                entry.panel_id,
                entry.leaf_id,
            ),
        );
        let retained_log_generation = match super::read_optional_history_bytes(&log_path) {
            Ok(Some(bytes)) => {
                let Some((generation, _)) = crate::state::scrollback_log::read_frames(&bytes)
                else {
                    log::warn!("log retenido inválido; se conserva sin reemplazar el checkpoint");
                    continue;
                };
                Some(generation)
            }
            Ok(None) => None,
            Err(error) => {
                log::warn!("no se pudo leer el log antes del checkpoint: {error}");
                continue;
            }
        };
        // Deletion may fail after checkpoint publication. Exclude both old
        // generations so even an already stale retained log stays ineligible,
        // including when the counter wraps through zero.
        let mut generation = checkpoint_generation.wrapping_add(1);
        if retained_log_generation == Some(generation) {
            generation = generation.wrapping_add(1);
        }
        if let Err(err) = crate::state::scrollback_store::save_leaf_scrollback_versioned(
            &entry.dir,
            entry.panel_id,
            entry.leaf_id,
            generation,
            &text,
        ) {
            log::warn!("no se pudo guardar el scrollback de una hoja: {err}");
            continue;
        }
        next_sequences.remove(&(entry.dir.clone(), entry.panel_id, entry.leaf_id));
        if entry.leaf_id.is_some_and(|leaf| {
            legacy_roots.get(&(entry.dir.clone(), entry.panel_id)) == Some(&leaf)
        }) {
            // Canonical publication is complete before any later raw output
            // switches away from the legacy root. The old alias remains as
            // compatibility/recovery data, never as a partial stable log.
            legacy_roots.remove(&(entry.dir.clone(), entry.panel_id));
            next_sequences.remove(&(entry.dir.clone(), entry.panel_id, None));
        }
        // Checkpoint y generation se publican en el mismo rename atómico. Si
        // el proceso cae antes de este remove, el log viejo queda presente
        // pero su generation ya no coincide y el restore no lo reaplica.
        let _ = remove_file(&log_path);
        let _ = remove_file(&entry.dir.join(
            crate::state::scrollback_store::scrollback_leaf_gen_file_name(
                entry.panel_id,
                entry.leaf_id,
            ),
        ));
        acknowledgements.push(IncrementalAck {
            panel_id: entry.panel_id,
            leaf_id: entry.leaf_id,
            runtime_session_id: entry.runtime_session_id,
            written_bytes: pending_bytes,
        });
    }
    acknowledgements
}

fn persist_incremental_entries(
    entries: Vec<IncrementalEntry>,
    next_sequences: &mut HashMap<(PathBuf, Uuid, Option<Uuid>), u64>,
    legacy_roots: &HashMap<(PathBuf, Uuid), Uuid>,
) -> (Vec<Uuid>, Vec<IncrementalAck>) {
    let mut rollover = HashSet::new();
    let mut acknowledgements = Vec::new();
    for entry in entries {
        if entry.frames.is_empty() {
            acknowledgements.push(IncrementalAck {
                panel_id: entry.panel_id,
                leaf_id: entry.leaf_id,
                runtime_session_id: entry.runtime_session_id,
                written_bytes: 0,
            });
            continue;
        }
        let storage_leaf_id = if entry.leaf_id.is_some_and(|leaf| {
            legacy_roots.get(&(entry.dir.clone(), entry.panel_id)) == Some(&leaf)
        }) {
            None
        } else {
            entry.leaf_id
        };
        let log_path = entry.dir.join(
            crate::state::scrollback_store::scrollback_leaf_log_file_name(
                entry.panel_id,
                storage_leaf_id,
            ),
        );
        let key = (entry.dir.clone(), entry.panel_id, storage_leaf_id);
        let first_seq = if let Some(next_seq) = next_sequences.get(&key) {
            *next_seq
        } else {
            let generation =
                match super::read_leaf_generation(&entry.dir, entry.panel_id, storage_leaf_id) {
                    Ok(generation) => generation,
                    Err(error) => {
                        log::warn!("no se pudo leer la generación antes del append: {error}");
                        continue;
                    }
                };
            match super::read_optional_history_bytes(&log_path) {
                Ok(Some(bytes)) => {
                    let Some((log_generation, frames)) =
                        crate::state::scrollback_log::read_frames(&bytes)
                    else {
                        log::warn!("log durable inválido; se conserva sin ACK");
                        continue;
                    };
                    if log_generation == generation {
                        frames
                            .last()
                            .map(|frame| frame.seq.saturating_add(1))
                            .unwrap_or(1)
                    } else {
                        1
                    }
                }
                Ok(None) => 1,
                Err(error) => {
                    log::warn!("no se pudo leer la secuencia durable: {error}");
                    continue;
                }
            }
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
            storage_leaf_id,
            &durable_frames,
        ) {
            if needs_rollover {
                rollover.insert(entry.panel_id);
            }
            acknowledgements.push(IncrementalAck {
                panel_id: entry.panel_id,
                leaf_id: entry.leaf_id,
                runtime_session_id: entry.runtime_session_id,
                written_bytes: entry.frames.len(),
            });
            next_sequences.insert(key, next_seq);
        }
    }
    (rollover.into_iter().collect(), acknowledgements)
}

fn persist_incremental_batch(
    batch: IncrementalBatch,
    next_sequences: &mut HashMap<(PathBuf, Uuid, Option<Uuid>), u64>,
    legacy_roots: &mut HashMap<(PathBuf, Uuid), Uuid>,
) -> (Vec<Uuid>, Vec<IncrementalAck>) {
    let (rollover, acknowledgements) =
        persist_incremental_entries(batch.entries, next_sequences, legacy_roots);
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
    legacy_roots.retain(|(dir, panel), root_leaf| {
        dir != &batch.dir
            || live_leaves
                .get(panel)
                .is_some_and(|leaves| leaves.contains(root_leaf))
    });
    (rollover, acknowledgements)
}

#[cfg(test)]
mod tests {
    use super::{
        persist_full_entries, persist_incremental_batch, FullContent, FullEntry, IncrementalBatch,
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
                    runtime_session_id: uuid::Uuid::new_v4(),
                    frames,
                }],
                live_panels: Vec::new(),
                known_panels: vec![(panel_id, vec![leaf_id])],
            },
            &mut std::collections::HashMap::new(),
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
                    runtime_session_id: uuid::Uuid::new_v4(),
                    frames: encode_frame(1, FrameKind::Output, b"must survive in memory"),
                }],
                live_panels: vec![(panel_id, Vec::new())],
                known_panels: vec![(panel_id, Vec::new())],
            },
            &mut std::collections::HashMap::new(),
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
            &mut std::collections::HashMap::new(),
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
        let acknowledgements = persist_full_entries(
            vec![FullEntry {
                dir: root.clone(),
                panel_id,
                leaf_id: Some(leaf_id),
                runtime_session_id: uuid::Uuid::new_v4(),
                content: FullContent::Checkpoint {
                    text: "snapshot durable".to_owned(),
                    pending_bytes: 123,
                },
            }],
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
        );

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
    fn full_checkpoint_wraps_generation_to_a_distinct_value() {
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

        let acknowledgements = persist_full_entries(
            vec![FullEntry {
                dir: root.clone(),
                panel_id,
                leaf_id: None,
                runtime_session_id: uuid::Uuid::new_v4(),
                content: FullContent::Checkpoint {
                    text: "nuevo".to_owned(),
                    pending_bytes: 5,
                },
            }],
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
        );

        assert_eq!(acknowledgements.len(), 1);
        let (generation, text) =
            crate::state::scrollback_store::load_leaf_scrollback_checkpoint(&root, panel_id, None)
                .unwrap();
        assert_eq!(generation, Some(0));
        assert_eq!(text, "nuevo");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn idle_barrier_returns_incremental_and_full_acknowledgements_once() {
        let root = std::env::temp_dir().join(format!("tc-barrier-ack-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let panel_id = uuid::Uuid::new_v4();
        let leaf_id = uuid::Uuid::new_v4();
        let runtime_session_id = uuid::Uuid::new_v4();
        let frames = encode_frame(1, FrameKind::Output, b"written once\r\n");
        let written_bytes = frames.len();
        let mut worker = super::PersistenceWorker::new();
        assert!(worker.submit_incremental(IncrementalBatch {
            dir: root.clone(),
            entries: vec![IncrementalEntry {
                dir: root.clone(),
                panel_id,
                leaf_id: Some(leaf_id),
                runtime_session_id,
                frames,
            }],
            live_panels: vec![(panel_id, vec![leaf_id])],
            known_panels: vec![(panel_id, vec![leaf_id])],
        }));
        let completions = worker.wait_until_idle();
        assert_eq!(completions.len(), 1);
        let super::Completion::Incremental {
            acknowledgements, ..
        } = &completions[0]
        else {
            panic!("incremental completion lost")
        };
        assert_eq!(acknowledgements.len(), 1);
        assert_eq!(acknowledgements[0].runtime_session_id, runtime_session_id);
        assert_eq!(acknowledgements[0].written_bytes, written_bytes);
        assert!(worker.poll().is_empty());
        assert!(worker.wait_until_idle().is_empty());

        assert!(worker.submit_full(vec![FullEntry {
            dir: root.clone(),
            panel_id,
            leaf_id: Some(leaf_id),
            runtime_session_id,
            content: FullContent::Checkpoint {
                text: "checkpoint\n".to_owned(),
                pending_bytes: written_bytes
            },
        }]));
        let completions = worker.wait_until_idle();
        assert_eq!(completions.len(), 1);
        let super::Completion::Full { acknowledgements } = &completions[0] else {
            panic!("full completion lost")
        };
        assert_eq!(acknowledgements.len(), 1);
        assert_eq!(acknowledgements[0].runtime_session_id, runtime_session_id);
        assert_eq!(acknowledgements[0].written_bytes, written_bytes);
        assert!(worker.poll().is_empty());
        assert!(worker.wait_until_idle().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn full_checkpoint_survives_retained_max_generation_log_and_restarts_sequences() {
        let root =
            std::env::temp_dir().join(format!("tc-full-retained-log-{}", uuid::Uuid::new_v4()));
        let panel_id = uuid::Uuid::new_v4();
        let leaf_id = uuid::Uuid::new_v4();
        let runtime_session_id = uuid::Uuid::new_v4();
        crate::state::scrollback_store::save_leaf_scrollback_versioned(
            &root,
            panel_id,
            Some(leaf_id),
            u32::MAX,
            "previous checkpoint\n",
        )
        .unwrap();
        let log_path = root.join(
            crate::state::scrollback_store::scrollback_leaf_log_file_name(panel_id, Some(leaf_id)),
        );
        crate::state::scrollback_log::reset_log(&log_path, u32::MAX).unwrap();
        crate::state::scrollback_log::append_frames(
            &log_path,
            &encode_frame(
                u64::MAX,
                FrameKind::Output,
                b"already included in the full checkpoint",
            ),
        )
        .unwrap();
        let old_log = std::fs::read(&log_path).unwrap();
        let key = (root.clone(), panel_id, Some(leaf_id));
        let mut sequences = std::collections::HashMap::from([(key.clone(), u64::MAX)]);
        let mut legacy_roots = std::collections::HashMap::new();
        let acknowledgements = super::persist_full_entries_with_remove(
            vec![FullEntry {
                dir: root.clone(),
                panel_id,
                leaf_id: Some(leaf_id),
                runtime_session_id,
                content: FullContent::Checkpoint {
                    text: "fully saved prior output\n".to_owned(),
                    pending_bytes: 42,
                },
            }],
            &mut sequences,
            &mut legacy_roots,
            |_| Err(std::io::ErrorKind::PermissionDenied.into()),
        );
        assert_eq!(acknowledgements.len(), 1);
        assert!(
            !sequences.contains_key(&key),
            "a full checkpoint invalidates the old counter"
        );
        assert_eq!(
            std::fs::read(&log_path).unwrap(),
            old_log,
            "failed deletion leaves the old artifact intact"
        );
        let (checkpoint, frames) =
            crate::state::scrollback_store::load_leaf_session(&root, panel_id, Some(leaf_id));
        assert_eq!(checkpoint, b"fully saved prior output\r\n");
        assert!(
            frames.is_empty(),
            "the retained previous generation must never be reapplied"
        );
        let (rollover, acknowledgements) = persist_incremental_batch(
            IncrementalBatch {
                dir: root.clone(),
                entries: vec![IncrementalEntry {
                    dir: root.clone(),
                    panel_id,
                    leaf_id: Some(leaf_id),
                    runtime_session_id,
                    frames: encode_frame(1, FrameKind::Output, b"new output"),
                }],
                live_panels: vec![(panel_id, vec![leaf_id])],
                known_panels: vec![(panel_id, vec![leaf_id])],
            },
            &mut sequences,
            &mut legacy_roots,
        );
        assert!(
            rollover.is_empty(),
            "the saturated old cache must not force another full save"
        );
        assert_eq!(acknowledgements.len(), 1);
        let (generation, frames) =
            crate::state::scrollback_log::read_frames(&std::fs::read(&log_path).unwrap()).unwrap();
        assert_eq!(generation, 0);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].seq, 1);
        assert_eq!(frames[0].payload, b"new output");
        let _ = std::fs::remove_dir_all(root);
    }

    fn remove_owned_temp_directory(root: &std::path::Path, expected_name: &str) {
        let resolved_root = root.canonicalize().unwrap();
        let resolved_temp = std::env::temp_dir().canonicalize().unwrap();
        assert_eq!(resolved_root.parent(), Some(resolved_temp.as_path()));
        assert_eq!(
            resolved_root.file_name(),
            Some(std::ffi::OsStr::new(expected_name))
        );
        std::fs::remove_dir_all(resolved_root).unwrap();
    }

    #[cfg(windows)]
    fn assert_unreadable_artifact_is_preserved(block_checkpoint: bool) {
        use std::os::windows::fs::OpenOptionsExt;

        for warm_cache in [false, true] {
            let root_name = format!("tc-write-sharing-{}", uuid::Uuid::new_v4());
            let root = std::env::temp_dir().join(&root_name);
            let panel_id = uuid::Uuid::new_v4();
            let leaf_id = Some(uuid::Uuid::new_v4());
            let runtime_session_id = uuid::Uuid::new_v4();
            crate::state::scrollback_store::save_leaf_scrollback_versioned(
                &root,
                panel_id,
                leaf_id,
                8,
                "previous checkpoint\n",
            )
            .unwrap();
            let checkpoint_path = root.join(
                crate::state::scrollback_store::scrollback_leaf_file_name(panel_id, leaf_id),
            );
            let checkpoint_before = std::fs::read(&checkpoint_path).unwrap();
            let log_path = root.join(
                crate::state::scrollback_store::scrollback_leaf_log_file_name(panel_id, leaf_id),
            );
            crate::state::scrollback_log::reset_log(&log_path, 8).unwrap();
            crate::state::scrollback_log::append_frames(
                &log_path,
                &encode_frame(1, FrameKind::Output, b"previous tail\r\n"),
            )
            .unwrap();
            let log_before = std::fs::read(&log_path).unwrap();
            let key = (root.clone(), panel_id, leaf_id);
            let mut sequences = std::collections::HashMap::new();
            if warm_cache {
                sequences.insert(key.clone(), 2);
            }
            let sequences_before = sequences.clone();
            let mut legacy_roots = std::collections::HashMap::new();
            let frames = encode_frame(1, FrameKind::Output, b"new pending output\r\n");
            let new_entry = || IncrementalEntry {
                dir: root.clone(),
                panel_id,
                leaf_id,
                runtime_session_id,
                frames: frames.clone(),
            };
            let blocked_path = if block_checkpoint {
                &checkpoint_path
            } else {
                &log_path
            };
            let blocker = std::fs::OpenOptions::new()
                .write(true)
                .share_mode(4)
                .open(blocked_path)
                .unwrap();
            assert_eq!(
                std::fs::read(blocked_path).unwrap_err().raw_os_error(),
                Some(32)
            );
            let (rollover, acknowledgements) = super::persist_incremental_entries(
                vec![new_entry()],
                &mut sequences,
                &legacy_roots,
            );
            assert!(rollover.is_empty());
            assert!(
                acknowledgements.is_empty(),
                "unreadable durable data cannot be acknowledged"
            );
            assert_eq!(
                sequences, sequences_before,
                "a failed read must not seed or advance the sequence cache"
            );
            let acknowledgements = persist_full_entries(
                vec![FullEntry {
                    dir: root.clone(),
                    panel_id,
                    leaf_id,
                    runtime_session_id,
                    content: FullContent::Checkpoint {
                        text: "must not replace unread history\n".to_owned(),
                        pending_bytes: frames.len(),
                    },
                }],
                &mut sequences,
                &mut legacy_roots,
            );
            assert!(
                acknowledgements.is_empty(),
                "unreadable checkpoint or retained log cannot authorize a full replacement"
            );
            assert_eq!(sequences, sequences_before);
            drop(blocker);
            assert_eq!(std::fs::read(&checkpoint_path).unwrap(), checkpoint_before);
            assert_eq!(std::fs::read(&log_path).unwrap(), log_before);
            let (_, acknowledgements) = super::persist_incremental_entries(
                vec![new_entry()],
                &mut sequences,
                &legacy_roots,
            );
            assert_eq!(acknowledgements.len(), 1);
            assert_eq!(acknowledgements[0].written_bytes, frames.len());
            assert_eq!(sequences.get(&key), Some(&3));
            let (generation, written) =
                crate::state::scrollback_log::read_frames(&std::fs::read(&log_path).unwrap())
                    .unwrap();
            assert_eq!(generation, 8);
            assert_eq!(
                written.iter().map(|frame| frame.seq).collect::<Vec<_>>(),
                vec![1, 2]
            );
            assert_eq!(written[0].payload, b"previous tail\r\n");
            assert_eq!(written[1].payload, b"new pending output\r\n");
            remove_owned_temp_directory(&root, &root_name);
        }
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_incremental_log_is_preserved_with_cold_and_warm_sequence_caches() {
        assert_unreadable_artifact_is_preserved(false);
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_checkpoint_blocks_incremental_and_full_writes_without_ack() {
        assert_unreadable_artifact_is_preserved(true);
    }

    fn assert_retained_log_cannot_match_next_checkpoint(
        checkpoint_generation: u32,
        stale_generation: u32,
        expected_generation: u32,
    ) {
        let root_name = format!("tc-retained-generation-{}", uuid::Uuid::new_v4());
        let root = std::env::temp_dir().join(&root_name);
        let panel_id = uuid::Uuid::new_v4();
        let leaf_id = Some(uuid::Uuid::new_v4());
        crate::state::scrollback_store::save_leaf_scrollback_versioned(
            &root,
            panel_id,
            leaf_id,
            checkpoint_generation,
            "previous complete history\n",
        )
        .unwrap();
        let log_path = root
            .join(crate::state::scrollback_store::scrollback_leaf_log_file_name(panel_id, leaf_id));
        crate::state::scrollback_log::reset_log(&log_path, stale_generation).unwrap();
        crate::state::scrollback_log::append_frames(
            &log_path,
            &encode_frame(1, FrameKind::Output, b"stale output already discarded\r\n"),
        )
        .unwrap();
        let log_before = std::fs::read(&log_path).unwrap();
        let acknowledgements = super::persist_full_entries_with_remove(
            vec![FullEntry {
                dir: root.clone(),
                panel_id,
                leaf_id,
                runtime_session_id: uuid::Uuid::new_v4(),
                content: FullContent::Checkpoint {
                    text: "complete current history\n".to_owned(),
                    pending_bytes: 42,
                },
            }],
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
            |_| Err(std::io::ErrorKind::PermissionDenied.into()),
        );
        let log_after = std::fs::read(&log_path).unwrap();
        let (generation, text) = crate::state::scrollback_store::load_leaf_scrollback_checkpoint(
            &root, panel_id, leaf_id,
        )
        .unwrap();
        let (_, replayed) =
            crate::state::scrollback_store::load_leaf_session(&root, panel_id, leaf_id);
        remove_owned_temp_directory(&root, &root_name);
        assert_eq!(acknowledgements.len(), 1);
        assert_eq!(acknowledgements[0].written_bytes, 42);
        assert_eq!(
            generation,
            Some(expected_generation),
            "the new generation must differ from both the checkpoint and the retained log"
        );
        assert_eq!(text, "complete current history\n");
        assert_eq!(
            log_after, log_before,
            "the injected deletion failure retains the stale log"
        );
        assert!(
            replayed.is_empty(),
            "retained stale output must never become eligible for replay"
        );
    }

    #[test]
    fn retained_stale_next_generation_log_is_never_replayed_after_a_full_checkpoint() {
        assert_retained_log_cannot_match_next_checkpoint(3, 4, 5);
    }

    #[test]
    fn retained_stale_wraparound_log_is_never_replayed_after_a_full_checkpoint() {
        assert_retained_log_cannot_match_next_checkpoint(u32::MAX, 0, 1);
    }
}
