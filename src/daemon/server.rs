//! Servidor del daemon (P3.15, T2 y T5): tiene los PTYs y los sobrevive a la
//! app. Cada conexión habla NDJSON; los eventos de salida se numeran con un
//! `seq` monotónico por sesión para que el reattach pueda deduplicar (T4).

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::protocol::{
    birth_rejected_response, decode_line, encode_response_line_with_limit, handshake_ok,
    read_protocol_line, snapshot_ansi_budget, snapshot_failure_response, Request, Response,
    SnapshotFailureKind, WireSpec, MAX_PROTOCOL_LINE_BYTES, PROTOCOL_VERSION,
};
use crate::runtime::{RuntimeScheduler, SharedPtyHandle};
use crate::terminal::pty::{HookIdentity, PtyHandle};

type AttachBoundary = (Vec<u8>, Vec<(u64, Vec<u8>)>);

/// Cuánto espera el daemon sin ninguna app conectada antes de apagarse si
/// además no le quedan sesiones (adoption timeout).
pub const ADOPTION_TIMEOUT: Duration = Duration::from_secs(120);

/// Historial que el daemon guarda por sesión para el reattach caliente.
/// Es un tope duro: el checkpoint completo vive en el store de scrollback.
pub const MAX_SNAPSHOT_BYTES: usize = 256 * 1024;
/// Máximo de eventos pendientes por cliente. Un cliente suspendido no puede
/// convertir la salida de un build en crecimiento ilimitado del daemon.
pub const SUBSCRIBER_QUEUE_CAPACITY: usize = 64;
/// Presupuesto interactivo (P3.16, T2): la sesión enfocada tiene derecho a
/// drenar hasta 32 KB antes que cualquier sesión de fondo en cada pump.
const INTERACTIVE_BYTE_BUDGET: usize = 32 * 1024;
/// Compact long-lived TUI logs from a disposable replay, never by truncating
/// bytes needed to recover the hidden primary screen.
const MAX_ALTERNATE_LOG_BYTES: u64 = 4 * 1024 * 1024;
/// Raw streaming output is split before base64 serialization. Semantic repair
/// is a single complete snapshot, bounded independently by its wire envelope.
const RAW_OUTPUT_CHUNK_BYTES: usize = 64 * 1024;
const LIVE_REPAIR_RETRY: Duration = Duration::from_secs(2);
const MAX_BIRTH_IDENTITIES: usize = 8192;

#[derive(Clone, Copy)]
struct LiveRepairPending {
    kind: SnapshotFailureKind,
    retry_at: Instant,
}

/// Metadata and both base64 envelopes are budgeted before pending frames move.
/// This limit concerns the NDJSON line, not total terminal/history memory.
fn bounded_wire_snapshot(
    term: &mut alacritty_terminal::term::Term<crate::terminal::pty::EventProxy>,
    metadata: &[u8],
    id: Uuid,
    line_limit: usize,
) -> Result<String, SnapshotFailureKind> {
    let budget = snapshot_ansi_budget(id, metadata.len(), line_limit)
        .map_err(|_| SnapshotFailureKind::TooLarge)?;
    let metadata = std::str::from_utf8(metadata).map_err(|_| SnapshotFailureKind::Unavailable)?;
    let mut snapshot =
        crate::terminal::export::bounded_live_snapshot_with_terminal_state(term, budget)
            .map_err(|_| SnapshotFailureKind::TooLarge)?
            .ansi;
    snapshot.push_str(metadata);
    encode_response_line_with_limit(
        &Response::Attached {
            id,
            snapshot: snapshot.as_bytes().to_vec(),
            seq: u64::MAX,
            alive: false,
            output_ready: false,
            hook_generation: Some(Uuid::nil()),
            hook_target: Some(super::protocol::HostHookTarget {
                workspace_id: Uuid::nil(),
                panel_id: Uuid::nil(),
                leaf_id: Uuid::nil(),
            }),
        },
        line_limit,
    )
    .map_err(|_| SnapshotFailureKind::TooLarge)?;
    Ok(snapshot)
}

/// Estado de una sesión desde la vista del daemon.
pub struct DaemonSession {
    pub spec: WireSpec,
    /// Todo lo que salió, recortado al tope, para el snapshot del reattach.
    pub snapshot: Vec<u8>,
    /// Último `seq` emitido.
    pub seq: u64,
    pub alive: bool,
    /// El grid cambió desde el último checkpoint programado.
    scrollback_dirty: bool,
    /// Frames aún no confirmados por el writer durable del daemon.
    persist_pending: Vec<u8>,
    persist_seq: u64,
    last_metadata: Vec<u8>,
    was_alternate: bool,
    last_input_error: Option<String>,
    live_repair: Option<LiveRepairPending>,
    output_ready: bool,
    // A real unscoped PTY still needs a durable destination. This is not hook identity.
    history_panel: Option<Uuid>,
    retirement_requested: bool,
    retirement_captured: bool,
    retirement_draining: bool,
    retirement_final_captured: bool,
    /// Stable authenticated app identity which created this birth. Attach is
    /// adoption, and cannot grant creator status after a UUID collision.
    birth_client: Option<Uuid>,
    adopters: HashSet<Uuid>,
    /// El PTY real (T2). `None` en los tests del registro, que no montan
    /// procesos: la lógica de sesiones se testea sin spawnear nada.
    pub handle: Option<SharedPtyHandle>,
}

impl DaemonSession {
    fn client_may_close(&self, client_id: Uuid) -> bool {
        self.birth_client == Some(client_id) || self.adopters.contains(&client_id)
    }

    fn refuse_live_snapshot(&mut self, kind: SnapshotFailureKind, now: Instant) {
        self.live_repair = Some(LiveRepairPending {
            kind,
            retry_at: now.checked_add(LIVE_REPAIR_RETRY).unwrap_or(now),
        });
    }

    fn repair_retry_due(&self, now: Instant) -> bool {
        self.live_repair
            .is_some_and(|repair| now >= repair.retry_at)
    }

    fn new(spec: WireSpec) -> Self {
        let history_panel = spec.panel_id;
        Self {
            spec,
            snapshot: Vec::new(),
            seq: 0,
            alive: true,
            scrollback_dirty: false,
            persist_pending: Vec::new(),
            persist_seq: 0,
            last_metadata: Vec::new(),
            was_alternate: false,
            last_input_error: None,
            live_repair: None,
            output_ready: false,
            history_panel,
            retirement_requested: false,
            retirement_captured: false,
            retirement_draining: false,
            retirement_final_captured: false,
            birth_client: None,
            adopters: HashSet::new(),
            handle: None,
        }
    }

    /// Registra salida nueva y devuelve el `seq` que le toca.
    pub fn push_output(&mut self, data: &[u8]) -> u64 {
        self.output_ready |= !data.is_empty();
        self.record_persist_frame(crate::state::scrollback_log::FrameKind::Output, data);
        self.push_output_event(data)
    }

    fn push_output_event(&mut self, data: &[u8]) -> u64 {
        self.seq = self.seq.checked_add(1).unwrap_or_else(|| {
            // El contador no es persistido ni controlable por el cliente. Si
            // alguna corrupción interna lo agotara, conservar MAX evita el
            // wrap a cero (que haría pasar salida vieja por nueva).
            log::error!("se agotó la secuencia de salida de una sesión del daemon");
            u64::MAX
        });
        self.snapshot.extend_from_slice(data);
        clamp_snapshot(&mut self.snapshot, MAX_SNAPSHOT_BYTES);
        self.scrollback_dirty = true;
        self.seq
    }

    fn record_persist_frame(
        &mut self,
        kind: crate::state::scrollback_log::FrameKind,
        payload: &[u8],
    ) {
        let Some(next_seq) = self.persist_seq.checked_add(1) else {
            // El checkpoint completo sigue conteniendo este output. Omitir el
            // frame incremental es preferible a generar un log con secuencias
            // duplicadas que después no pueda restaurarse.
            log::error!("se agotó la secuencia incremental del daemon");
            self.scrollback_dirty = true;
            return;
        };
        self.persist_seq = next_seq;
        self.persist_pending
            .extend_from_slice(&crate::state::scrollback_log::encode_frame(
                self.persist_seq,
                kind,
                payload,
            ));
        self.scrollback_dirty = true;
        if let Some(handle) = &self.handle {
            handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .set_daemon_durable_pressure(self.persist_pending.len());
        }
    }

    fn ingest_pty_frames(&mut self, frames: &[u8]) -> Vec<(u64, Vec<u8>)> {
        if let Some(handle) = &self.handle {
            self.output_ready |= handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .semantic_output_ready();
        } else {
            self.output_ready |= decode_frames(frames).iter().any(|frame| {
                frame.kind == crate::state::scrollback_log::FrameKind::Output
                    && !frame.payload.is_empty()
            });
        }
        let mut events = Vec::new();
        let mut output = Vec::new();
        for frame in decode_frames(frames) {
            // Durable frames keep their original kind, order and payload.
            self.record_persist_frame(frame.kind, &frame.payload);
            if frame.kind != crate::state::scrollback_log::FrameKind::Output {
                continue;
            }
            for bytes in frame.payload.chunks(RAW_OUTPUT_CHUNK_BYTES) {
                if output.len() + bytes.len() > RAW_OUTPUT_CHUNK_BYTES {
                    let seq = self.push_output_event(&output);
                    events.push((seq, std::mem::take(&mut output)));
                }
                output.extend_from_slice(bytes);
            }
        }
        if !output.is_empty() {
            let seq = self.push_output_event(&output);
            events.push((seq, output));
        }
        events
    }

    fn acknowledge_persisted(&mut self, written_bytes: usize) {
        let acknowledged = written_bytes.min(self.persist_pending.len());
        self.persist_pending.drain(..acknowledged);
        if let Some(handle) = &self.handle {
            handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .set_daemon_durable_pressure(self.persist_pending.len());
        }
        if self.persist_pending.is_empty() && !self.retirement_requested {
            // La secuencia de estos frames sólo ordena el lote en RAM. El
            // writer durable los renumera contra el archivo, así que reiniciar
            // al vaciar el lote evita un contador de vida infinita.
            self.persist_seq = 0;
        }
    }

    /// Snapshot semántico + salida todavía no numerada, tomados en una única
    /// frontera. El evento devuelto debe difundirse a subscribers anteriores;
    /// el cliente que está haciendo attach ya lo recibe dentro del snapshot.
    fn attach_boundary(
        &mut self,
        id: Uuid,
        line_limit: usize,
        now: Instant,
    ) -> Result<AttachBoundary, SnapshotFailureKind> {
        if let Some(repair) = self.live_repair.filter(|_| !self.repair_retry_due(now)) {
            return Err(repair.kind);
        }
        let Some(handle) = self.handle.clone() else {
            // Only the process-free registry uses a raw fallback. A real PTY
            // with an unreadable/restoring grid must never masquerade as it.
            encode_response_line_with_limit(
                &Response::Attached {
                    id,
                    snapshot: self.snapshot.clone(),
                    seq: self.seq,
                    alive: self.alive,
                    output_ready: self.output_ready,
                    hook_generation: self.spec.hook_generation,
                    hook_target: host_target_from_spec(&self.spec),
                },
                line_limit,
            )
            .map_err(|_| SnapshotFailureKind::TooLarge)?;
            return Ok((self.snapshot.clone(), Vec::new()));
        };
        let boundary = handle
            .lock()
            .map_err(|_| SnapshotFailureKind::Unavailable)
            .and_then(|pty| {
                let metadata = pty.metadata_osc(true);
                pty.try_attach_snapshot_and_drain(|term| {
                    let snapshot = bounded_wire_snapshot(term, &metadata, id, line_limit)?;
                    let alternate = term
                        .mode()
                        .contains(alacritty_terminal::term::TermMode::ALT_SCREEN);
                    Ok::<_, SnapshotFailureKind>((snapshot, alternate))
                })?
                .ok_or(SnapshotFailureKind::Unavailable)
            });
        let ((snapshot, alternate), frames) = match boundary {
            Ok(boundary) => boundary,
            Err(kind) => {
                self.refuse_live_snapshot(kind, now);
                return Err(kind);
            }
        };
        let restore_primary = self.live_repair.is_some() || (self.was_alternate && !alternate);
        self.was_alternate = alternate;
        self.live_repair = None;
        let output = self.ingest_pty_frames(&frames);
        let snapshot = snapshot.into_bytes();
        let events = if restore_primary {
            let seq = output
                .last()
                .map(|(seq, _)| *seq)
                .unwrap_or_else(|| self.push_output_event(&snapshot));
            vec![(seq, snapshot.clone())]
        } else {
            output
        };
        Ok((snapshot, events))
    }
}

fn decode_frames(frames: &[u8]) -> Vec<crate::state::scrollback_log::Frame> {
    if frames.is_empty() {
        return Vec::new();
    }
    let Some((_, decoded)) = crate::state::scrollback_log::read_frames(
        &[
            crate::state::scrollback_log::encode_header(0),
            frames.to_vec(),
        ]
        .concat(),
    ) else {
        return Vec::new();
    };
    decoded
}

struct ScrollbackTarget {
    session_id: Uuid,
    panel_id: Uuid,
    leaf_id: Option<Uuid>,
    text: String,
    pending_bytes: usize,
}

struct IncrementalTarget {
    session_id: Uuid,
    panel_id: Uuid,
    leaf_id: Option<Uuid>,
    frames: Vec<u8>,
    alternate: bool,
    cols: u16,
    rows: u16,
}

/// Recorta el snapshot conservando exactamente los bytes más recientes.
pub fn clamp_snapshot(snapshot: &mut Vec<u8>, max_bytes: usize) {
    if snapshot.len() <= max_bytes {
        return;
    }
    let cut = snapshot.len() - max_bytes;
    snapshot.drain(..cut);
}

struct Subscriber {
    connection_id: Uuid,
    session_id: Uuid,
    sender: std::sync::mpsc::SyncSender<Response>,
    /// Clone del socket para cortar una conexión que dejó de consumir.
    disconnect: Option<UnixStream>,
}

/// Orden de drenado del pump: la sesión prioritaria va primero, el resto en
/// orden estable por UUID. Puro para poder testear la política sin PTYs.
fn pump_order(sessions: &HashMap<Uuid, DaemonSession>, priority: Option<Uuid>) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = sessions.keys().copied().collect();
    ids.sort_by_key(Uuid::as_u128);
    if let Some(priority) = priority {
        if let Some(pos) = ids.iter().position(|id| *id == priority) {
            ids.swap(0, pos);
        }
    }
    ids
}

/// Registro de sesiones del daemon. Sin I/O: testeable de punta a punta.
#[derive(Default)]
pub struct DaemonState {
    sessions: HashMap<Uuid, DaemonSession>,
    hook_store: Option<crate::memory::MemoryStore>,
    /// Instancias de app que adoptaron cada sesión. Una sesión puede estar
    /// adjunta temporalmente en dos UIs durante una reconexión; mientras
    /// cualquiera de sus claimants siga vivo, otra app no puede eliminarla.
    session_claimants: HashMap<Uuid, HashSet<Uuid>>,
    /// No eviction: a late authenticated Spawn must remain fenced.
    birth_tombstones: HashMap<Uuid, Uuid>,
    /// Cantidad de sockets autenticados por instancia (control + PTYs).
    client_connections: HashMap<Uuid, usize>,
    /// Conexiones de app abiertas ahora mismo.
    pub clients: usize,
    /// Desde cuándo no hay ninguna app conectada.
    idle_since: Option<Instant>,
    /// Una conexión solo recibe eventos de la sesión a la que hizo attach.
    /// Esto evita difundir cada PTY a cada reader cuando hay muchos paneles.
    subscribers: Vec<Subscriber>,
    /// Sesión del panel enfocado (P3.16, T2): se drena primero en cada pump
    /// con su propio presupuesto de 32 KB para que tipear nunca se sienta
    /// atrás de un panel de fondo escupiendo salida.
    priority_session: Option<Uuid>,
    /// None selects the production limit; tests inject a small real envelope.
    snapshot_line_limit: Option<usize>,
}

impl DaemonState {
    fn snapshot_line_limit(&self) -> usize {
        self.snapshot_line_limit.unwrap_or(MAX_PROTOCOL_LINE_BYTES)
    }

    pub fn new() -> Self {
        Self {
            // Si el proceso arrancó pero la app murió antes del handshake,
            // también debe cumplirse el adoption timeout.
            idle_since: Some(Instant::now()),
            ..Self::default()
        }
    }

    /// Registra una sesión con un id dado (sin PTY).
    pub fn insert_session(&mut self, id: Uuid, spec: WireSpec) {
        self.sessions.insert(id, DaemonSession::new(spec));
    }

    /// Registra una sesión **sin** PTY (para tests del registro).
    pub fn spawn(&mut self, spec: WireSpec) -> Uuid {
        let id = Uuid::new_v4();
        self.sessions.insert(id, DaemonSession::new(spec));
        id
    }

    /// Registra una sesión y le espawnea el PTY real (T2): a partir de acá el
    /// dueño del proceso hijo es el daemon, no la app.
    pub fn spawn_with_pty(
        &mut self,
        spec: WireSpec,
        scheduler: &Arc<Mutex<RuntimeScheduler>>,
        desired_id: Option<Uuid>,
    ) -> anyhow::Result<Uuid> {
        let history_dir = crate::state::scrollback_store::scrollback_dir()
            .ok_or_else(|| anyhow::anyhow!("no se pudo resolver el historial del daemon"))?;

        self.spawn_with_pty_in_history_dir(spec, scheduler, desired_id, Some(&history_dir))
    }

    fn spawn_with_pty_in_history_dir(
        &mut self,
        spec: WireSpec,
        scheduler: &Arc<Mutex<RuntimeScheduler>>,
        desired_id: Option<Uuid>,
        history_dir: Option<&Path>,
    ) -> anyhow::Result<Uuid> {
        self.spawn_with_pty_in_history_dir_using(
            spec,
            desired_id,
            history_dir,
            |id, spec, history| {
                PtyHandle::spawn_with_history(
                    spec.cwd.as_deref().map(Path::new),
                    spec.cols.max(1),
                    spec.rows.max(1),
                    id,
                    Arc::clone(scheduler),
                    HookIdentity {
                        memory_task_id: spec.memory_task_id,
                        panel_id: spec.panel_id,
                        workspace_id: spec.workspace_id,
                        leaf_id: spec.leaf_id,

                        hook_generation: spec.hook_generation,
                        hook_owner: spec.hook_owner,
                    },
                    &history.checkpoint,
                    &history.frames,
                )
            },
        )
    }

    /// Register only a successfully created PTY. The per-call factory also
    /// lets failure regressions exercise this boundary without a shell.
    fn spawn_with_pty_in_history_dir_using(
        &mut self,
        spec: WireSpec,
        desired_id: Option<Uuid>,
        history_dir: Option<&Path>,
        spawn_pty: impl FnOnce(
            Uuid,
            &WireSpec,
            &crate::state::scrollback_store::LeafSessionHistory,
        ) -> anyhow::Result<PtyHandle>,
    ) -> anyhow::Result<Uuid> {
        let id = match desired_id {
            Some(id) if self.sessions.contains_key(&id) => {
                anyhow::bail!("el UUID solicitado ya pertenece a otra sesión: {id}");
            }
            Some(id) => id,
            None => loop {
                let candidate = Uuid::new_v4();
                if !self.sessions.contains_key(&candidate) {
                    break candidate;
                }
            },
        };
        // Validate before registering the session or opening a PTY. Starting
        // a fresh shell after a failed read would let its later autosave
        // replace history which neither the daemon nor the UI recovered.
        let history_panel = spec.panel_id.or_else(|| history_dir.map(|_| id));
        let history = match history_panel {
            Some(panel_id) => crate::state::scrollback_store::try_load_leaf_session(
                history_dir.ok_or_else(|| anyhow::anyhow!("falta el directorio de historial"))?,
                panel_id,
                spec.leaf_id,
            )?,
            None => crate::state::scrollback_store::LeafSessionHistory::default(),
        };

        let handle = spawn_pty(id, &spec, &history).inspect_err(|err| {
            log::warn!("no se pudo espawnear el PTY de {id}: {err}");
        })?;
        if let Some(command) = spec.startup_command.as_deref().map(str::trim) {
            if !command.is_empty() {
                handle.write_all(format!("{command}\n").as_bytes());
            }
        }
        let mut session = DaemonSession::new(spec);
        session.history_panel = history_panel;
        handle.set_daemon_durable_pressure(0);
        session.handle = Some(Arc::new(Mutex::new(handle)));
        self.sessions.insert(id, session);
        Ok(id)
    }

    /// Escribe bytes en el PTY de la sesión. `false` si no existe.
    pub fn write_to(&self, id: Uuid, data: &[u8]) -> bool {
        let Some(session) = self.sessions.get(&id) else {
            return false;
        };
        if session.retirement_requested {
            return false;
        }
        if let Some(handle) = session.handle.as_ref() {
            if let Ok(pty) = handle.lock() {
                pty.write_all(data);
            }
        }
        true
    }

    /// Reenvía el resize al PTY además de guardar la geometría.
    pub fn resize(&mut self, id: Uuid, cols: u16, rows: u16) -> bool {
        let Some(session) = self.sessions.get_mut(&id) else {
            return false;
        };
        if session.retirement_requested {
            return false;
        }
        session.spec.cols = cols;
        session.spec.rows = rows;
        session.scrollback_dirty = true;
        if let Some(handle) = session.handle.as_ref() {
            if let Ok(mut pty) = handle.lock() {
                pty.resize(cols.max(1), rows.max(1));
            }
        }
        true
    }

    /// Drena la salida nueva de cada PTY, la numera y devuelve los eventos a
    /// difundir. También marca las sesiones cuyo proceso murió.
    ///
    /// Carril interactivo (P3.16, T2): la sesión del panel enfocado se drena
    /// primero con presupuesto propio (32 KB) antes que el resto, para que
    /// tipear nunca se sienta atrás de un panel de fondo escupiendo salida.
    pub fn pump_output(&mut self) -> Vec<Response> {
        self.pump_output_at(Instant::now())
    }

    fn pump_output_at(&mut self, now: Instant) -> Vec<Response> {
        let mut events = Vec::new();
        let mut refused = Vec::new();
        let line_limit = self.snapshot_line_limit();
        let ids = pump_order(&self.sessions, self.priority_session);
        let mut interactive_budget = if self.priority_session.is_some() {
            INTERACTIVE_BYTE_BUDGET
        } else {
            0
        };
        for id in ids {
            let Some(session) = self.sessions.get_mut(&id) else {
                continue;
            };
            let Some(handle) = session.handle.clone() else {
                continue;
            };
            if session.retirement_requested {
                self.capture_retirement(id);
                continue;
            }
            let is_priority = self.priority_session == Some(id);
            let boundary = match handle.lock() {
                Ok(pty) => {
                    let alive = !pty.output_finished();
                    let budget = if alive && is_priority && interactive_budget > 0 {
                        interactive_budget
                    } else {
                        usize::MAX
                    };
                    if session.live_repair.is_some() && !session.repair_retry_due(now) {
                        // The first refusal drained nothing. Later pumps move only
                        // complete raw frames into the durable queue, with no live
                        // delivery or rendered repair recorded as PTY output.
                        Ok((
                            pty.drain_pending_log_capped(RAW_OUTPUT_CHUNK_BYTES),
                            alive,
                            session.was_alternate,
                            None,
                            true,
                        ))
                    } else {
                        let metadata = pty.metadata_osc(true);
                        if session.live_repair.is_some() {
                            pty.try_attach_snapshot_and_drain(|term| {
                                let snapshot =
                                    bounded_wire_snapshot(term, &metadata, id, line_limit)?;
                                let alternate = term
                                    .mode()
                                    .contains(alacritty_terminal::term::TermMode::ALT_SCREEN);
                                Ok::<_, SnapshotFailureKind>((snapshot, alternate))
                            })
                            .and_then(|boundary| boundary.ok_or(SnapshotFailureKind::Unavailable))
                            .map(|((snapshot, alternate), frames)| {
                                (frames, alive, alternate, Some(snapshot), false)
                            })
                        } else {
                            pty.try_output_update(budget, session.was_alternate, |term| {
                                bounded_wire_snapshot(term, &metadata, id, line_limit)
                            })
                            .and_then(|boundary| boundary.ok_or(SnapshotFailureKind::Unavailable))
                            .map(|(frames, alternate, repair)| {
                                (frames, alive, alternate, repair, false)
                            })
                        }
                    }
                }
                Err(poison) => {
                    if session.live_repair.is_some() && !session.repair_retry_due(now) {
                        // A poisoned outer mutex does not make its independent
                        // pending-log lock unsafe for a durable rescue. Its grid
                        // still cannot be used as a semantic snapshot.
                        let pty = poison.into_inner();
                        Ok((
                            pty.drain_pending_log_capped(RAW_OUTPUT_CHUNK_BYTES),
                            !pty.output_finished(),
                            session.was_alternate,
                            None,
                            true,
                        ))
                    } else {
                        Err(SnapshotFailureKind::Unavailable)
                    }
                }
            };
            let (frames, alive, alternate, replacement, durable_only) = match boundary {
                Ok(boundary) => boundary,
                Err(kind) => {
                    session.refuse_live_snapshot(kind, now);
                    refused.push(id);
                    continue;
                }
            };
            if is_priority {
                interactive_budget = interactive_budget.saturating_sub(frames.len());
            }
            let output = session.ingest_pty_frames(&frames);
            if durable_only {
                // Liveness remains separate from snapshot availability. Existing
                // subscribers were disconnected at refusal; no false Exit is sent.
                if !alive {
                    session.alive = false;
                }
                continue;
            }
            session.was_alternate = alternate;
            session.live_repair = None;
            if let Some(snapshot) = replacement {
                let data = snapshot.into_bytes();
                let seq = output
                    .last()
                    .map(|(seq, _)| *seq)
                    .unwrap_or_else(|| session.push_output_event(&data));
                events.push(Response::Output { id, seq, data });
            } else {
                events.extend(output.into_iter().map(|(seq, data)| Response::Output {
                    id,
                    seq,
                    data,
                }));
            }
            if let Ok(pty) = handle.lock() {
                if let Some(message) = pty.input_error() {
                    if session.last_input_error.as_ref() != Some(&message) {
                        session.last_input_error = Some(message.clone());
                        events.push(Response::InputError { id, message });
                    }
                }
                let metadata = pty.metadata_osc(false);
                if metadata != session.last_metadata {
                    session.last_metadata = metadata.clone();
                    for data in metadata.chunks(RAW_OUTPUT_CHUNK_BYTES) {
                        let seq = session.push_output_event(data);
                        events.push(Response::Output {
                            id,
                            seq,
                            data: data.to_vec(),
                        });
                    }
                }
            }
            if !alive && session.alive {
                session.alive = false;
                events.push(Response::Exit { id });
            }
        }
        for id in refused {
            self.disconnect_session_subscribers(id);
        }
        events
    }

    /// Captura checkpoints completos consistentes. El export ocurre bajo el
    /// lock del PTY, pero el I/O durable se hace después de soltar el registro
    /// global del daemon.
    fn take_dirty_scrollback_targets(&mut self) -> Vec<ScrollbackTarget> {
        self.take_dirty_scrollback_targets_at(Instant::now())
    }

    fn take_dirty_scrollback_targets_at(&mut self, now: Instant) -> Vec<ScrollbackTarget> {
        let mut targets = Vec::new();
        let mut events = Vec::new();
        let mut refused = Vec::new();
        let line_limit = self.snapshot_line_limit();
        for (session_id, session) in &mut self.sessions {
            if session.retirement_requested {
                continue;
            }
            if !session.scrollback_dirty {
                continue;
            }
            let Some(panel_id) = session.history_panel else {
                continue;
            };
            if session.live_repair.is_some() && !session.repair_retry_due(now) {
                continue;
            }
            let Some(handle) = session.handle.clone() else {
                continue;
            };
            let boundary = handle
                .lock()
                .map_err(|_| SnapshotFailureKind::Unavailable)
                .and_then(|pty| {
                    let metadata = pty.metadata_osc(true);
                    pty.try_attach_snapshot_and_drain(|term| {
                        let alternate = term
                            .mode()
                            .contains(alacritty_terminal::term::TermMode::ALT_SCREEN);
                        let repair = if session.live_repair.is_some()
                            || (session.was_alternate && !alternate)
                        {
                            Some(bounded_wire_snapshot(
                                term,
                                &metadata,
                                *session_id,
                                line_limit,
                            )?)
                        } else {
                            None
                        };
                        // Checkpoint text stays complete and independent of the
                        // bounded live replay. Both exports observe the same grid
                        // before the pending frames can leave the atomic boundary.
                        let text =
                            (!alternate).then(|| crate::terminal::export::scrollback_to_ansi(term));
                        Ok::<_, SnapshotFailureKind>((text, repair, alternate))
                    })?
                    .ok_or(SnapshotFailureKind::Unavailable)
                });
            let ((text, repair, alternate), frames) = match boundary {
                Ok(boundary) => boundary,
                Err(kind) => {
                    session.refuse_live_snapshot(kind, now);
                    refused.push(*session_id);
                    continue;
                }
            };
            let output = session.ingest_pty_frames(&frames);
            session.was_alternate = alternate;
            session.live_repair = None;
            if let Some(snapshot) = repair {
                let data = snapshot.into_bytes();
                let seq = output
                    .last()
                    .map(|(seq, _)| *seq)
                    .unwrap_or_else(|| session.push_output_event(&data));
                events.push(Response::Output {
                    id: *session_id,
                    seq,
                    data,
                });
            } else {
                events.extend(output.into_iter().map(|(seq, data)| Response::Output {
                    id: *session_id,
                    seq,
                    data,
                }));
            }
            let Some(text) = text else { continue };
            session.scrollback_dirty = false;
            targets.push(ScrollbackTarget {
                session_id: *session_id,
                panel_id,
                leaf_id: session.spec.leaf_id,
                text,
                pending_bytes: session.persist_pending.len(),
            });
        }
        for id in refused {
            self.disconnect_session_subscribers(id);
        }
        self.broadcast(&events);
        targets
    }

    fn incremental_scrollback_targets(&self) -> Vec<IncrementalTarget> {
        self.sessions
            .iter()
            .filter_map(|(session_id, session)| {
                let panel_id = session.history_panel?;
                if session.persist_pending.is_empty() {
                    return None;
                }
                Some(IncrementalTarget {
                    session_id: *session_id,
                    panel_id,
                    leaf_id: session.spec.leaf_id,
                    frames: session.persist_pending.clone(),
                    alternate: session.was_alternate,
                    cols: session.spec.cols,
                    rows: session.spec.rows,
                })
            })
            .collect()
    }

    pub fn session_ids(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self.sessions.keys().copied().collect();
        ids.sort_by_key(Uuid::as_u128);
        ids
    }

    pub fn session_mut(&mut self, id: Uuid) -> Option<&mut DaemonSession> {
        self.sessions.get_mut(&id)
    }

    pub fn session(&self, id: Uuid) -> Option<&DaemonSession> {
        self.sessions.get(&id)
    }

    /// True only after durable final output permits destroying the handle.
    pub fn kill(&mut self, id: Uuid) -> bool {
        let Some(session) = self.sessions.get_mut(&id) else {
            return false;
        };
        if host_target_from_spec(&session.spec).is_some()
            && !host_close_fenced(self.hook_store.as_ref(), id, session, None)
        {
            return false;
        }
        session.retirement_requested = true;
        // Registry-only sessions have no PTY/log ownership. Production creation
        // always publishes a handle; retaining this route preserves that harness.
        if session.handle.is_none() {
            session.retirement_captured = true;
            session.retirement_final_captured = true;
        }
        self.capture_retirement(id);
        if !self.sessions.get(&id).is_some_and(|session| {
            session.retirement_final_captured
                && (session.handle.is_none() || session.persist_pending.is_empty())
        }) {
            return false;
        }
        let session = self.sessions.remove(&id).expect("retirement was validated");
        if let Some(creator) = session.birth_client {
            self.birth_tombstones.insert(id, creator);
        }
        self.broadcast(&[Response::Exit { id }]);
        self.disconnect_session_subscribers(id);
        self.session_claimants.remove(&id);
        if self.priority_session == Some(id) {
            self.priority_session = None;
        }
        drop(session.handle);
        true
    }

    fn capture_retirement(&mut self, id: Uuid) {
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        if !session.retirement_requested || session.retirement_final_captured {
            return;
        }
        let Some(handle) = session.handle.clone() else {
            return;
        };
        let mut pty = handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !session.retirement_captured {
            match pty.take_retirement_prefix(
                &mut session.persist_pending,
                &mut session.persist_seq,
                session.history_panel.is_some(),
            ) {
                Ok(true) => session.retirement_captured = true,
                Ok(false) | Err(_) => return,
            }
        }
        // This includes all output previously moved into the durable queue too.
        // No child termination is admitted before this exact prefix is ACKed.
        if !session.retirement_draining {
            if !session.persist_pending.is_empty() {
                return;
            }
            if pty.begin_retirement_drain().is_err() {
                return;
            }
            session.retirement_draining = true;
        }
        // Transfer incrementally while draining: durable rejection still keeps
        // pressure high, but successful ACKs let the reader reach physical EOF.
        // Sample the physical boundary BEFORE the final transfer. If EOF races
        // an earlier transfer, this remains false and another pump captures the
        // newly appended tail; it cannot mark an empty early transfer as final.
        let physical_complete = pty.retirement_physical_complete();
        if !matches!(
            pty.take_retirement_tail(
                &mut session.persist_pending,
                &mut session.persist_seq,
                session.history_panel.is_some()
            ),
            Ok(true)
        ) {
            return;
        }
        // A finished/panicked/canceled reader is insufficient. No more read or
        // resize frames can arrive after this true EOF + child-wait boundary.
        session.retirement_final_captured = physical_complete;
    }

    fn finish_acknowledged_retirements(&mut self) {
        let ids: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|(id, session)| session.retirement_requested.then_some(*id))
            .collect();
        for id in ids {
            let _ = self.kill(id);
        }
    }

    /// Mata las sesiones que la app ya no reconoce (T5): si un panel se fue,
    /// su PTY no puede quedar corriendo para siempre.
    pub fn reconcile_live(&mut self, client_id: Uuid, live: &[Uuid]) -> Vec<Uuid> {
        // La lista es autoritativa sólo para este cliente. Quitar su claim de
        // lo omitido no invalida el claim de otra app todavía conectada.
        for (id, claimants) in &mut self.session_claimants {
            if !live.contains(id) {
                claimants.remove(&client_id);
            }
        }
        for id in live {
            if self.sessions.get(id).is_some_and(|session| {
                session.birth_client.is_none() || session.client_may_close(client_id)
            }) {
                self.session_claimants
                    .entry(*id)
                    .or_default()
                    .insert(client_id);
            }
        }
        let orphans: Vec<Uuid> = self
            .sessions
            .keys()
            .copied()
            .filter(|id| {
                if live.contains(id) {
                    return false;
                }
                if self.sessions.get(id).is_some_and(|session| {
                    session.birth_client.is_some() && !session.client_may_close(client_id)
                }) {
                    return false;
                }
                !self.session_claimants.get(id).is_some_and(|claimants| {
                    claimants
                        .iter()
                        .any(|claimant| self.client_connections.contains_key(claimant))
                })
            })
            .collect();
        let mut sorted: Vec<_> = orphans.into_iter().filter(|id| self.kill(*id)).collect();
        sorted.sort_by_key(Uuid::as_u128);
        sorted
    }

    pub fn client_connected(&mut self, client_id: Uuid) {
        self.clients = self.clients.saturating_add(1);
        let connections = self.client_connections.entry(client_id).or_default();
        *connections = connections.saturating_add(1);
        self.idle_since = None;
    }

    /// Asocia una conexión a una única sesión. Repetir attach reemplaza la
    /// suscripción anterior de esa conexión, sin acumular canales obsoletos.
    pub fn subscribe(
        &mut self,
        connection_id: Uuid,
        session_id: Uuid,
        sender: std::sync::mpsc::SyncSender<Response>,
        disconnect: Option<UnixStream>,
    ) {
        self.subscribers
            .retain(|subscriber| subscriber.connection_id != connection_id);
        self.subscribers.push(Subscriber {
            connection_id,
            session_id,
            sender,
            disconnect,
        });
    }

    pub fn unsubscribe(&mut self, connection_id: Uuid) {
        self.subscribers
            .retain(|subscriber| subscriber.connection_id != connection_id);
    }

    /// Empuja los eventos a las apps conectadas y descarta los canales muertos
    /// (una app que se cerró): sin esto la lista crecería para siempre.
    fn disconnect_session_subscribers(&mut self, session_id: Uuid) {
        self.subscribers.retain(|subscriber| {
            if subscriber.session_id != session_id {
                return true;
            }
            if let Some(stream) = subscriber.disconnect.as_ref() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            false
        });
    }

    pub fn broadcast(&mut self, events: &[Response]) {
        use std::sync::mpsc::TrySendError;

        self.subscribers.retain(|subscriber| {
            events.iter().all(|event| {
                if event_session_id(event).is_some_and(|id| id != subscriber.session_id) {
                    return true;
                }
                match subscriber.sender.try_send(event.clone()) {
                    Ok(()) => true,
                    // Desconectar el consumidor lento conserva la salud del
                    // daemon y del PTY. La app puede reattach con snapshot+seq.
                    Err(TrySendError::Full(_)) => {
                        log::warn!(
                            "cliente lento desconectado de la sesión {}",
                            subscriber.session_id
                        );
                        if let Some(stream) = subscriber.disconnect.as_ref() {
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                        }
                        false
                    }
                    Err(TrySendError::Disconnected(_)) => false,
                }
            })
        });
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }

    pub fn client_disconnected(&mut self, client_id: Uuid, now: Instant) {
        self.clients = self.clients.saturating_sub(1);
        if let Some(connections) = self.client_connections.get_mut(&client_id) {
            *connections = connections.saturating_sub(1);
            if *connections == 0 {
                self.client_connections.remove(&client_id);
            }
        }
        if self.clients == 0 {
            self.idle_since = Some(now);
        }
    }

    /// An explicit idle-shutdown request remains pending, but can only take
    /// effect while the current registry contains no sessions.
    fn should_shutdown_with_request(&self, now: Instant, requested: bool) -> bool {
        (requested && self.sessions.is_empty()) || self.should_shutdown(now)
    }

    /// ¿Puede apagarse? Solo si no quedan apps ni sesiones. Con sesiones vivas
    /// el daemon se queda, que es justo el motivo de que exista.
    pub fn should_shutdown(&self, now: Instant) -> bool {
        if self.clients > 0 || !self.sessions.is_empty() {
            return false;
        }
        match self.idle_since {
            Some(since) => now.duration_since(since) >= ADOPTION_TIMEOUT,
            None => false,
        }
    }
}

fn persist_scrollback_targets(targets: Vec<ScrollbackTarget>) -> (Vec<(Uuid, usize)>, Vec<Uuid>) {
    if targets.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let Some(dir) = crate::state::scrollback_store::scrollback_dir() else {
        return (
            Vec::new(),
            targets
                .into_iter()
                .map(|target| target.session_id)
                .collect(),
        );
    };
    persist_scrollback_targets_in_dir(&dir, targets)
}

fn persist_scrollback_targets_in_dir(
    dir: &Path,
    targets: Vec<ScrollbackTarget>,
) -> (Vec<(Uuid, usize)>, Vec<Uuid>) {
    let mut acknowledgements = Vec::new();
    let mut failed = Vec::new();
    for target in targets {
        let history = match crate::state::scrollback_store::try_load_leaf_session(
            dir,
            target.panel_id,
            target.leaf_id,
        ) {
            Ok(history) => history,
            Err(error) => {
                log::warn!(
                    "el daemon conserva el historial ilegible antes del checkpoint: {error}"
                );
                failed.push(target.session_id);
                continue;
            }
        };
        let generation = next_checkpoint_generation(&history);
        if let Err(err) = crate::state::scrollback_store::save_leaf_scrollback_versioned(
            dir,
            target.panel_id,
            target.leaf_id,
            generation,
            &target.text,
        ) {
            log::warn!("el daemon no pudo guardar el scrollback: {err}");
            failed.push(target.session_id);
            continue;
        }

        let log_path = dir.join(
            crate::state::scrollback_store::scrollback_leaf_log_file_name(
                target.panel_id,
                target.leaf_id,
            ),
        );
        acknowledgements.push((target.session_id, target.pending_bytes));
        if let Err(err) = crate::state::scrollback_log::reset_log(&log_path, generation) {
            log::warn!("el daemon no pudo rotar el log incremental: {err}");
            failed.push(target.session_id);
            continue;
        }
    }
    (acknowledgements, failed)
}

fn persist_incremental_targets(targets: Vec<IncrementalTarget>) -> (Vec<(Uuid, usize)>, Vec<Uuid>) {
    if targets.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let Some(dir) = crate::state::scrollback_store::scrollback_dir() else {
        return (Vec::new(), Vec::new());
    };
    persist_incremental_targets_in_dir(&dir, targets)
}

fn persist_incremental_targets_in_dir(
    dir: &Path,
    targets: Vec<IncrementalTarget>,
) -> (Vec<(Uuid, usize)>, Vec<Uuid>) {
    let mut acknowledgements = Vec::new();
    let mut rollover = Vec::new();
    for target in targets {
        let history = match crate::state::scrollback_store::try_load_leaf_session(
            dir,
            target.panel_id,
            target.leaf_id,
        ) {
            Ok(history) => history,
            Err(error) => {
                log::warn!("el daemon conserva el historial ilegible antes del append: {error}");
                continue;
            }
        };
        let generation = history.generation;
        let log_path = dir.join(
            crate::state::scrollback_store::scrollback_leaf_log_file_name(
                target.panel_id,
                target.leaf_id,
            ),
        );
        let current = history.log_generation == Some(generation);
        if !current && crate::state::scrollback_log::reset_log(&log_path, generation).is_err() {
            continue;
        }
        // Renumber to continue from the last durable seq in the existing log.
        // Without this, a fresh daemon process would append seq 1 behind an
        // existing log ending at seq N, creating a gap that invalidates the
        // entire log on the next cold restore.
        let first_seq = if current {
            match history.frames.last().map(|frame| frame.seq.checked_add(1)) {
                Some(Some(next)) => next,
                Some(None) => {
                    log::warn!("secuencia durable agotada; se fuerza checkpoint completo");
                    rollover.push(target.session_id);
                    continue;
                }
                None => 1,
            }
        } else {
            1
        };
        let Some((durable_frames, _)) =
            crate::state::scrollback_log::renumber_frames(&target.frames, first_seq)
        else {
            log::warn!("lote incremental del daemon inválido; se conserva en RAM");
            continue;
        };
        if let Err(err) = crate::state::scrollback_log::append_frames(&log_path, &durable_frames) {
            log::warn!("el daemon no pudo appendear scrollback incremental: {err}");
            continue;
        }
        acknowledgements.push((target.session_id, target.frames.len()));
        if target.alternate
            && std::fs::metadata(&log_path).is_ok_and(|meta| meta.len() > MAX_ALTERNATE_LOG_BYTES)
        {
            if let Err(error) = compact_alternate_log(dir, &target, generation) {
                log::warn!(
                    "could not compact alternate-screen history; preserving its log: {error}"
                );
            }
        }
        if std::fs::metadata(&log_path)
            .map(|metadata| metadata.len() > crate::state::scrollback_log::MAX_LOG_BYTES)
            .unwrap_or(false)
        {
            rollover.push(target.session_id);
        }
    }
    (acknowledgements, rollover)
}

fn next_checkpoint_generation(history: &crate::state::scrollback_store::LeafSessionHistory) -> u32 {
    // If publication succeeds but log rotation fails, neither a current nor
    // an already stale retained log may become eligible for replay again.
    let next = history.generation.wrapping_add(1);
    if history.log_generation == Some(next) {
        next.wrapping_add(1)
    } else {
        next
    }
}

fn compact_alternate_log(
    dir: &Path,
    target: &IncrementalTarget,
    generation: u32,
) -> anyhow::Result<()> {
    use crate::state::scrollback_log::{encode_header, FrameKind};
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config, Term, TermMode};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
    let history = crate::state::scrollback_store::try_load_leaf_session(
        dir,
        target.panel_id,
        target.leaf_id,
    )?;
    anyhow::ensure!(
        history.generation == generation,
        "history generation changed before compaction"
    );
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut term = Term::new(
        Config {
            scrolling_history: crate::config::runtime_config().scrollback_lines,
            ..Default::default()
        },
        &TermSize::new(target.cols.max(1) as usize, target.rows.max(1) as usize),
        crate::terminal::pty::EventProxy::new(tx),
    );
    let mut parser = Processor::<StdSyncHandler>::new();
    parser.advance(&mut term, &history.checkpoint);
    for frame in &history.frames {
        match frame.kind {
            FrameKind::Output => parser.advance(&mut term, &frame.payload),
            FrameKind::Resize => {
                if let Some((cols, rows)) =
                    crate::state::scrollback_log::parse_resize(&frame.payload)
                {
                    term.resize(TermSize::new(cols.max(1) as usize, rows.max(1) as usize));
                }
            }
            FrameKind::Clear => parser.advance(&mut term, b"\x1b[2J\x1b[3J\x1b[H"),
        }
    }
    if !term.mode().contains(TermMode::ALT_SCREEN) {
        return Ok(());
    }
    // This is a disposable parser, so leaving its alternate screen cannot
    // mutate the live PTY or its saved cursor, selection, or keyboard stack.
    parser.advance(&mut term, b"\x1b[?1049l");
    let mut primary = crate::terminal::export::live_snapshot_with_terminal_state(&mut term);
    let alternate_entry = "\x1b[?1049h";
    while primary.len() + alternate_entry.len()
        > crate::state::scrollback_store::MAX_SNAPSHOT_CHECKPOINT_BYTES
        && term.grid().history_size() > 0
    {
        // Trim oldest history in the disposable grid, then serialize again.
        // Slicing the resulting ANSI would discard mode/cursor state or split
        // escape sequences. Visible rows and the saved cursor remain intact.
        let retained = term.grid().history_size() / 2;
        term.grid_mut().update_history(retained);
        primary = crate::terminal::export::live_snapshot_with_terminal_state(&mut term);
    }
    // Store the screen mode inside the same atomic checkpoint as the primary
    // text. A crash before log rotation must still confine later TUI output.
    primary.push_str(alternate_entry);
    let next = next_checkpoint_generation(&history);
    crate::state::scrollback_store::save_leaf_snapshot_versioned(
        dir,
        target.panel_id,
        target.leaf_id,
        next,
        &primary,
    )?;
    let new_log = encode_header(next);
    // Future TUI bytes still belong to the alternate screen. Its transient
    // pixels are unnecessary for cold recovery of the primary shell history.
    crate::state::durable_write::write_atomic(
        &dir.join(
            crate::state::scrollback_store::scrollback_leaf_log_file_name(
                target.panel_id,
                target.leaf_id,
            ),
        ),
        &new_log,
    )?;
    Ok(())
}

fn event_session_id(response: &Response) -> Option<Uuid> {
    match response {
        Response::Output { id, .. } | Response::Exit { id } | Response::InputError { id, .. } => {
            Some(*id)
        }
        _ => None,
    }
}

/// Aplica un pedido ya autenticado y devuelve la respuesta. Puro respecto del
/// socket: toda la lógica del daemon se testea por acá.
pub fn handle_request(
    state: &mut DaemonState,
    request: Request,
    scheduler: Option<&Arc<Mutex<RuntimeScheduler>>>,
) -> Response {
    handle_request_for_client(state, request, scheduler, Uuid::nil())
}

fn handle_request_for_client(
    state: &mut DaemonState,
    request: Request,
    scheduler: Option<&Arc<Mutex<RuntimeScheduler>>>,
    client_id: Uuid,
) -> Response {
    match request {
        Request::Hello { .. } => Response::Error {
            message: "handshake repetido".to_owned(),
        },
        Request::Spawn { mut spec, id } => {
            spec.hook_owner = Some(client_id);
            if id.is_some_and(|id| state.birth_tombstones.contains_key(&id)) {
                return birth_rejected_response(id.expect("identity was checked"));
            }
            if state
                .sessions
                .len()
                .saturating_add(state.birth_tombstones.len())
                >= MAX_BIRTH_IDENTITIES
                && !id.is_some_and(|id| state.sessions.contains_key(&id))
            {
                return Response::Error {
                    message: "La capacidad protegida de nacimientos está completa.".to_owned(),
                };
            }
            if let Some(id) = id {
                if let Some(session) = state.session(id) {
                    // Repeating the same birth after a lost ACK must not run
                    // a second shell or startup command. A different creator
                    // never inherits this UUID through Spawn.
                    return if session.birth_client == Some(client_id)
                        && !session.retirement_requested
                        && session.spec.hook_generation == spec.hook_generation
                        && host_target_from_spec(&session.spec) == host_target_from_spec(&spec)
                    {
                        Response::Spawned { id }
                    } else {
                        birth_rejected_response(id)
                    };
                }
            }
            // A scoped birth must carry a preauthorized nonce even when the
            // client bypasses the app API. PtyHandle consumes/rechecks it atomically.
            if host_target_from_spec(&spec).is_some()
                && (id.is_none() || spec.hook_generation.is_none() || state.hook_store.is_none())
            {
                return match id {
                    Some(id) => birth_rejected_response(id),
                    None => Response::Error {
                        message: "managed birth requires a reserved UUID and generation".to_owned(),
                    },
                };
            }
            let id = match scheduler {
                Some(scheduler) => match state.spawn_with_pty(spec, scheduler, id) {
                    Ok(id) => id,
                    Err(error) => {
                        return Response::Error {
                            message: format!("no se pudo crear la sesión del terminal: {error}"),
                        };
                    }
                },
                // Sin scheduler (tests del registro) no se espawnea nada.
                None => match id {
                    Some(id) => {
                        state.insert_session(id, spec);
                        id
                    }
                    None => state.spawn(spec),
                },
            };
            if let Some(session) = state.session_mut(id) {
                session.birth_client = Some(client_id);
            }
            state
                .session_claimants
                .entry(id)
                .or_default()
                .insert(client_id);
            Response::Spawned { id }
        }
        Request::Attach { id } => {
            let line_limit = state.snapshot_line_limit();
            match state.session_mut(id) {
                Some(session) => {
                    if session.retirement_requested {
                        return super::protocol::close_pending_response(id);
                    }
                    // Explicit adoption also authorizes closing a known session
                    // whose snapshot is unavailable. It never changes its creator.
                    session.adopters.insert(client_id);
                    let (snapshot, pending_output) =
                        match session.attach_boundary(id, line_limit, Instant::now()) {
                            Ok(boundary) => boundary,
                            Err(kind) => {
                                state.disconnect_session_subscribers(id);
                                return snapshot_failure_response(id, kind);
                            }
                        };
                    let response = Response::Attached {
                        id,
                        snapshot,
                        seq: session.seq,
                        alive: session.alive,
                        output_ready: session.output_ready,
                        hook_generation: session.spec.hook_generation,
                        hook_target: host_target_from_spec(&session.spec),
                    };
                    let events: Vec<_> = pending_output
                        .into_iter()
                        .map(|(seq, data)| Response::Output { id, seq, data })
                        .collect();
                    state.broadcast(&events);
                    state
                        .session_claimants
                        .entry(id)
                        .or_default()
                        .insert(client_id);
                    response
                }
                None => Response::Error {
                    message: format!("sesión desconocida: {id}"),
                },
            }
        }
        Request::Write { id, data } => {
            if state.write_to(id, &data) {
                Response::Sessions {
                    ids: state.session_ids(),
                }
            } else {
                Response::Error {
                    message: format!("sesión desconocida: {id}"),
                }
            }
        }
        Request::Resize { id, cols, rows } => {
            if state.resize(id, cols, rows) {
                Response::Sessions {
                    ids: state.session_ids(),
                }
            } else {
                Response::Error {
                    message: format!("sesión desconocida: {id}"),
                }
            }
        }
        Request::Kill { id } => {
            if state.session(id).is_some_and(|session| {
                host_target_from_spec(&session.spec).is_some()
                    && !host_close_fenced(state.hook_store.as_ref(), id, session, Some(client_id))
            }) {
                return birth_rejected_response(id);
            }
            if state
                .session(id)
                .is_some_and(|session| !session.client_may_close(client_id))
            {
                return birth_rejected_response(id);
            }
            if state.kill(id) {
                Response::Killed { id }
            } else if state.session(id).is_some() {
                super::protocol::close_pending_response(id)
            } else {
                Response::Error {
                    message: format!("sesión desconocida: {id}"),
                }
            }
        }
        Request::CloseBirth { id } => {
            if let Some(session) = state.session(id) {
                let managed = host_target_from_spec(&session.spec).is_some();
                if (managed
                    && !host_close_fenced(state.hook_store.as_ref(), id, session, Some(client_id)))
                    || (!managed && session.birth_client != Some(client_id))
                {
                    return birth_rejected_response(id);
                }
                if state.kill(id) {
                    Response::Killed { id }
                } else {
                    super::protocol::close_pending_response(id)
                }
            } else if let Some(creator) = state.birth_tombstones.get(&id) {
                if *creator == client_id
                    || host_unknown_close_fence(state.hook_store.as_ref(), id, client_id).is_some()
                {
                    Response::Killed { id }
                } else {
                    birth_rejected_response(id)
                }
            } else if state
                .sessions
                .len()
                .saturating_add(state.birth_tombstones.len())
                < MAX_BIRTH_IDENTITIES
            {
                // Only an unconsumed, durably fenced nonce proves no factory
                // ran. Consumed + absent registry is still uncertain after a
                // crash/restart; never acknowledge it as physical closure.
                let Some(claim) =
                    host_unknown_close_fence(state.hook_store.as_ref(), id, client_id)
                else {
                    return super::protocol::close_pending_response(id);
                };
                let no_factory = state.hook_store.as_ref().is_some_and(|store| {
                    store
                        .hook_birth_consumed(claim)
                        .is_ok_and(|consumed| !consumed)
                });
                if !no_factory {
                    return super::protocol::close_pending_response(id);
                }
                state.birth_tombstones.insert(id, client_id);
                Response::Killed { id }
            } else {
                super::protocol::close_pending_response(id)
            }
        }
        Request::SetPriority { id } => {
            state.priority_session = id;
            if let Some(scheduler) = scheduler {
                if let Ok(mut scheduler) = scheduler.lock() {
                    scheduler.set_priority_session(id);
                }
            }
            Response::Sessions {
                ids: state.session_ids(),
            }
        }
        Request::List => Response::Sessions {
            ids: state.session_ids(),
        },
        Request::ReconcileLive { ids } => {
            let killed = state.reconcile_live(client_id, &ids);
            if !killed.is_empty() {
                log::info!(
                    "reconcile mató {} sesiones (vivas: {})",
                    killed.len(),
                    ids.len()
                );
            }
            Response::Reconciled { killed }
        }
        Request::ShutdownIfIdle => Response::ShuttingDown,
    }
}

/// Corre el daemon sobre un unix socket hasta que se apaga.
pub fn serve(dir: &Path, token: String) -> std::io::Result<()> {
    let socket = super::protocol::socket_path(dir);
    std::fs::create_dir_all(dir)?;
    // El bind falla con "invalid argument" si el path no entra en sun_path:
    // se detecta antes para poder decir qué pasó.
    if !super::protocol::socket_path_fits(&socket) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "el path del socket tiene {} bytes y el tope es {}: {}",
                socket.as_os_str().len(),
                super::protocol::MAX_SOCKET_PATH_BYTES,
                socket.display()
            ),
        ));
    }
    // Nunca desenlazar a ciegas: otro daemon puede estar atendiendo en ese
    // inode y seguiría vivo pero inaccesible. Sólo se retira un socket cuya
    // conexión ya falla, que es el artefacto stale de un proceso muerto.
    if socket.exists() {
        match UnixStream::connect(&socket) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "ya hay un daemon atendiendo este socket",
                ));
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                std::fs::remove_file(&socket)?;
            }
            Err(err) => return Err(err),
        }
    }
    let listener = UnixListener::bind(&socket)?;
    super::protocol::restrict_to_owner(&socket)?;
    let pid_file = super::protocol::PidFile::new(std::process::id());
    if let Err(err) = super::protocol::write_pid_file(dir, &pid_file) {
        log::warn!("no se pudo escribir el pid-file: {err}");
    }
    log::info!("daemon escuchando en {}", socket.display());

    let mut registry = DaemonState::new();
    registry.hook_store = crate::memory::process_store();
    let state = Arc::new(Mutex::new(registry));
    let shutdown = Arc::new(Mutex::new(false));
    let scheduler = Arc::new(Mutex::new(RuntimeScheduler::new()));

    // Pump de salida + persistencia (T2): el daemon es el dueño del grid, así
    // que el checkpoint del scrollback lo escribe él.
    {
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let mut last_persist = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(50));
                let checkpoint_due = last_persist.elapsed() >= Duration::from_secs(2);
                let mut scrollback_targets = Vec::new();
                let mut incremental_targets = Vec::new();
                if let Ok(mut state) = state.lock() {
                    let events = state.pump_output();
                    if !events.is_empty() {
                        state.broadcast(&events);
                    }
                    if checkpoint_due {
                        scrollback_targets = state.take_dirty_scrollback_targets();
                    }
                    incremental_targets = if checkpoint_due {
                        state
                            .incremental_scrollback_targets()
                            .into_iter()
                            .filter(|target| {
                                state
                                    .session(target.session_id)
                                    .is_some_and(|session| session.retirement_requested)
                            })
                            .collect()
                    } else {
                        state.incremental_scrollback_targets()
                    };
                }
                if checkpoint_due {
                    // Nunca sostener el registro global durante export/fsync:
                    // bajo carga eso congelaba input, attach y resize juntos.
                    let (acknowledgements, failed) = persist_scrollback_targets(scrollback_targets);
                    if let Ok(mut state) = state.lock() {
                        for (session_id, written_bytes) in acknowledgements {
                            if let Some(session) = state.session_mut(session_id) {
                                session.acknowledge_persisted(written_bytes);
                            }
                        }
                        for session_id in failed {
                            if let Some(session) = state.session_mut(session_id) {
                                session.scrollback_dirty = true;
                            }
                        }
                    }
                    last_persist = Instant::now();
                }
                {
                    let (acknowledgements, rollover) =
                        persist_incremental_targets(incremental_targets);
                    if let Ok(mut state) = state.lock() {
                        for (session_id, written_bytes) in acknowledgements {
                            if let Some(session) = state.session_mut(session_id) {
                                session.acknowledge_persisted(written_bytes);
                            }
                        }
                        for session_id in rollover {
                            if let Some(session) = state.session_mut(session_id) {
                                session.scrollback_dirty = true;
                            }
                        }
                    }
                }
                if let Ok(mut state) = state.lock() {
                    state.finish_acknowledged_retirements();
                }
            }
        });
    }

    // Vigía del apagado por inactividad (T5).
    {
        let state = Arc::clone(&state);
        let shutdown = Arc::clone(&shutdown);
        let socket = socket.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            let asked = shutdown.lock().map(|flag| *flag).unwrap_or(false);
            if let Ok(state) = state.lock() {
                if state.should_shutdown_with_request(Instant::now(), asked) {
                    // Keep the registry locked through exit: a new Spawn
                    // cannot be registered after the final idle decision.
                    let _ = std::fs::remove_file(&socket);
                    std::process::exit(0);
                }
            }
        });
    }

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let state = Arc::clone(&state);
        let shutdown = Arc::clone(&shutdown);
        let scheduler = Arc::clone(&scheduler);
        let token = token.clone();
        std::thread::spawn(move || {
            serve_connection(stream, state, shutdown, scheduler, token);
        });
    }
    Ok(())
}

fn serve_connection(
    stream: UnixStream,
    state: Arc<Mutex<DaemonState>>,
    shutdown: Arc<Mutex<bool>>,
    scheduler: Arc<Mutex<RuntimeScheduler>>,
    token: String,
) {
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let Ok(disconnect) = stream.try_clone() else {
        return;
    };
    let _ = write_half.set_write_timeout(Some(Duration::from_secs(5)));
    // Respuestas síncronas y eventos comparten un único escritor serializado.
    // Dos clones de UnixStream escribiendo NDJSON a la vez pueden intercalar
    // bytes y producir líneas imposibles de decodificar.
    let writer = Arc::new(Mutex::new(write_half));
    let mut reader = BufReader::new(stream);
    let mut authenticated_client_id = None;
    let connection_id = Uuid::new_v4();
    let mut event_sender = None;

    loop {
        let line = match read_protocol_line(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(_) => {
                let _ = write_response(
                    &writer,
                    &Response::Error {
                        message: "línea del protocolo inválida o demasiado grande".to_owned(),
                    },
                );
                break;
            }
        };
        let Some(request) = decode_line::<Request>(&line) else {
            let _ = write_response(
                &writer,
                &Response::Error {
                    message: "mensaje ilegible".to_owned(),
                },
            );
            continue;
        };

        // Sin handshake válido no se atiende nada más.
        if authenticated_client_id.is_none() {
            match request {
                Request::Hello {
                    version,
                    token: provided,
                    client_id,
                } => {
                    if handshake_ok(version, &provided, &token) {
                        authenticated_client_id = Some(client_id);
                        // Canal de eventos: un hilo escritor los empuja a esta
                        // app sin bloquear el loop de pedidos.
                        let (event_tx, event_rx) =
                            std::sync::mpsc::sync_channel::<Response>(SUBSCRIBER_QUEUE_CAPACITY);
                        if let Ok(mut state) = state.lock() {
                            state.client_connected(client_id);
                        }
                        event_sender = Some(event_tx);
                        let event_writer = Arc::clone(&writer);
                        std::thread::spawn(move || {
                            while let Ok(event) = event_rx.recv() {
                                if write_response(&event_writer, &event).is_err() {
                                    // Close the read half too: framing failure must
                                    // not leave a timed-out request socket reusable.
                                    let stream = event_writer
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                                    let _ = stream.shutdown(std::net::Shutdown::Both);
                                    break;
                                }
                            }
                        });
                        let _ = write_response(
                            &writer,
                            &Response::Welcome {
                                version: PROTOCOL_VERSION,
                                birth_idempotent: true,
                                birth_cancel_safe: true,
                                // Never advertise ledger fencing when SQLite is unavailable.
                                // Killed is emitted only after child.wait, EOF and durable tail ACK.
                                hook_birth_authority: state
                                    .lock()
                                    .ok()
                                    .is_some_and(|state| state.hook_store.is_some()),
                            },
                        );
                    } else {
                        let _ = write_response(
                            &writer,
                            &Response::Error {
                                message: "handshake rechazado".to_owned(),
                            },
                        );
                        break;
                    }
                }
                _ => {
                    let _ = write_response(
                        &writer,
                        &Response::Error {
                            message: "falta el handshake".to_owned(),
                        },
                    );
                    break;
                }
            }
            continue;
        }

        let asked_shutdown = matches!(request, Request::ShutdownIfIdle);
        let attach_id = match &request {
            Request::Attach { id } => Some(*id),
            _ => None,
        };
        let mut response_queued = false;
        let response = match state.lock() {
            Ok(mut state) => {
                let response = handle_request_for_client(
                    &mut state,
                    request,
                    Some(&scheduler),
                    authenticated_client_id.unwrap_or_default(),
                );
                if matches!(response, Response::Attached { .. }) {
                    // Enqueue snapshot and subscribe under the same registry
                    // boundary. The sole connection writer performs all socket
                    // I/O later; a stalled client cannot block other sessions.
                    if let (Some(session_id), Some(sender)) = (attach_id, &event_sender) {
                        response_queued = sender.try_send(response.clone()).is_ok();
                        if response_queued {
                            state.subscribe(
                                connection_id,
                                session_id,
                                sender.clone(),
                                disconnect.try_clone().ok(),
                            );
                        }
                    }
                }
                response
            }
            Err(_) => Response::Error {
                message: "estado del daemon envenenado".to_owned(),
            },
        };
        if !response_queued
            && !event_sender
                .as_ref()
                .is_some_and(|sender| sender.try_send(response).is_ok())
        {
            let _ = disconnect.shutdown(std::net::Shutdown::Both);
            break;
        }

        if asked_shutdown {
            let idle = state
                .lock()
                .map(|state| state.session_ids().is_empty())
                .unwrap_or(false);
            if idle {
                if let Ok(mut flag) = shutdown.lock() {
                    *flag = true;
                }
                break;
            }
        }
    }

    if let Some(client_id) = authenticated_client_id {
        if let Ok(mut state) = state.lock() {
            state.unsubscribe(connection_id);
            state.client_disconnected(client_id, Instant::now());
        }
    }
}

/// Encode and validate completely before invoking the socket writer. No
/// oversized line or partial JSON prefix is ever presented to the transport.
fn with_encoded_response(
    response: &Response,
    line_limit: usize,
    write: impl FnOnce(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let line = encode_response_line_with_limit(response, line_limit)?;
    write(&line)
}

fn write_response(writer: &Arc<Mutex<UnixStream>>, response: &Response) -> std::io::Result<()> {
    let result = with_encoded_response(response, MAX_PROTOCOL_LINE_BYTES, |line| {
        let mut writer = writer
            .lock()
            .map_err(|_| std::io::Error::other("escritor del daemon envenenado"))?;
        writer.write_all(line)?;
        writer.flush()
    });
    if result.is_err() {
        // Every call path, including Welcome and synchronous errors, invalidates
        // the read half too. A failed stream cannot deliver a later stale reply.
        let stream = writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
    result
}

/// Path del directorio del daemon, con override por env para los tests de
/// proceso real (T6).
pub fn resolve_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("MI_TERMINAL_DAEMON_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    super::protocol::daemon_dir()
}

#[cfg(test)]
mod tests {
    use super::{
        clamp_snapshot, handle_request, DaemonState, ADOPTION_TIMEOUT, MAX_SNAPSHOT_BYTES,
        SUBSCRIBER_QUEUE_CAPACITY,
    };
    use crate::daemon::protocol::{Request, Response, WireSpec};
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    fn spawn_one(state: &mut DaemonState) -> Uuid {
        match handle_request(
            state,
            Request::Spawn {
                spec: WireSpec::default(),
                id: None,
            },
            None,
        ) {
            Response::Spawned { id } => id,
            other => panic!("esperaba Spawned, got {other:?}"),
        }
    }

    #[test]
    fn failed_pty_creation_returns_the_original_error_without_registering() {
        let mut state = DaemonState::new();
        let desired = Uuid::new_v4();
        let failure = std::io::Error::new(std::io::ErrorKind::NotFound, "injected missing shell");
        let result = state.spawn_with_pty_in_history_dir_using(
            WireSpec::default(),
            Some(desired),
            None,
            |id, spec, history| {
                assert_eq!(id, desired);
                assert_eq!(spec, &WireSpec::default());
                assert!(history.checkpoint.is_empty());
                assert!(history.frames.is_empty());
                Err(anyhow::Error::new(failure))
            },
        );
        let error = result.unwrap_err();
        let original = error.downcast_ref::<std::io::Error>().unwrap();
        assert_eq!(original.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(original.to_string(), "injected missing shell");
        assert!(state.session_ids().is_empty());
        assert!(state.session_claimants.is_empty());
        assert_eq!(state.subscriber_count(), 0);
    }

    #[test]
    fn failed_pty_creation_does_not_replace_a_colliding_desired_session() {
        let mut state = DaemonState::new();
        let existing = Uuid::new_v4();
        let owner = Uuid::new_v4();
        let existing_spec = WireSpec {
            title: "keep original".to_owned(),
            ..WireSpec::default()
        };
        state.insert_session(existing, existing_spec.clone());
        state
            .session_mut(existing)
            .unwrap()
            .push_output(b"retained output");
        state
            .session_claimants
            .entry(existing)
            .or_default()
            .insert(owner);
        state.priority_session = Some(existing);
        let result = state.spawn_with_pty_in_history_dir_using(
            WireSpec::default(),
            Some(existing),
            None,
            |_, _, _| panic!("a UUID collision must be rejected before the PTY factory"),
        );
        assert!(result.unwrap_err().to_string().contains("UUID solicitado"));
        assert_eq!(state.session_ids(), vec![existing]);
        let session = state.session(existing).unwrap();
        assert_eq!(session.spec, existing_spec);
        assert_eq!(session.snapshot, b"retained output");
        assert_eq!(session.seq, 1);
        assert!(session.alive);
        assert_eq!(state.priority_session, Some(existing));
        assert_eq!(state.session_claimants.len(), 1);
        assert!(state.session_claimants[&existing].contains(&owner));
        let response = super::handle_request_for_client(
            &mut state,
            Request::Attach { id: existing },
            None,
            owner,
        );
        assert!(matches!(
            response,
            Response::Attached { id, snapshot, seq: 1, alive: true,
            ..
        }
                if id == existing && snapshot == b"retained output"
        ));
    }

    #[test]
    fn successful_pty_creation_registers_the_requested_identity_once() {
        let mut state = DaemonState::new();
        let desired = Uuid::new_v4();
        let spec = WireSpec {
            title: "fake terminal".to_owned(),
            ..WireSpec::default()
        };
        let result = state.spawn_with_pty_in_history_dir_using(
            spec.clone(),
            Some(desired),
            None,
            |id, received, history| {
                assert_eq!(id, desired);
                assert_eq!(received, &spec);
                assert!(history.checkpoint.is_empty());
                assert!(history.frames.is_empty());
                Ok(crate::terminal::pty::PtyHandle::for_persistence_tests(
                    b"fake ready",
                ))
            },
        );
        assert_eq!(result.unwrap(), desired);
        assert_eq!(state.session_ids(), vec![desired]);
        let session = state.session(desired).unwrap();
        assert_eq!(session.spec, spec);
        assert!(session.alive);
        assert!(session.handle.is_some());
        assert!(state.session_claimants.is_empty());
        let response = handle_request(&mut state, Request::Attach { id: desired }, None);
        assert!(matches!(
            response,
            Response::Attached { id, snapshot, alive: true, .. }
                if id == desired && snapshot.windows(b"fake ready".len())
                    .any(|part| part == b"fake ready")
        ));
        assert_eq!(state.session_ids(), vec![desired]);
    }

    #[test]
    fn pending_idle_shutdown_waits_for_a_new_session_then_remains_requested() {
        let mut state = DaemonState::new();
        let first_client = Uuid::new_v4();
        state.client_connected(first_client);
        let now = Instant::now();
        assert!(state.should_shutdown_with_request(now, true));
        let second_client = Uuid::new_v4();
        state.client_connected(second_client);
        let response = super::handle_request_for_client(
            &mut state,
            Request::Spawn {
                spec: WireSpec::default(),
                id: None,
            },
            None,
            second_client,
        );
        let Response::Spawned { id } = response else {
            panic!("expected registered fake session");
        };
        assert!(!state.should_shutdown_with_request(now + ADOPTION_TIMEOUT, true));
        assert!(state.session(id).unwrap().alive);
        assert!(state.kill(id));
        assert!(state.should_shutdown_with_request(now + ADOPTION_TIMEOUT, true));
    }

    #[test]
    fn ordinary_idle_shutdown_keeps_connection_and_adoption_guards() {
        let mut state = DaemonState::new();
        let client = Uuid::new_v4();
        state.client_connected(client);
        let now = Instant::now();
        assert!(!state.should_shutdown_with_request(now + ADOPTION_TIMEOUT * 2, false));
        state.client_disconnected(client, now);
        assert!(!state.should_shutdown_with_request(now, false));
        assert!(state.should_shutdown_with_request(now + ADOPTION_TIMEOUT, false));
        let session = spawn_one(&mut state);
        assert!(!state.should_shutdown_with_request(now + ADOPTION_TIMEOUT * 2, false));
        assert!(!state.should_shutdown_with_request(now + ADOPTION_TIMEOUT * 2, true));
        assert!(state.session(session).unwrap().alive);
    }

    #[test]
    fn spawning_registers_the_session() {
        let mut state = DaemonState::new();
        let id = spawn_one(&mut state);
        assert_eq!(state.session_ids(), vec![id]);
    }

    #[test]
    fn priority_session_delivers_its_entire_final_burst_before_exit() {
        use std::sync::{Arc, Mutex};
        let mut state = DaemonState::new();
        let scheduler = Arc::new(Mutex::new(crate::runtime::RuntimeScheduler::new()));
        let id = state
            .spawn_with_pty(WireSpec::default(), &scheduler, None)
            .unwrap();
        let handle = state.session(id).unwrap().handle.clone().expect("real PTY");
        // Shell startup may prepend bracketed-paste control bytes to the first
        // output line. Delimit the burst so all 3000 markers remain whole lines.
        let marker = format!("TC_FINAL_{}", Uuid::new_v4().simple());
        let command = format!(
            "sh -c 'printf \"\\n\"; i=0; while [ $i -lt 3000 ]; do printf \"{marker}\\n\"; i=$((i+1)); done'; exit\r"
        );
        handle.lock().unwrap().write_all(command.as_bytes());
        let deadline = Instant::now() + Duration::from_secs(20);
        while !handle.lock().unwrap().output_finished() {
            assert!(Instant::now() < deadline, "fixture reader did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            handle.lock().unwrap().pending_log_snapshot().len() > super::INTERACTIVE_BYTE_BUDGET
        );
        state.priority_session = Some(id);
        let events = state.pump_output();
        let exit = events
            .iter()
            .position(|event| matches!(event, Response::Exit { id: exited } if *exited == id))
            .expect("exit event");
        let mut output = Vec::new();
        for event in &events[..exit] {
            if let Response::Output {
                id: source, data, ..
            } = event
            {
                if *source == id {
                    output.extend_from_slice(data);
                }
            }
        }
        let text = String::from_utf8_lossy(&output);
        assert!(
            text.lines().filter(|line| line.trim() == marker).count() >= 3000,
            "Exit arrived before the complete final output"
        );
        assert!(handle.lock().unwrap().pending_log_snapshot().is_empty());
    }

    #[test]
    fn early_exit_signal_does_not_discard_later_pty_output() {
        use std::sync::{Arc, Mutex};
        let mut state = DaemonState::new();
        let scheduler = Arc::new(Mutex::new(crate::runtime::RuntimeScheduler::new()));
        let id = state
            .spawn_with_pty(WireSpec::default(), &scheduler, None)
            .unwrap();
        let handle = state.session(id).unwrap().handle.clone().expect("real PTY");
        state.priority_session = Some(id);
        // Model a process-exit notification while the reader is still active.
        handle
            .lock()
            .unwrap()
            .alive
            .store(false, std::sync::atomic::Ordering::Release);
        assert!(!handle.lock().unwrap().output_finished());
        assert!(!state
            .pump_output()
            .iter()
            .any(|event| matches!(event, Response::Exit { .. })));
        let marker = format!("TC_LATE_{}", Uuid::new_v4().simple());
        handle
            .lock()
            .unwrap()
            .write_all(format!("printf '\\n{marker}\\n'; exit\r").as_bytes());
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut output = Vec::new();
        let mut exited = false;
        while !exited {
            for event in state.pump_output() {
                match event {
                    Response::Output { id: got, data, .. } if got == id => {
                        assert!(!exited, "output delivered after Exit");
                        output.extend_from_slice(&data);
                    }
                    Response::Exit { id: got } if got == id => exited = true,
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline, "PTY reader did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(String::from_utf8_lossy(&output)
            .lines()
            .any(|line| line.trim() == marker));
        assert!(handle.lock().unwrap().pending_log_snapshot().is_empty());
        assert!(!state
            .pump_output()
            .iter()
            .any(|event| matches!(event, Response::Output { .. } | Response::Exit { .. })));
    }

    #[test]
    fn attach_reports_process_exit_and_kill_notifies_subscribers() {
        let mut state = DaemonState::new();
        let id = spawn_one(&mut state);
        state.session_mut(id).unwrap().alive = false;
        assert!(matches!(
            handle_request(&mut state, Request::Attach { id }, None),
            Response::Attached { alive: false, .. }
        ));
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        state.subscribe(Uuid::new_v4(), id, tx, None);
        assert!(state.kill(id));
        assert_eq!(rx.try_recv().unwrap(), Response::Exit { id });
    }

    #[test]
    fn attaching_returns_the_snapshot_and_the_current_seq() {
        let mut state = DaemonState::new();
        let id = spawn_one(&mut state);
        let seq = state.session_mut(id).unwrap().push_output(b"hola\n");
        state.session_mut(id).unwrap().push_output(b"chau\n");

        match handle_request(&mut state, Request::Attach { id }, None) {
            Response::Attached {
                snapshot, seq: at, ..
            } => {
                assert_eq!(snapshot, b"hola\nchau\n");
                assert_eq!(at, seq + 1, "el seq avanza con cada salida");
            }
            other => panic!("esperaba Attached, got {other:?}"),
        }
    }

    #[test]
    fn the_seq_is_monotonic_so_reattach_can_dedup() {
        let mut state = DaemonState::new();
        let id = spawn_one(&mut state);
        let session = state.session_mut(id).unwrap();
        let first = session.push_output(b"a");
        let second = session.push_output(b"b");
        let third = session.push_output(b"c");
        assert!(first < second && second < third, "{first} {second} {third}");
    }

    #[test]
    fn output_marks_only_that_session_scrollback_dirty() {
        let mut state = DaemonState::new();
        let dirty = spawn_one(&mut state);
        let idle = spawn_one(&mut state);

        state.session_mut(dirty).unwrap().push_output(b"nuevo");

        assert!(state.session(dirty).unwrap().scrollback_dirty);
        assert!(!state.session(idle).unwrap().scrollback_dirty);
    }

    #[test]
    fn durable_ack_removes_only_the_captured_prefix() {
        let mut session = super::DaemonSession::new(WireSpec::default());
        session.push_output(b"primero");
        let captured = session.persist_pending.len();
        session.push_output(b"despues");

        session.acknowledge_persisted(captured);

        let frames = super::decode_frames(&session.persist_pending);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload, b"despues");
    }

    #[test]
    fn durable_ack_resets_the_batch_sequence_only_when_empty() {
        let mut session = super::DaemonSession::new(WireSpec::default());
        session.push_output(b"one");
        let first_batch = session.persist_pending.len();
        session.push_output(b"two");

        session.acknowledge_persisted(first_batch);
        assert_eq!(session.persist_seq, 2, "a partial batch keeps its sequence");

        session.acknowledge_persisted(usize::MAX);
        assert_eq!(session.persist_seq, 0, "an empty batch can restart safely");
    }

    #[test]
    fn exhausted_incremental_sequence_does_not_emit_a_duplicate_frame() {
        let mut session = super::DaemonSession::new(WireSpec::default());
        session.persist_seq = u64::MAX;

        session.record_persist_frame(
            crate::state::scrollback_log::FrameKind::Output,
            b"checkpoint only",
        );

        assert!(session.persist_pending.is_empty());
        assert!(session.scrollback_dirty);
        assert_eq!(session.persist_seq, u64::MAX);
    }

    #[test]
    fn incremental_persistence_continues_from_the_existing_durable_seq() {
        use crate::state::scrollback_log::{encode_frame, read_frames, FrameKind};

        let dir = std::env::temp_dir().join(format!("tc-inc-seq-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("MI_TERMINAL_SCROLLBACK_DIR", &dir);
        let panel_id = uuid::Uuid::new_v4();
        let leaf_id = uuid::Uuid::new_v4();
        let log_path = dir.join(
            crate::state::scrollback_store::scrollback_leaf_log_file_name(panel_id, Some(leaf_id)),
        );

        // Simular un checkpoint + log durable preexistente con frames hasta seq 5.
        crate::state::scrollback_store::save_leaf_scrollback_versioned(
            &dir,
            panel_id,
            Some(leaf_id),
            1,
            "checkpoint viejo\n",
        )
        .unwrap();
        crate::state::scrollback_log::reset_log(&log_path, 1).unwrap();
        for seq in 1..=5 {
            crate::state::scrollback_log::append_frames(
                &log_path,
                &encode_frame(seq, FrameKind::Output, format!("old{seq}").as_bytes()),
            )
            .unwrap();
        }

        // El daemon arranca fresco: su persist_seq empieza en 0, así que los
        // frames nuevos tendrían seq 1, 2 sin el renumber.
        let target = super::IncrementalTarget {
            alternate: false,
            cols: 80,
            rows: 24,
            session_id: uuid::Uuid::new_v4(),
            panel_id,
            leaf_id: Some(leaf_id),
            frames: encode_frame(1, FrameKind::Output, b"new1"),
        };
        let (acks, _) = super::persist_incremental_targets(vec![target]);
        assert_eq!(acks.len(), 1);

        let (_, frames) = read_frames(&std::fs::read(&log_path).unwrap()).unwrap();
        let seqs: Vec<u64> = frames.iter().map(|f| f.seq).collect();
        assert_eq!(
            seqs,
            vec![1, 2, 3, 4, 5, 6],
            "la secuencia debe continuar sin gap"
        );
        assert_eq!(frames.last().unwrap().payload, b"new1");

        std::env::remove_var("MI_TERMINAL_SCROLLBACK_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn strict_history_fixture(label: &str) -> (std::path::PathBuf, Uuid, Uuid, Uuid) {
        use crate::state::{scrollback_log, scrollback_store};
        let dir = std::env::temp_dir().join(format!("tc-daemon-{label}-{}", Uuid::new_v4()));
        let panel = Uuid::new_v4();
        let leaf = Uuid::new_v4();
        let session = Uuid::new_v4();
        // Legacy checkpoints require the sidecar. All three artifacts must
        // be read successfully before either the new PTY or a writer starts.
        scrollback_store::save_leaf_scrollback(&dir, panel, Some(leaf), "CHECKPOINT\n").unwrap();
        std::fs::write(
            dir.join(scrollback_store::scrollback_leaf_gen_file_name(
                panel,
                Some(leaf),
            )),
            3_u32.to_le_bytes(),
        )
        .unwrap();
        let log = dir.join(scrollback_store::scrollback_leaf_log_file_name(
            panel,
            Some(leaf),
        ));
        scrollback_log::reset_log(&log, 3).unwrap();
        scrollback_log::append_frames(
            &log,
            &scrollback_log::encode_frame(1, scrollback_log::FrameKind::Output, b"OLD\r\n"),
        )
        .unwrap();
        (dir, panel, leaf, session)
    }

    fn strict_history_paths(
        dir: &std::path::Path,
        panel: Uuid,
        leaf: Uuid,
    ) -> Vec<std::path::PathBuf> {
        use crate::state::scrollback_store;
        [
            scrollback_store::scrollback_leaf_file_name(panel, Some(leaf)),
            scrollback_store::scrollback_leaf_log_file_name(panel, Some(leaf)),
            scrollback_store::scrollback_leaf_gen_file_name(panel, Some(leaf)),
        ]
        .into_iter()
        .map(|name| dir.join(name))
        .collect()
    }

    fn strict_incremental_target(
        panel: Uuid,
        leaf: Uuid,
        session: Uuid,
        text: &[u8],
    ) -> super::IncrementalTarget {
        super::IncrementalTarget {
            session_id: session,
            panel_id: panel,
            leaf_id: Some(leaf),
            frames: crate::state::scrollback_log::encode_frame(
                1,
                crate::state::scrollback_log::FrameKind::Output,
                text,
            ),
            alternate: false,
            cols: 80,
            rows: 24,
        }
    }

    fn assert_daemon_rejects_unread_history(
        dir: &std::path::Path,
        panel: Uuid,
        leaf: Uuid,
        session: Uuid,
    ) {
        let scheduler = std::sync::Arc::new(std::sync::Mutex::new(
            crate::runtime::RuntimeScheduler::new(),
        ));
        let mut state = DaemonState::new();
        let result = state.spawn_with_pty_in_history_dir(
            WireSpec {
                panel_id: Some(panel),
                leaf_id: Some(leaf),
                ..WireSpec::default()
            },
            &scheduler,
            Some(session),
            Some(dir),
        );
        assert!(
            result.is_err(),
            "the real cold-spawn path must fail before opening its PTY"
        );
        assert!(state.session_ids().is_empty());
        assert!(state.session_claimants.is_empty());

        let (acks, rollover) = super::persist_incremental_targets_in_dir(
            dir,
            vec![strict_incremental_target(panel, leaf, session, b"NEW\r\n")],
        );
        assert!(
            acks.is_empty(),
            "unread output cannot authorize an incremental ACK"
        );
        assert!(rollover.is_empty());
        let (acks, failed) = super::persist_scrollback_targets_in_dir(
            dir,
            vec![super::ScrollbackTarget {
                session_id: session,
                panel_id: panel,
                leaf_id: Some(leaf),
                text: "CHECKPOINT\nOLD\nNEW\n".to_owned(),
                pending_bytes: 20,
            }],
        );
        assert!(
            acks.is_empty(),
            "unread history cannot authorize a full ACK"
        );
        assert_eq!(failed, vec![session]);
        let target = strict_incremental_target(panel, leaf, session, b"NEW\r\n");
        assert!(super::compact_alternate_log(dir, &target, 3).is_err());
    }

    #[test]
    fn unreadable_artifacts_block_cold_spawn_full_incremental_and_compaction() {
        use std::os::unix::fs::PermissionsExt;
        struct ReadBlock(std::path::PathBuf);
        impl Drop for ReadBlock {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o600));
            }
        }
        for blocked in 0..3 {
            let (dir, panel, leaf, session) = strict_history_fixture("unreadable");
            let paths = strict_history_paths(&dir, panel, leaf);
            let before = paths
                .iter()
                .map(|path| std::fs::read(path).unwrap())
                .collect::<Vec<_>>();
            // The owner may write and the directory allows rename, so the old
            // code could replace these artifacts despite being unable to read.
            std::fs::set_permissions(&paths[blocked], std::fs::Permissions::from_mode(0o200))
                .unwrap();
            let guard = ReadBlock(paths[blocked].clone());
            assert_eq!(
                std::fs::read(&guard.0).unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
            assert_daemon_rejects_unread_history(&dir, panel, leaf, session);
            drop(guard);
            for (path, bytes) in paths.iter().zip(&before) {
                assert_eq!(
                    std::fs::read(path).unwrap(),
                    *bytes,
                    "no unread artifact may be replaced"
                );
            }
            let target = strict_incremental_target(panel, leaf, session, b"NEW\r\n");
            let expected_bytes = target.frames.len();
            let (acks, rollover) = super::persist_incremental_targets_in_dir(&dir, vec![target]);
            assert_eq!(acks, vec![(session, expected_bytes)]);
            assert!(rollover.is_empty());
            let loaded =
                crate::state::scrollback_store::try_load_leaf_session(&dir, panel, Some(leaf))
                    .unwrap();
            assert_eq!(loaded.checkpoint, b"CHECKPOINT\r\n");
            assert_eq!(
                loaded
                    .frames
                    .iter()
                    .map(|frame| (frame.seq, frame.payload.as_slice()))
                    .collect::<Vec<_>>(),
                vec![(1, b"OLD\r\n".as_slice()), (2, b"NEW\r\n".as_slice())]
            );
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn invalid_artifacts_are_retained_without_a_pty_or_persistence_ack() {
        use crate::state::{scrollback_log, scrollback_store};
        for invalid in 0..4 {
            let (dir, panel, leaf, session) = strict_history_fixture("invalid");
            let paths = strict_history_paths(&dir, panel, leaf);
            match invalid {
                0 => std::fs::write(&paths[0], b"TC-SCROLLBACK\x00\x02").unwrap(),
                1 => std::fs::write(&paths[1], b"invalid log header").unwrap(),
                2 => {
                    let mut gap = scrollback_log::encode_header(3);
                    gap.extend_from_slice(&scrollback_log::encode_frame(
                        1,
                        scrollback_log::FrameKind::Output,
                        b"ONE",
                    ));
                    gap.extend_from_slice(&scrollback_log::encode_frame(
                        3,
                        scrollback_log::FrameKind::Output,
                        b"THREE",
                    ));
                    std::fs::write(&paths[1], gap).unwrap();
                }
                3 => std::fs::write(&paths[2], [3_u8, 0]).unwrap(),
                _ => unreachable!(),
            }
            let before = paths
                .iter()
                .map(|path| std::fs::read(path).unwrap())
                .collect::<Vec<_>>();
            assert!(scrollback_store::try_load_leaf_session(&dir, panel, Some(leaf)).is_err());
            assert_daemon_rejects_unread_history(&dir, panel, leaf, session);
            for (path, bytes) in paths.iter().zip(&before) {
                assert_eq!(std::fs::read(path).unwrap(), *bytes);
            }
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn absent_history_and_valid_stale_logs_allow_incremental_progress() {
        use crate::state::{scrollback_log, scrollback_store};
        for stale in [false, true] {
            let dir =
                std::env::temp_dir().join(format!("tc-daemon-missing-stale-{}", Uuid::new_v4()));
            let panel = Uuid::new_v4();
            let leaf = Uuid::new_v4();
            let session = Uuid::new_v4();
            if stale {
                scrollback_store::save_leaf_scrollback_versioned(
                    &dir,
                    panel,
                    Some(leaf),
                    5,
                    "PRIMARY\n",
                )
                .unwrap();
                let log = dir.join(scrollback_store::scrollback_leaf_log_file_name(
                    panel,
                    Some(leaf),
                ));
                scrollback_log::reset_log(&log, 4).unwrap();
                scrollback_log::append_frames(
                    &log,
                    &scrollback_log::encode_frame(7, scrollback_log::FrameKind::Output, b"STALE"),
                )
                .unwrap();
            } else {
                assert!(
                    scrollback_store::try_load_leaf_session(&dir, panel, Some(leaf))
                        .unwrap()
                        .frames
                        .is_empty()
                );
            }
            let target = strict_incremental_target(panel, leaf, session, b"NEW");
            let expected = target.frames.len();
            let (acks, rollover) = super::persist_incremental_targets_in_dir(&dir, vec![target]);
            assert_eq!(acks, vec![(session, expected)]);
            assert!(rollover.is_empty());
            let history = scrollback_store::try_load_leaf_session(&dir, panel, Some(leaf)).unwrap();
            assert_eq!(history.generation, if stale { 5 } else { 0 });
            assert_eq!(history.frames.len(), 1);
            assert_eq!(history.frames[0].seq, 1);
            assert_eq!(history.frames[0].payload, b"NEW");
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn daemon_append_repairs_truncated_tail_and_retries_at_exact_next_sequence() {
        use crate::state::{scrollback_log, scrollback_store};
        let (dir, panel, leaf, session) = strict_history_fixture("truncated-tail");
        let log = dir.join(scrollback_store::scrollback_leaf_log_file_name(
            panel,
            Some(leaf),
        ));
        let mut bytes = std::fs::read(&log).unwrap();
        let truncated =
            scrollback_log::encode_frame(2, scrollback_log::FrameKind::Output, b"INCOMPLETE");
        bytes.extend_from_slice(&truncated[..truncated.len() - 2]);
        std::fs::write(&log, bytes).unwrap();
        for (index, payload) in [b"NEW".as_slice(), b"THIRD".as_slice()]
            .into_iter()
            .enumerate()
        {
            let target = strict_incremental_target(panel, leaf, session, payload);
            let expected = target.frames.len();
            let (acks, rollover) = super::persist_incremental_targets_in_dir(&dir, vec![target]);
            assert_eq!(acks, vec![(session, expected)]);
            assert!(rollover.is_empty());
            let history = scrollback_store::try_load_leaf_session(&dir, panel, Some(leaf)).unwrap();
            assert_eq!(history.frames.len(), index + 2);
            assert_eq!(history.frames.last().unwrap().seq, (index + 2) as u64);
            assert_eq!(history.frames.last().unwrap().payload, payload);
        }
        let history = scrollback_store::try_load_leaf_session(&dir, panel, Some(leaf)).unwrap();
        assert_eq!(
            history
                .frames
                .iter()
                .map(|frame| frame.payload.as_slice())
                .collect::<Vec<_>>(),
            vec![
                b"OLD\r\n".as_slice(),
                b"NEW".as_slice(),
                b"THIRD".as_slice()
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn full_daemon_checkpoint_excludes_current_and_retained_stale_generations() {
        use crate::state::{scrollback_log, scrollback_store};
        for (generation, retained_log, expected) in
            [(3, 4, 5), (u32::MAX, u32::MAX, 0), (u32::MAX, 0, 1)]
        {
            let dir =
                std::env::temp_dir().join(format!("tc-daemon-next-generation-{}", Uuid::new_v4()));
            let panel = Uuid::new_v4();
            let leaf = Uuid::new_v4();
            let session = Uuid::new_v4();
            scrollback_store::save_leaf_scrollback_versioned(
                &dir,
                panel,
                Some(leaf),
                generation,
                "OLD\n",
            )
            .unwrap();
            let log = dir.join(scrollback_store::scrollback_leaf_log_file_name(
                panel,
                Some(leaf),
            ));
            scrollback_log::reset_log(&log, retained_log).unwrap();
            scrollback_log::append_frames(
                &log,
                &scrollback_log::encode_frame(1, scrollback_log::FrameKind::Output, b"RETAINED"),
            )
            .unwrap();
            let (acks, failed) = super::persist_scrollback_targets_in_dir(
                &dir,
                vec![super::ScrollbackTarget {
                    session_id: session,
                    panel_id: panel,
                    leaf_id: Some(leaf),
                    text: "OLD\nNEW\n".to_owned(),
                    pending_bytes: 17,
                }],
            );
            assert_eq!(acks, vec![(session, 17)]);
            assert!(failed.is_empty());
            let history = scrollback_store::try_load_leaf_session(&dir, panel, Some(leaf)).unwrap();
            assert_eq!(history.generation, expected);
            assert_ne!(history.generation, generation);
            assert_ne!(history.generation, retained_log);
            assert!(history.frames.is_empty());
            assert_eq!(history.checkpoint, b"OLD\r\nNEW\r\n");
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn attaching_an_unknown_session_is_an_error_not_a_panic() {
        let mut state = DaemonState::new();
        match handle_request(&mut state, Request::Attach { id: Uuid::new_v4() }, None) {
            Response::Error { message } => assert!(message.contains("desconocida")),
            other => panic!("esperaba Error, got {other:?}"),
        }
    }

    #[test]
    fn killing_removes_the_session() {
        let mut state = DaemonState::new();
        let id = spawn_one(&mut state);
        assert!(matches!(
            handle_request(&mut state, Request::Kill { id }, None),
            Response::Killed { .. }
        ));
        assert!(state.session_ids().is_empty());
        // Matarla de nuevo es un error, no un panic.
        assert!(matches!(
            handle_request(&mut state, Request::Kill { id }, None),
            Response::Error { .. }
        ));
    }

    #[test]
    fn alternate_log_compaction_keeps_primary_and_atomic_screen_mode() {
        use crate::state::{scrollback_log, scrollback_store};
        let dir = std::env::temp_dir().join(format!("tui-compact-{}", Uuid::new_v4()));
        let panel_id = Uuid::new_v4();
        scrollback_store::save_leaf_scrollback_versioned(&dir, panel_id, None, 3, "PRIMARY\n")
            .unwrap();
        let log = dir.join(scrollback_store::scrollback_leaf_log_file_name(
            panel_id, None,
        ));
        scrollback_log::reset_log(&log, 3).unwrap();
        scrollback_log::append_frames(
            &log,
            &scrollback_log::encode_frame(
                1,
                scrollback_log::FrameKind::Output,
                b"TAIL\r\n\x1b[?1049hTUI",
            ),
        )
        .unwrap();
        let target = super::IncrementalTarget {
            session_id: Uuid::new_v4(),
            panel_id,
            leaf_id: None,
            frames: Vec::new(),
            alternate: true,
            cols: 80,
            rows: 24,
        };
        super::compact_alternate_log(&dir, &target, 3).unwrap();
        let (generation, text) =
            scrollback_store::load_leaf_scrollback_checkpoint(&dir, panel_id, None).unwrap();
        assert_eq!(generation, Some(4));
        assert!(text.contains("PRIMARY"));
        assert!(text.contains("TAIL"));
        assert!(!text.contains("TUI"));
        assert!(text.contains("\x1b[?1049h"));
        let (log_generation, frames) =
            scrollback_log::read_frames(&std::fs::read(log).unwrap()).unwrap();
        assert_eq!(log_generation, 4);
        assert!(frames.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn alternate_compaction_keeps_the_saved_primary_cursor_for_later_output() {
        use crate::state::{scrollback_log, scrollback_store};
        use alacritty_terminal::term::{test::TermSize, Config, Term};
        use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
        let dir = std::env::temp_dir().join(format!("tui-cursor-{}", Uuid::new_v4()));
        let panel = Uuid::new_v4();
        scrollback_store::save_leaf_scrollback_versioned(&dir, panel, None, 3, "").unwrap();
        let log = dir.join(scrollback_store::scrollback_leaf_log_file_name(panel, None));
        scrollback_log::reset_log(&log, 3).unwrap();
        scrollback_log::append_frames(
            &log,
            &scrollback_log::encode_frame(
                1,
                scrollback_log::FrameKind::Output,
                b"ABC\rX\x1b[?1049hTUI",
            ),
        )
        .unwrap();
        let target = super::IncrementalTarget {
            session_id: Uuid::new_v4(),
            panel_id: panel,
            leaf_id: None,
            frames: Vec::new(),
            alternate: true,
            cols: 80,
            rows: 24,
        };
        super::compact_alternate_log(&dir, &target, 3).unwrap();
        // The app leaves the TUI and overwrites the second primary character,
        // then the daemon crashes before another full checkpoint is taken.
        scrollback_log::append_frames(
            &log,
            &scrollback_log::encode_frame(1, scrollback_log::FrameKind::Output, b"\x1b[?1049lY"),
        )
        .unwrap();
        let (checkpoint, frames) = scrollback_store::load_leaf_session(&dir, panel, None);
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut term = Term::new(
            Config::default(),
            &TermSize::new(80, 24),
            crate::terminal::pty::EventProxy::new(tx),
        );
        let mut parser = Processor::<StdSyncHandler>::new();
        parser.advance(&mut term, &checkpoint);
        for frame in frames {
            parser.advance(&mut term, &frame.payload);
        }
        assert_eq!(
            crate::terminal::export::scrollback_to_text(&term).trim(),
            "XYC"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resize_updates_the_stored_geometry() {
        let mut state = DaemonState::new();
        let id = spawn_one(&mut state);
        handle_request(
            &mut state,
            Request::Resize {
                id,
                cols: 200,
                rows: 60,
            },
            None,
        );
        let session = state.session(id).unwrap();
        assert_eq!((session.spec.cols, session.spec.rows), (200, 60));
    }

    #[test]
    fn focus_is_forwarded_to_the_daemon_scheduler() {
        let mut state = DaemonState::new();
        let focused = Uuid::new_v4();
        let background = Uuid::new_v4();
        let scheduler = std::sync::Arc::new(std::sync::Mutex::new(
            crate::runtime::RuntimeScheduler::new(),
        ));
        let response = handle_request(
            &mut state,
            Request::SetPriority { id: Some(focused) },
            Some(&scheduler),
        );
        assert!(matches!(response, Response::Sessions { .. }));
        let scheduler = scheduler.lock().unwrap();
        assert!(scheduler.reader_delay(focused).is_zero());
        assert!(!scheduler.reader_delay(background).is_zero());
        // El estado del daemon también registra la prioridad para pump_output.
        assert_eq!(state.priority_session, Some(focused));
    }

    #[test]
    fn pump_order_puts_the_priority_session_first() {
        let mut state = DaemonState::new();
        let background_a = spawn_one(&mut state);
        let focused = spawn_one(&mut state);
        let background_b = spawn_one(&mut state);

        let ordered = super::pump_order(&state.sessions, Some(focused));
        assert_eq!(ordered.first(), Some(&focused));
        assert_eq!(ordered.len(), 3);
        assert!(ordered.contains(&background_a));
        assert!(ordered.contains(&background_b));

        // Sin prioridad, el orden es alfabético por UUID.
        let ordered = super::pump_order(&state.sessions, None);
        let mut expected = vec![background_a, focused, background_b];
        expected.sort_by_key(Uuid::as_u128);
        assert_eq!(ordered, expected);
    }

    #[test]
    fn killing_the_priority_session_clears_it() {
        let mut state = DaemonState::new();
        let focused = spawn_one(&mut state);
        state.priority_session = Some(focused);

        state.kill(focused);
        assert_eq!(state.priority_session, None);
    }

    #[test]
    fn reconcile_clears_priority_if_the_session_was_killed() {
        let mut state = DaemonState::new();
        let focused = spawn_one(&mut state);
        let other = spawn_one(&mut state);
        state.priority_session = Some(focused);

        // The default Spawn fixture belongs to the nil client. A foreign
        // client cannot retire its session or change that session's priority.
        state.reconcile_live(Uuid::new_v4(), &[other]);
        assert_eq!(state.priority_session, Some(focused));
        assert!(state.session(focused).is_some());
        state.reconcile_live(Uuid::nil(), &[other]);
        assert_eq!(state.priority_session, None);
    }

    #[test]
    fn reconcile_kills_only_the_orphans() {
        let mut state = DaemonState::new();
        let keep = spawn_one(&mut state);
        let orphan = spawn_one(&mut state);
        match handle_request(&mut state, Request::ReconcileLive { ids: vec![keep] }, None) {
            Response::Reconciled { killed } => assert_eq!(killed, vec![orphan]),
            other => panic!("esperaba Reconciled, got {other:?}"),
        }
        assert_eq!(state.session_ids(), vec![keep]);
    }

    #[test]
    fn reconcile_with_everything_live_kills_nothing() {
        let mut state = DaemonState::new();
        let a = spawn_one(&mut state);
        let b = spawn_one(&mut state);
        let mut live = vec![a, b];
        live.sort_by_key(Uuid::as_u128);
        match handle_request(
            &mut state,
            Request::ReconcileLive { ids: live.clone() },
            None,
        ) {
            Response::Reconciled { killed } => assert!(killed.is_empty()),
            other => panic!("got {other:?}"),
        }
        assert_eq!(state.session_ids(), live);
    }

    #[test]
    fn reconcile_never_kills_sessions_owned_by_another_live_client() {
        let mut state = DaemonState::new();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        state.client_connected(first);
        state.client_connected(second);
        let first_session = match super::handle_request_for_client(
            &mut state,
            Request::Spawn {
                spec: WireSpec::default(),
                id: None,
            },
            None,
            first,
        ) {
            Response::Spawned { id } => id,
            other => panic!("spawn inesperado: {other:?}"),
        };
        let second_session = match super::handle_request_for_client(
            &mut state,
            Request::Spawn {
                spec: WireSpec::default(),
                id: None,
            },
            None,
            second,
        ) {
            Response::Spawned { id } => id,
            other => panic!("spawn inesperado: {other:?}"),
        };

        let response = super::handle_request_for_client(
            &mut state,
            Request::ReconcileLive {
                ids: vec![second_session],
            },
            None,
            second,
        );
        assert!(matches!(
            response,
            Response::Reconciled { ref killed } if killed.is_empty()
        ));
        assert!(state.session(first_session).is_some());
        assert!(state.session(second_session).is_some());
    }

    #[test]
    fn attaching_from_a_second_live_client_does_not_steal_ownership() {
        let mut state = DaemonState::new();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        state.client_connected(first);
        state.client_connected(second);
        let session = match super::handle_request_for_client(
            &mut state,
            Request::Spawn {
                spec: WireSpec::default(),
                id: None,
            },
            None,
            first,
        ) {
            Response::Spawned { id } => id,
            other => panic!("spawn inesperado: {other:?}"),
        };
        assert!(matches!(
            super::handle_request_for_client(
                &mut state,
                Request::Attach { id: session },
                None,
                second,
            ),
            Response::Attached { .. }
        ));

        let second_reconcile = super::handle_request_for_client(
            &mut state,
            Request::ReconcileLive { ids: Vec::new() },
            None,
            second,
        );
        assert!(matches!(
            second_reconcile,
            Response::Reconciled { ref killed } if killed.is_empty()
        ));
        assert!(
            state.session(session).is_some(),
            "el attach de la segunda app no puede robar el claim de la primera"
        );

        let first_reconcile = super::handle_request_for_client(
            &mut state,
            Request::ReconcileLive { ids: Vec::new() },
            None,
            first,
        );
        assert!(matches!(
            first_reconcile,
            Response::Reconciled { ref killed } if killed == &[session]
        ));
    }

    #[test]
    fn a_daemon_with_live_sessions_never_shuts_down() {
        let mut state = DaemonState::new();
        spawn_one(&mut state);
        let client = Uuid::new_v4();
        state.client_connected(client);
        let now = Instant::now();
        state.client_disconnected(client, now);
        assert!(
            !state.should_shutdown(now + ADOPTION_TIMEOUT * 10),
            "con sesiones vivas el daemon se queda: para eso existe"
        );
    }

    #[test]
    fn an_empty_daemon_shuts_down_after_the_adoption_timeout() {
        let mut state = DaemonState::new();
        let client = Uuid::new_v4();
        state.client_connected(client);
        let now = Instant::now();
        state.client_disconnected(client, now);
        assert!(!state.should_shutdown(now), "todavía no");
        assert!(
            !state.should_shutdown(now + ADOPTION_TIMEOUT - Duration::from_secs(1)),
            "todavía dentro de la ventana de adopción"
        );
        assert!(state.should_shutdown(now + ADOPTION_TIMEOUT));
    }

    #[test]
    fn a_daemon_never_adopted_still_shuts_down_after_the_timeout() {
        let state = DaemonState::new();
        let started = state.idle_since.expect("countdown inicial");
        assert!(!state.should_shutdown(started));
        assert!(state.should_shutdown(started + ADOPTION_TIMEOUT));
    }

    #[test]
    fn a_daemon_with_a_client_connected_never_shuts_down() {
        let mut state = DaemonState::new();
        state.client_connected(Uuid::new_v4());
        assert!(!state.should_shutdown(Instant::now() + ADOPTION_TIMEOUT * 10));
    }

    #[test]
    fn reconnecting_cancels_the_shutdown_countdown() {
        let mut state = DaemonState::new();
        let client = Uuid::new_v4();
        state.client_connected(client);
        let now = Instant::now();
        state.client_disconnected(client, now);
        state.client_connected(client);
        state.client_disconnected(client, now + Duration::from_secs(1));
        // El reloj se reinicia con la reconexión.
        assert!(!state.should_shutdown(now + ADOPTION_TIMEOUT));
    }

    #[test]
    fn the_snapshot_is_clamped_keeping_the_tail() {
        let mut snapshot = "viejo".repeat(1000).into_bytes();
        snapshot.extend_from_slice(b"RECIENTE");
        clamp_snapshot(&mut snapshot, 32);
        assert!(snapshot.len() <= 32, "got {}", snapshot.len());
        assert!(snapshot.ends_with(b"RECIENTE"), "se conserva el final");
    }

    #[test]
    fn clamping_preserves_the_exact_binary_tail() {
        let original = [0, 0xff, 0x80, b'a', b'\n'].repeat(20);
        for max in 1..original.len() {
            let mut snapshot = original.clone();
            clamp_snapshot(&mut snapshot, max);
            assert_eq!(snapshot, original[original.len() - max..]);
        }
    }

    #[test]
    fn output_beyond_the_cap_does_not_grow_the_snapshot() {
        let mut state = DaemonState::new();
        let id = spawn_one(&mut state);
        let session = state.session_mut(id).unwrap();
        for _ in 0..50 {
            session.push_output(&vec![b'x'; 16 * 1024]);
        }
        assert!(
            session.snapshot.len() <= MAX_SNAPSHOT_BYTES,
            "got {} bytes",
            session.snapshot.len()
        );
    }

    #[test]
    fn a_repeated_handshake_is_rejected() {
        let mut state = DaemonState::new();
        let response = handle_request(
            &mut state,
            Request::Hello {
                version: 1,
                token: "x".to_owned(),
                client_id: Uuid::new_v4(),
            },
            None,
        );
        assert!(matches!(response, Response::Error { .. }));
    }

    #[test]
    fn broadcast_drops_the_channels_of_apps_that_closed() {
        let mut state = DaemonState::new();
        let (alive_tx, alive_rx) = std::sync::mpsc::sync_channel(SUBSCRIBER_QUEUE_CAPACITY);
        let (dead_tx, dead_rx) = std::sync::mpsc::sync_channel(SUBSCRIBER_QUEUE_CAPACITY);
        let session_id = spawn_one(&mut state);
        state.subscribe(Uuid::new_v4(), session_id, alive_tx, None);
        state.subscribe(Uuid::new_v4(), session_id, dead_tx, None);
        assert_eq!(state.subscriber_count(), 2);

        // La segunda "app" se cerró: su receiver ya no existe.
        drop(dead_rx);
        state.broadcast(&[Response::ShuttingDown]);

        assert_eq!(
            state.subscriber_count(),
            1,
            "el canal muerto tiene que salir de la lista"
        );
        assert_eq!(alive_rx.try_recv(), Ok(Response::ShuttingDown));
    }

    #[test]
    fn output_is_sent_only_to_the_connection_attached_to_that_session() {
        let mut state = DaemonState::new();
        let first = spawn_one(&mut state);
        let second = spawn_one(&mut state);
        let (first_tx, first_rx) = std::sync::mpsc::sync_channel(SUBSCRIBER_QUEUE_CAPACITY);
        let (second_tx, second_rx) = std::sync::mpsc::sync_channel(SUBSCRIBER_QUEUE_CAPACITY);
        state.subscribe(Uuid::new_v4(), first, first_tx, None);
        state.subscribe(Uuid::new_v4(), second, second_tx, None);

        state.broadcast(&[Response::Output {
            id: first,
            seq: 1,
            data: b"solo primera".to_vec(),
        }]);

        assert!(matches!(first_rx.try_recv(), Ok(Response::Output { id, .. }) if id == first));
        assert!(second_rx.try_recv().is_err());
    }

    #[test]
    fn a_slow_subscriber_cannot_accumulate_unbounded_output() {
        let mut state = DaemonState::new();
        let session_id = spawn_one(&mut state);
        let (slow_tx, slow_rx) = std::sync::mpsc::sync_channel(SUBSCRIBER_QUEUE_CAPACITY);
        state.subscribe(Uuid::new_v4(), session_id, slow_tx, None);

        for seq in 1..=1024 {
            state.broadcast(&[Response::Output {
                id: session_id,
                seq,
                data: vec![b'x'; 1024],
            }]);
        }

        let queued = slow_rx.try_iter().count();
        assert!(
            queued <= SUBSCRIBER_QUEUE_CAPACITY,
            "un cliente suspendido acumuló {queued} bloques sin backpressure"
        );
    }

    #[test]
    fn a_session_without_a_pty_pumps_nothing() {
        // El registro sin PTY (tests) no puede inventar salida.
        let mut state = DaemonState::new();
        spawn_one(&mut state);
        assert!(state.pump_output().is_empty());
    }

    #[test]
    fn writing_to_an_unknown_session_is_reported() {
        let state = DaemonState::new();
        assert!(!state.write_to(Uuid::new_v4(), b"hola"));
    }

    #[test]
    fn resizing_an_unknown_session_is_reported() {
        let mut state = DaemonState::new();
        assert!(!state.resize(Uuid::new_v4(), 80, 24));
    }
}

#[cfg(test)]
#[path = "attach_transition_tests.rs"]
mod attach_transition_tests;

#[cfg(test)]
mod bounded_attach_contract_tests {
    use super::*;
    use crate::daemon::protocol::{snapshot_failure_from_response, snapshot_payload_budget};
    use crate::state::scrollback_log::FrameKind;
    use crate::terminal::pty::EventProxy;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config, Term, TermMode};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
    use std::sync::mpsc;

    // A parser-backed fake starts only the existing sink/input and no-op reader
    // helpers; it never starts a PTY, shell, socket, watcher, app or server.
    fn fixture(output: &[u8]) -> (DaemonState, Uuid, SharedPtyHandle) {
        let mut state = DaemonState::new();
        let id = Uuid::new_v4();
        state.insert_session(
            id,
            WireSpec {
                panel_id: Some(Uuid::new_v4()),
                ..WireSpec::default()
            },
        );
        let handle = Arc::new(Mutex::new(PtyHandle::for_persistence_tests(output)));
        state.session_mut(id).unwrap().handle = Some(Arc::clone(&handle));
        (state, id, handle)
    }

    fn durable_output(session: &DaemonSession) -> Vec<u8> {
        decode_frames(&session.persist_pending)
            .into_iter()
            .filter(|frame| frame.kind == FrameKind::Output)
            .flat_map(|frame| frame.payload)
            .collect()
    }

    #[test]
    fn refused_attach_preserves_pending_durable_sequence_and_other_subscriptions() {
        let (mut state, id, handle) = fixture(b"UNSEEN_RAW");
        state
            .session_mut(id)
            .unwrap()
            .push_output(b"ALREADY_DURABLE");
        let before_pending = handle.lock().unwrap().pending_log_snapshot();
        let before_seq = state.session(id).unwrap().seq;
        let before_persist = state.session(id).unwrap().persist_pending.clone();
        let other = Uuid::new_v4();
        state.insert_session(other, WireSpec::default());
        let (tx, rx) = mpsc::sync_channel(8);
        state.subscribe(Uuid::new_v4(), id, tx, None);
        let (other_tx, other_rx) = mpsc::sync_channel(8);
        state.subscribe(Uuid::new_v4(), other, other_tx, None);
        state.snapshot_line_limit = Some(1);
        let response = handle_request(&mut state, Request::Attach { id }, None);
        assert_eq!(
            snapshot_failure_from_response(&response),
            Some((id, SnapshotFailureKind::TooLarge))
        );
        let session = state.session(id).unwrap();
        assert_eq!(session.seq, before_seq);
        assert_eq!(session.persist_pending, before_persist);
        assert_eq!(
            handle.lock().unwrap().pending_log_snapshot(),
            before_pending
        );
        assert!(session.alive);
        assert!(session.live_repair.is_some());
        assert_eq!(state.subscriber_count(), 1);
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert!(matches!(
            other_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(!state.session_claimants.contains_key(&id));
    }

    #[test]
    fn rejection_then_durable_only_pump_then_repair_preserves_ack_prefix_and_uuid() {
        let (mut state, id, handle) = fixture(b"FIRST");
        let now = Instant::now();
        state.snapshot_line_limit = Some(1);
        assert_eq!(
            state
                .session_mut(id)
                .unwrap()
                .attach_boundary(id, 1, now)
                .unwrap_err(),
            SnapshotFailureKind::TooLarge
        );
        assert!(!handle.lock().unwrap().pending_log_snapshot().is_empty());
        assert!(state.session(id).unwrap().persist_pending.is_empty());
        assert!(state
            .pump_output_at(now + Duration::from_millis(1))
            .is_empty());
        assert!(handle.lock().unwrap().pending_log_snapshot().is_empty());
        assert_eq!(durable_output(state.session(id).unwrap()), b"FIRST");
        let first_prefix = state.session(id).unwrap().persist_pending.len();
        handle
            .lock()
            .unwrap()
            .feed_output_for_persistence_tests(b"SECOND");
        assert!(state
            .pump_output_at(now + Duration::from_millis(2))
            .is_empty());
        assert_eq!(durable_output(state.session(id).unwrap()), b"FIRSTSECOND");
        state
            .session_mut(id)
            .unwrap()
            .acknowledge_persisted(first_prefix);
        assert_eq!(durable_output(state.session(id).unwrap()), b"SECOND");
        assert!(state
            .take_dirty_scrollback_targets_at(now + Duration::from_millis(3))
            .is_empty());
        assert_eq!(durable_output(state.session(id).unwrap()), b"SECOND");
        state.snapshot_line_limit = None;
        let events = state.pump_output_at(now + LIVE_REPAIR_RETRY);
        assert!(events.iter().any(
            |event| matches!(event, Response::Output { id: source, data, .. }
            if *source == id && String::from_utf8_lossy(data).contains("FIRSTSECOND"))
        ));
        assert!(state.session(id).unwrap().live_repair.is_none());
        assert_eq!(
            durable_output(state.session(id).unwrap()),
            b"SECOND",
            "rendered repair must not become a durable raw output frame"
        );
        let targets = state.take_dirty_scrollback_targets_at(now + LIVE_REPAIR_RETRY);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].session_id, id);
        assert!(targets[0].text.contains("FIRSTSECOND"));
        assert_eq!(
            targets[0].pending_bytes,
            state.session(id).unwrap().persist_pending.len()
        );
        assert_eq!(
            durable_output(state.session(id).unwrap()),
            b"SECOND",
            "capture alone never acknowledges durable frames"
        );
    }

    struct View {
        term: Term<EventProxy>,
        parser: Processor<StdSyncHandler>,
        seq: u64,
    }

    impl View {
        fn attached(response: Response) -> Self {
            let Response::Attached { snapshot, seq, .. } = response else {
                panic!("expected Attached")
            };
            let (tx, _rx) = mpsc::channel();
            let mut view = Self {
                term: Term::new(
                    Config::default(),
                    &TermSize::new(80, 24),
                    EventProxy::new(tx),
                ),
                parser: Processor::<StdSyncHandler>::new(),
                seq,
            };
            view.parser.advance(&mut view.term, &snapshot);
            view
        }

        fn receive(&mut self, rx: &mpsc::Receiver<Response>, id: Uuid) {
            for event in rx.try_iter() {
                if let Response::Output {
                    id: source,
                    seq,
                    data,
                } = event
                {
                    assert_eq!(source, id);
                    assert!(seq > self.seq);
                    self.seq = seq;
                    self.parser.advance(&mut self.term, &data);
                }
            }
        }

        fn assert_repaired(&self) {
            assert!(!self.term.mode().contains(TermMode::ALT_SCREEN));
            let text = crate::terminal::export::scrollback_to_text(&self.term);
            for marker in ["PRIMARY", "RETURNED"] {
                assert_eq!(
                    text.lines().filter(|line| line.trim() == marker).count(),
                    1,
                    "{text:?}"
                );
            }
        }
    }

    fn assert_transition_order(first_observer: u8) {
        let (mut state, id, handle) = fixture(b"\x1b[2J\x1b[3J\x1b[HPRIMARY\r\n\x1b[?1049hTUI");
        let mut first = View::attached(handle_request(&mut state, Request::Attach { id }, None));
        assert!(first.term.mode().contains(TermMode::ALT_SCREEN));
        let (tx, rx) = mpsc::sync_channel(64);
        state.subscribe(Uuid::new_v4(), id, tx, None);
        handle
            .lock()
            .unwrap()
            .feed_output_for_persistence_tests(b"\x1b[?1049l\r\nRETURNED\r\n");
        if first_observer == 1 {
            let events = state.pump_output();
            state.broadcast(&events);
        } else if first_observer == 2 {
            let targets = state.take_dirty_scrollback_targets();
            assert_eq!(targets.len(), 1);
            assert!(targets[0].text.contains("PRIMARY"));
            assert!(targets[0].text.contains("RETURNED"));
        }
        let second = View::attached(handle_request(&mut state, Request::Attach { id }, None));
        first.receive(&rx, id);
        first.assert_repaired();
        second.assert_repaired();
        for _ in 0..3 {
            let events = state.pump_output();
            state.broadcast(&events);
            first.receive(&rx, id);
        }
        first.assert_repaired();
        let durable = durable_output(state.session(id).unwrap());
        for marker in [b"PRIMARY".as_slice(), b"RETURNED".as_slice()] {
            assert_eq!(
                durable
                    .windows(marker.len())
                    .filter(|window| *window == marker)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn attach_first_repairs_primary_without_duplicate_durable_output() {
        assert_transition_order(0);
    }

    #[test]
    fn pump_first_repairs_primary_without_duplicate_durable_output() {
        assert_transition_order(1);
    }

    #[test]
    fn checkpoint_first_repairs_primary_without_duplicate_durable_output() {
        assert_transition_order(2);
    }

    #[test]
    fn refused_checkpoint_keeps_primary_repair_pending_and_unacknowledged() {
        let (mut state, id, handle) = fixture(b"PRIMARY\r\n\x1b[?1049hTUI");
        assert!(matches!(
            handle_request(&mut state, Request::Attach { id }, None),
            Response::Attached { .. }
        ));
        handle
            .lock()
            .unwrap()
            .feed_output_for_persistence_tests(b"\x1b[?1049l\r\nRETURNED");
        let pending = handle.lock().unwrap().pending_log_snapshot();
        let seq = state.session(id).unwrap().seq;
        let durable = state.session(id).unwrap().persist_pending.clone();
        state.snapshot_line_limit = Some(1);
        assert!(state.take_dirty_scrollback_targets().is_empty());
        let session = state.session(id).unwrap();
        assert!(session.was_alternate);
        assert!(session.live_repair.is_some());
        assert!(session.scrollback_dirty);
        assert_eq!(session.seq, seq);
        assert_eq!(session.persist_pending, durable);
        assert_eq!(handle.lock().unwrap().pending_log_snapshot(), pending);
    }

    #[test]
    fn refused_pump_keeps_primary_repair_pending_and_unacknowledged() {
        let (mut state, id, handle) = fixture(b"PRIMARY\r\n\x1b[?1049hTUI");
        assert!(matches!(
            handle_request(&mut state, Request::Attach { id }, None),
            Response::Attached { .. }
        ));
        handle
            .lock()
            .unwrap()
            .feed_output_for_persistence_tests(b"\x1b[?1049l\r\nRETURNED");
        let pending = handle.lock().unwrap().pending_log_snapshot();
        let seq = state.session(id).unwrap().seq;
        state.snapshot_line_limit = Some(1);
        assert!(state.pump_output().is_empty());
        let session = state.session(id).unwrap();
        assert!(session.was_alternate);
        assert!(session.live_repair.is_some());
        assert!(session.alive);
        assert_eq!(session.seq, seq);
        assert_eq!(handle.lock().unwrap().pending_log_snapshot(), pending);
    }

    #[test]
    fn raw_burst_chunks_roundtrip_with_strict_sequences_and_exact_durable_payload() {
        let payload = vec![b'x'; RAW_OUTPUT_CHUNK_BYTES * 3 + 7];
        let (mut state, id, _handle) = fixture(&payload);
        let events = state.pump_output();
        let mut previous = 0;
        let mut live = Vec::new();
        for event in events {
            if let Response::Output {
                id: source,
                seq,
                data,
            } = &event
            {
                assert_eq!(*source, id);
                assert!(*seq > previous);
                previous = *seq;
                assert!(data.len() <= RAW_OUTPUT_CHUNK_BYTES);
                let encoded =
                    encode_response_line_with_limit(&event, MAX_PROTOCOL_LINE_BYTES).unwrap();
                assert!(encoded.len() <= MAX_PROTOCOL_LINE_BYTES);
                assert_eq!(
                    decode_line::<Response>(std::str::from_utf8(&encoded).unwrap()),
                    Some(event.clone())
                );
                if data.iter().all(|byte| *byte == b'x') {
                    live.extend_from_slice(data);
                }
            }
        }
        assert_eq!(live, payload);
        assert_eq!(durable_output(state.session(id).unwrap()), payload);
    }

    #[test]
    fn metadata_budget_failure_happens_before_atomic_drain_and_utf8_is_not_lossy() {
        let (_state, id, handle) = fixture(b"RAW");
        let pending = handle.lock().unwrap().pending_log_snapshot();
        let cap = 512;
        let metadata = vec![b'm'; snapshot_payload_budget(id, cap).unwrap() + 1];
        let result = handle
            .lock()
            .unwrap()
            .try_attach_snapshot_and_drain(|term| bounded_wire_snapshot(term, &metadata, id, cap));
        assert_eq!(result.unwrap_err(), SnapshotFailureKind::TooLarge);
        assert_eq!(handle.lock().unwrap().pending_log_snapshot(), pending);
        let result = handle
            .lock()
            .unwrap()
            .try_attach_snapshot_and_drain(|term| {
                bounded_wire_snapshot(term, &[0xff], id, MAX_PROTOCOL_LINE_BYTES)
            });
        assert_eq!(result.unwrap_err(), SnapshotFailureKind::Unavailable);
        assert_eq!(handle.lock().unwrap().pending_log_snapshot(), pending);
    }

    #[test]
    fn final_writer_guard_never_calls_transport_for_an_oversized_response() {
        let response = Response::Output {
            id: Uuid::new_v4(),
            seq: 1,
            data: vec![b'x'; 512],
        };
        let mut transmitted = Vec::new();
        let result = with_encoded_response(&response, 128, |line| {
            transmitted.extend_from_slice(line);
            Ok(())
        });
        assert!(result.is_err());
        assert!(transmitted.is_empty());
        with_encoded_response(&response, MAX_PROTOCOL_LINE_BYTES, |line| {
            transmitted.extend_from_slice(line);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            decode_line::<Response>(std::str::from_utf8(&transmitted).unwrap()),
            Some(response)
        );
    }

    #[test]
    fn poisoned_outer_handle_refuses_before_drain_without_a_false_exit() {
        let (mut state, id, handle) = fixture(b"RESCUE");
        let pending = handle.lock().unwrap().pending_log_snapshot();
        let poisoned = Arc::clone(&handle);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = poisoned.lock().unwrap();
            panic!("injected outer mutex failure");
        }))
        .is_err());
        let now = Instant::now();
        assert!(state.pump_output_at(now).is_empty());
        assert!(state.session(id).unwrap().alive);
        assert_eq!(state.session(id).unwrap().seq, 0);
        let rescued = match handle.lock() {
            Ok(_) => panic!("expected poisoned outer mutex"),
            Err(poison) => poison.into_inner().pending_log_snapshot(),
        };
        assert_eq!(rescued, pending);
        assert!(state
            .pump_output_at(now + Duration::from_millis(1))
            .is_empty());
        assert_eq!(durable_output(state.session(id).unwrap()), b"RESCUE");
    }
}

#[cfg(test)]
mod full_attach_birth_retirement_fixtures {
    use super::*;
    use crate::daemon::protocol::{birth_rejected_from_response, snapshot_failure_from_response};

    #[test]
    fn actual_final_read_and_child_wait_cannot_race_an_empty_tail_capture() {
        use std::io::{self, Read};
        use std::sync::mpsc;
        struct Reader {
            step: usize,
            events: mpsc::Sender<&'static str>,
            release: mpsc::Receiver<()>,
        }
        impl Read for Reader {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.step += 1;
                match self.step {
                    1 => {
                        bytes[..6].copy_from_slice(b"PREFIX");
                        Ok(6)
                    }
                    2 => {
                        self.events.send("blocked-before-pause").unwrap();
                        self.release.recv().unwrap();
                        Err(io::ErrorKind::Interrupted.into())
                    }
                    3 => {
                        self.events.send("final-read-admitted").unwrap();
                        self.release.recv().unwrap();
                        bytes[..4].copy_from_slice(b"TAIL");
                        Ok(4)
                    }
                    _ => Ok(0),
                }
            }
        }
        let (events_tx, events_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (child_tx, child_rx) = mpsc::channel();
        let (wait_tx, wait_rx) = mpsc::channel();
        let handle = Arc::new(Mutex::new(PtyHandle::for_retirement_boundary_tests(
            Box::new(Reader {
                step: 0,
                events: events_tx,
                release: release_rx,
            }),
            child_tx,
            wait_rx,
        )));
        handle.lock().unwrap().set_daemon_durable_pressure(0);
        let id = Uuid::from_u128(0x6260);
        let owner = Uuid::from_u128(0x6261);
        let mut state = DaemonState::new();
        state.insert_session(
            id,
            WireSpec {
                panel_id: Some(Uuid::from_u128(0x6262)),
                ..WireSpec::default()
            },
        );
        state.session_mut(id).unwrap().birth_client = Some(owner);
        state.session_mut(id).unwrap().handle = Some(handle.clone());
        assert_eq!(
            events_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "blocked-before-pause"
        );
        assert!(!state.kill(id));
        assert!(child_rx.try_recv().is_err());
        release_tx.send(()).unwrap(); // injected completion of canceled sync IO
        let deadline = Instant::now() + Duration::from_secs(2);
        while !state.session(id).unwrap().retirement_captured {
            state.capture_retirement(id);
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let prefix = state.session(id).unwrap().persist_pending.clone();
        assert_eq!(decode_frames(&prefix)[0].payload, b"PREFIX");
        assert!(handle.lock().unwrap().pending_log_snapshot().is_empty());
        assert!(child_rx.try_recv().is_err()); // no termination before durable ACK
        state
            .session_mut(id)
            .unwrap()
            .acknowledge_persisted(prefix.len());
        state.capture_retirement(id);
        assert_eq!(
            child_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "terminate"
        );
        assert_eq!(
            child_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "wait"
        );
        assert_eq!(
            events_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "final-read-admitted"
        );
        wait_tx.send(Ok(())).unwrap();
        state.capture_retirement(id); // empty early tail, final read still blocked
        assert!(state.session(id).unwrap().persist_pending.is_empty());
        assert!(!state.session(id).unwrap().retirement_final_captured);
        assert!(!state.kill(id));
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !handle.lock().unwrap().output_finished() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        // A stale UI ACK has no ownership of the newly admitted physical tail.
        handle.lock().unwrap().acknowledge_pending_log(prefix.len());
        assert_eq!(
            decode_frames(
                &handle
                    .lock()
                    .unwrap()
                    .retirement_pending_snapshot()
                    .unwrap()
                    .unwrap()
            )[0]
            .payload,
            b"TAIL"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while !state.session(id).unwrap().retirement_final_captured {
            state.capture_retirement(id);
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let tail = state.session(id).unwrap().persist_pending.clone();
        assert_eq!(
            decode_frames(&tail)
                .iter()
                .map(|frame| frame.payload.clone())
                .collect::<Vec<_>>(),
            [b"TAIL".to_vec()]
        );
        assert!(!state.kill(id)); // exact final durable tail ACK still missing
        state
            .session_mut(id)
            .unwrap()
            .acknowledge_persisted(tail.len());
        assert!(state.kill(id));
        assert_eq!(
            request(&mut state, owner, Request::CloseBirth { id }),
            Response::Killed { id }
        );
    }

    fn request(state: &mut DaemonState, client: Uuid, request: Request) -> Response {
        handle_request_for_client(state, request, None, client)
    }

    fn fake_session(panel: Option<Uuid>) -> (DaemonState, Uuid, Uuid, SharedPtyHandle) {
        let mut state = DaemonState::new();
        let id = Uuid::from_u128(0x6201);
        let creator = Uuid::from_u128(0x6202);
        state.insert_session(
            id,
            WireSpec {
                panel_id: panel,
                ..WireSpec::default()
            },
        );
        let handle = Arc::new(Mutex::new(PtyHandle::for_persistence_tests(b"FINAL")));
        handle.lock().unwrap().set_daemon_durable_pressure(0);
        let session = state.session_mut(id).unwrap();
        session.birth_client = Some(creator);
        session.handle = Some(Arc::clone(&handle));
        (state, id, creator, handle)
    }

    #[test]
    fn lost_spawn_ack_repeats_uuid_without_replacing_the_creator_session() {
        let mut state = DaemonState::new();
        let id = Uuid::from_u128(0x6210);
        let creator = Uuid::from_u128(0x6211);
        let spec = WireSpec {
            startup_command: Some("once".to_owned()),
            ..WireSpec::default()
        };
        let _lost_ack = request(
            &mut state,
            creator,
            Request::Spawn {
                id: Some(id),
                spec: spec.clone(),
            },
        );
        state.session_mut(id).unwrap().push_output(b"PRESERVED");
        let before = state.session(id).unwrap().persist_pending.clone();
        let reply = request(
            &mut state,
            creator,
            Request::Spawn {
                id: Some(id),
                spec: WireSpec::default(),
            },
        );
        assert_eq!(reply, Response::Spawned { id });
        assert_eq!(state.session_ids(), vec![id]);
        let mut owned_spec = spec;
        owned_spec.hook_owner = Some(creator);
        assert_eq!(state.session(id).unwrap().spec, owned_spec);
        assert_eq!(state.session(id).unwrap().persist_pending, before);
        assert_eq!(state.session(id).unwrap().birth_client, Some(creator));
    }

    #[test]
    fn cancelled_birth_fences_a_late_spawn_and_foreign_creator() {
        let mut state = DaemonState::new();
        let id = Uuid::from_u128(0x6220);
        let creator = Uuid::from_u128(0x6221);
        let path =
            std::env::temp_dir().join(format!("tc-host-close-ledger-{}.sqlite", Uuid::new_v4()));
        let store = crate::memory::MemoryStore::open(&path).unwrap();
        let key = crate::hook_authority::LeafKey {
            workspace: Uuid::new_v4(),
            panel: Uuid::new_v4(),
            leaf: Uuid::new_v4(),
        };
        let birth = store
            .hook_transition(
                creator,
                Uuid::new_v4(),
                &crate::memory::hook_ledger::LedgerIntent::Reserve {
                    expected: None,
                    key,
                    birth: crate::hook_authority::BirthBinding {
                        runtime: id,
                        generation: Uuid::new_v4(),
                    },
                    owner: creator,
                },
            )
            .unwrap()
            .claim
            .unwrap();
        store
            .hook_transition(
                creator,
                Uuid::new_v4(),
                &crate::memory::hook_ledger::LedgerIntent::FenceRetire { expected: birth },
            )
            .unwrap();
        state.hook_store = Some(store);
        assert_eq!(
            request(&mut state, creator, Request::CloseBirth { id }),
            Response::Killed { id }
        );
        let late = request(
            &mut state,
            creator,
            Request::Spawn {
                id: Some(id),
                spec: WireSpec::default(),
            },
        );
        assert_eq!(birth_rejected_from_response(&late), Some(id));
        let foreign = request(
            &mut state,
            Uuid::from_u128(0x6222),
            Request::CloseBirth { id },
        );
        assert_eq!(birth_rejected_from_response(&foreign), Some(id));
        assert!(state.session_ids().is_empty());
        assert_eq!(state.birth_tombstones[&id], creator);
        drop(state);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn foreign_birth_rejection_never_grants_adoption_or_kill() {
        let (mut state, id, creator, _) = fake_session(Some(Uuid::from_u128(0x6230)));
        let foreign = Uuid::from_u128(0x6231);
        assert_eq!(
            birth_rejected_from_response(&request(
                &mut state,
                foreign,
                Request::Spawn {
                    id: Some(id),
                    spec: WireSpec::default()
                }
            )),
            Some(id)
        );
        assert_eq!(
            birth_rejected_from_response(&request(&mut state, foreign, Request::CloseBirth { id })),
            Some(id)
        );
        assert!(!state.session(id).unwrap().adopters.contains(&foreign));
        assert_eq!(state.session(id).unwrap().birth_client, Some(creator));
        assert!(!state.session(id).unwrap().retirement_requested);
    }

    #[test]
    fn creator_close_waits_for_final_raw_ack_even_after_snapshot_refusal() {
        let (mut state, id, creator, handle) = fake_session(Some(Uuid::from_u128(0x6240)));
        state.snapshot_line_limit = Some(1);
        assert!(snapshot_failure_from_response(&request(
            &mut state,
            creator,
            Request::Attach { id }
        ))
        .is_some());
        let before = handle.lock().unwrap().pending_log_snapshot();
        let close = request(&mut state, creator, Request::CloseBirth { id });
        assert!(matches!(close, Response::Error { .. }));
        assert!(state.session(id).unwrap().handle.is_some());
        assert!(!before.is_empty());
        assert!(handle.lock().unwrap().pending_log_snapshot().is_empty());
        assert_eq!(
            decode_frames(&state.session(id).unwrap().persist_pending)[0].payload,
            b"FINAL"
        );
        state.finish_acknowledged_retirements();
        assert!(state.session(id).is_some());
        let bytes = state.session(id).unwrap().persist_pending.len();
        state.session_mut(id).unwrap().acknowledge_persisted(bytes);
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while state.session(id).is_some() {
            state.finish_acknowledged_retirements();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(state.birth_tombstones[&id], creator);
        assert_eq!(
            request(&mut state, creator, Request::CloseBirth { id }),
            Response::Killed { id }
        );
    }

    #[test]
    fn old_prefix_ack_cannot_destroy_the_unacknowledged_final_tail() {
        let (mut state, id, _, _) = fake_session(Some(Uuid::from_u128(0x6250)));
        state.session_mut(id).unwrap().push_output(b"PREFIX");
        let prefix = state.session(id).unwrap().persist_pending.len();
        assert!(!state.kill(id));
        state.session_mut(id).unwrap().acknowledge_persisted(prefix);
        state.finish_acknowledged_retirements();
        assert!(state.session(id).is_some());
        let pending = &state.session(id).unwrap().persist_pending;
        assert_eq!(decode_frames(pending)[0].payload, b"FINAL");
        let bytes = pending.len();
        state.session_mut(id).unwrap().acknowledge_persisted(bytes);
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while state.session(id).is_some() {
            state.finish_acknowledged_retirements();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn reconcile_and_missing_destination_keep_the_pending_pty_protected() {
        let (mut state, id, creator, handle) = fake_session(None);
        let pending = handle.lock().unwrap().pending_log_snapshot();
        assert!(state.reconcile_live(creator, &[]).is_empty());
        assert!(state.session(id).unwrap().retirement_requested);
        assert!(!state.session(id).unwrap().retirement_captured);
        assert_eq!(
            handle
                .lock()
                .unwrap()
                .retirement_pending_snapshot()
                .unwrap()
                .unwrap(),
            pending
        );
        assert!(state.session(id).unwrap().persist_pending.is_empty());
        assert!(!state.should_shutdown_with_request(Instant::now(), true));
    }
}

fn host_target_from_spec(spec: &WireSpec) -> Option<super::protocol::HostHookTarget> {
    Some(super::protocol::HostHookTarget {
        workspace_id: spec.workspace_id?,
        panel_id: spec.panel_id?,
        leaf_id: spec.leaf_id?,
    })
}

fn host_unknown_close_fence(
    store: Option<&crate::memory::MemoryStore>,
    runtime: Uuid,
    owner: Uuid,
) -> Option<crate::hook_authority::Claim> {
    store?.hook_close_claim(runtime, Some(owner)).ok()?
}

fn host_close_fenced(
    store: Option<&crate::memory::MemoryStore>,
    runtime: Uuid,
    session: &DaemonSession,
    owner: Option<Uuid>,
) -> bool {
    let Some(target) = host_target_from_spec(&session.spec) else {
        return true;
    };
    store
        .and_then(|store| store.hook_close_claim(runtime, owner).ok().flatten())
        .is_some_and(|claim| {
            claim.generation == session.spec.hook_generation
                && claim.key.workspace == target.workspace_id
                && claim.key.panel == target.panel_id
                && claim.key.leaf == target.leaf_id
        })
}

#[cfg(test)]
mod unscoped_durable_destination_fixtures {
    use super::*;

    #[test]
    fn a_raw_scoped_spawn_without_ledger_authority_never_enters_the_pty_factory() {
        let mut state = DaemonState::new();
        let id = Uuid::new_v4();
        let spec = WireSpec {
            workspace_id: Some(Uuid::new_v4()),
            panel_id: Some(Uuid::new_v4()),
            leaf_id: Some(Uuid::new_v4()),
            hook_generation: Some(Uuid::new_v4()),
            ..WireSpec::default()
        };
        let scheduler = Arc::new(Mutex::new(RuntimeScheduler::new()));
        let response = handle_request_for_client(
            &mut state,
            Request::Spawn { id: Some(id), spec },
            Some(&scheduler),
            Uuid::new_v4(),
        );
        assert_eq!(
            super::super::protocol::birth_rejected_from_response(&response),
            Some(id)
        );
        assert!(state.sessions.is_empty());
        assert!(state.session_claimants.is_empty());
        assert!(state.birth_tombstones.is_empty());
    }

    #[test]
    fn unscoped_ptys_have_distinct_destinations_and_only_real_writes_release_pressure() {
        let dir = std::env::temp_dir().join(format!("tc-daemon-unscoped-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let blocked = dir.join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let mut state = DaemonState::new();
        let ids = [Uuid::new_v4(), Uuid::new_v4()];
        let output = vec![b'x'; 2 * crate::terminal::flow_control::HIGH_WATER];
        for id in ids {
            state
                .spawn_with_pty_in_history_dir_using(
                    WireSpec::default(),
                    Some(id),
                    Some(&dir),
                    |_, _, history| {
                        assert!(history.checkpoint.is_empty() && history.frames.is_empty());
                        Ok(PtyHandle::for_persistence_tests(&output))
                    },
                )
                .unwrap();
        }
        state.pump_output();
        for id in ids {
            let session = state.session(id).unwrap();
            assert_eq!(session.spec.panel_id, None);
            assert_eq!(host_target_from_spec(&session.spec), None);
            assert_eq!(session.history_panel, Some(id));
            assert!(session.persist_pending.len() >= output.len());
        }
        let (ack, _) =
            persist_incremental_targets_in_dir(&blocked, state.incremental_scrollback_targets());
        assert!(
            ack.is_empty(),
            "failed writes cannot release durable pressure"
        );
        for id in ids {
            assert!(!state.session(id).unwrap().persist_pending.is_empty());
        }
        let (ack, _) =
            persist_incremental_targets_in_dir(&dir, state.incremental_scrollback_targets());
        assert_eq!(ack.len(), ids.len());
        for (id, bytes) in ack {
            state.session_mut(id).unwrap().acknowledge_persisted(bytes);
        }
        for id in ids {
            assert!(state.session(id).unwrap().persist_pending.is_empty());
            let history =
                crate::state::scrollback_store::try_load_leaf_session(&dir, id, None).unwrap();
            let restored: Vec<u8> = history
                .frames
                .iter()
                .filter(|f| f.kind == crate::state::scrollback_log::FrameKind::Output)
                .flat_map(|f| f.payload.iter().copied())
                .collect();
            assert_eq!(restored, output);
        }
        assert!(state.incremental_scrollback_targets().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
