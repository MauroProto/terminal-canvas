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
    decode_line, encode_line, handshake_ok, read_protocol_line, Request, Response, WireSpec,
    PROTOCOL_VERSION,
};
use crate::runtime::{RuntimeScheduler, SharedPtyHandle};
use crate::terminal::pty::{HookIdentity, PtyHandle};

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
    /// El PTY real (T2). `None` en los tests del registro, que no montan
    /// procesos: la lógica de sesiones se testea sin spawnear nada.
    pub handle: Option<SharedPtyHandle>,
}

impl DaemonSession {
    fn new(spec: WireSpec) -> Self {
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
            handle: None,
        }
    }

    /// Registra salida nueva y devuelve el `seq` que le toca.
    pub fn push_output(&mut self, data: &[u8]) -> u64 {
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
    }

    fn ingest_pty_frames(&mut self, frames: &[u8]) -> Option<(u64, Vec<u8>)> {
        let mut output = Vec::new();
        for frame in decode_frames(frames) {
            self.record_persist_frame(frame.kind, &frame.payload);
            if frame.kind == crate::state::scrollback_log::FrameKind::Output {
                output.extend_from_slice(&frame.payload);
            }
        }
        if output.is_empty() {
            None
        } else {
            let seq = self.push_output_event(&output);
            Some((seq, output))
        }
    }

    fn acknowledge_persisted(&mut self, written_bytes: usize) {
        let acknowledged = written_bytes.min(self.persist_pending.len());
        self.persist_pending.drain(..acknowledged);
        if self.persist_pending.is_empty() {
            // La secuencia de estos frames sólo ordena el lote en RAM. El
            // writer durable los renumera contra el archivo, así que reiniciar
            // al vaciar el lote evita un contador de vida infinita.
            self.persist_seq = 0;
        }
    }

    /// Snapshot semántico + salida todavía no numerada, tomados en una única
    /// frontera. El evento devuelto debe difundirse a subscribers anteriores;
    /// el cliente que está haciendo attach ya lo recibe dentro del snapshot.
    fn attach_boundary(&mut self) -> (Vec<u8>, Option<(u64, Vec<u8>)>) {
        let boundary = self.handle.clone().and_then(|handle| {
            handle.lock().ok().and_then(|pty| {
                let ((mut snapshot, alternate), frames) =
                    pty.attach_snapshot_and_drain(|term| {
                        (
                            crate::terminal::export::live_snapshot_to_ansi(term),
                            term.mode()
                                .contains(alacritty_terminal::term::TermMode::ALT_SCREEN),
                        )
                    })?;
                snapshot.push_str(&String::from_utf8_lossy(&pty.metadata_osc(true)));
                Some((snapshot, frames, alternate))
            })
        });
        let Some((snapshot, frames, alternate)) = boundary else {
            // Fallback de tests/sesiones sin PTY.
            return (self.snapshot.clone(), None);
        };
        self.was_alternate = alternate;
        let event = self.ingest_pty_frames(&frames);
        (snapshot.into_bytes(), event)
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
    /// Instancias de app que adoptaron cada sesión. Una sesión puede estar
    /// adjunta temporalmente en dos UIs durante una reconexión; mientras
    /// cualquiera de sus claimants siga vivo, otra app no puede eliminarla.
    session_claimants: HashMap<Uuid, HashSet<Uuid>>,
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
}

impl DaemonState {
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
    ) -> Uuid {
        // Se respeta el id que propone el cliente para que el id de la app y
        // el del daemon coincidan; si ya está tomado, se genera uno nuevo.
        let id = match desired_id.filter(|id| !self.sessions.contains_key(id)) {
            Some(id) => {
                self.insert_session(id, spec.clone());
                id
            }
            None => self.spawn(spec.clone()),
        };
        let cwd = spec.cwd.as_deref().map(std::path::Path::new);
        let (checkpoint, frames) = spec
            .panel_id
            .and_then(|panel_id| {
                let dir = crate::state::scrollback_store::scrollback_dir()?;
                Some(crate::state::scrollback_store::load_leaf_session(
                    &dir,
                    panel_id,
                    spec.leaf_id,
                ))
            })
            .unwrap_or_default();
        match PtyHandle::spawn_with_history(
            cwd,
            spec.cols.max(1),
            spec.rows.max(1),
            id,
            Arc::clone(scheduler),
            HookIdentity {
                memory_task_id: spec.memory_task_id,
                panel_id: spec.panel_id,
                workspace_id: spec.workspace_id,
                leaf_id: spec.leaf_id,
            },
            &checkpoint,
            &frames,
        ) {
            Ok(handle) => {
                if let Some(command) = spec.startup_command.as_deref().map(str::trim) {
                    if !command.is_empty() {
                        handle.write_all(format!("{command}\n").as_bytes());
                    }
                }
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.handle = Some(Arc::new(Mutex::new(handle)));
                }
            }
            Err(err) => {
                log::warn!("no se pudo espawnear el PTY de {id}: {err}");
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.alive = false;
                }
            }
        }
        id
    }

    /// Escribe bytes en el PTY de la sesión. `false` si no existe.
    pub fn write_to(&self, id: Uuid, data: &[u8]) -> bool {
        let Some(session) = self.sessions.get(&id) else {
            return false;
        };
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
        let mut events = Vec::new();
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
            let is_priority = self.priority_session == Some(id);
            let (frames, alive, alternate, replacement) = match handle.lock() {
                Ok(pty) => {
                    let budget = if is_priority && interactive_budget > 0 {
                        interactive_budget
                    } else {
                        usize::MAX
                    };
                    let Some((frames, alternate, replacement)) = pty.output_update(
                        budget,
                        session.was_alternate,
                        crate::terminal::export::live_snapshot_to_ansi,
                    ) else {
                        continue;
                    };
                    if is_priority {
                        interactive_budget = interactive_budget.saturating_sub(frames.len());
                    }
                    (
                        frames,
                        pty.alive.load(std::sync::atomic::Ordering::Relaxed),
                        alternate,
                        replacement,
                    )
                }
                Err(_) => (Vec::new(), false, session.was_alternate, None),
            };
            session.was_alternate = alternate;
            let output = session.ingest_pty_frames(&frames);
            if let Some(snapshot) = replacement {
                let data = snapshot.into_bytes();
                let seq = output
                    .map(|(seq, _)| seq)
                    .unwrap_or_else(|| session.push_output_event(&data));
                events.push(Response::Output { id, seq, data });
            } else if let Some((seq, data)) = output {
                events.push(Response::Output { id, seq, data });
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
                    let seq = session.push_output_event(&metadata);
                    events.push(Response::Output {
                        id,
                        seq,
                        data: metadata,
                    });
                }
            }
            if !alive && session.alive {
                session.alive = false;
                events.push(Response::Exit { id });
            }
        }
        events
    }

    /// Captura checkpoints completos consistentes. El export ocurre bajo el
    /// lock del PTY, pero el I/O durable se hace después de soltar el registro
    /// global del daemon.
    fn take_dirty_scrollback_targets(&mut self) -> Vec<ScrollbackTarget> {
        let mut targets = Vec::new();
        let mut events = Vec::new();
        for (session_id, session) in &mut self.sessions {
            if !session.scrollback_dirty {
                continue;
            }
            let Some(panel_id) = session.spec.panel_id else {
                continue;
            };
            let boundary = session.handle.as_ref().and_then(|handle| {
                handle.lock().ok()?.attach_snapshot_and_drain(|term| {
                    (!term
                        .mode()
                        .contains(alacritty_terminal::term::TermMode::ALT_SCREEN))
                    .then(|| crate::terminal::export::scrollback_to_ansi(term))
                })
            });
            let Some((text, frames)) = boundary else {
                continue;
            };
            // The checkpoint includes output the pump has not seen yet.
            // Number and broadcast it now, and acknowledge that same prefix
            // only after the checkpoint is durable.
            if let Some((seq, data)) = session.ingest_pty_frames(&frames) {
                events.push(Response::Output {
                    id: *session_id,
                    seq,
                    data,
                });
            }
            // Keep the primary checkpoint and incremental transitions while
            // an alternate-screen application is active.
            let Some(text) = text else {
                continue;
            };
            session.scrollback_dirty = false;
            targets.push(ScrollbackTarget {
                session_id: *session_id,
                panel_id,
                leaf_id: session.spec.leaf_id,
                text,
                pending_bytes: session.persist_pending.len(),
            });
        }
        self.broadcast(&events);
        targets
    }
    fn incremental_scrollback_targets(&self) -> Vec<IncrementalTarget> {
        self.sessions
            .iter()
            .filter_map(|(session_id, session)| {
                let panel_id = session.spec.panel_id?;
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

    pub fn kill(&mut self, id: Uuid) -> bool {
        match self.sessions.remove(&id) {
            Some(session) => {
                self.broadcast(&[Response::Exit { id }]);
                self.session_claimants.remove(&id);
                if self.priority_session == Some(id) {
                    self.priority_session = None;
                }
                // Cerrar el handle mata al hijo: si no, queda huérfano.
                drop(session.handle);
                true
            }
            None => false,
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
            if self.sessions.contains_key(id) {
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
                !self.session_claimants.get(id).is_some_and(|claimants| {
                    claimants
                        .iter()
                        .any(|claimant| self.client_connections.contains_key(claimant))
                })
            })
            .collect();
        for id in &orphans {
            self.sessions.remove(id);
            self.session_claimants.remove(id);
            if self.priority_session == Some(*id) {
                self.priority_session = None;
            }
        }
        let mut sorted = orphans;
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

fn leaf_generation(dir: &Path, panel_id: Uuid, leaf_id: Option<Uuid>) -> u32 {
    crate::state::scrollback_store::load_leaf_scrollback_checkpoint(dir, panel_id, leaf_id)
        .and_then(|(generation, _)| generation)
        .unwrap_or(0)
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
    let mut acknowledgements = Vec::new();
    let mut failed = Vec::new();
    for target in targets {
        let generation = leaf_generation(&dir, target.panel_id, target.leaf_id).saturating_add(1);
        if let Err(err) = crate::state::scrollback_store::save_leaf_scrollback_versioned(
            &dir,
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
    let mut acknowledgements = Vec::new();
    let mut rollover = Vec::new();
    for target in targets {
        let generation = leaf_generation(&dir, target.panel_id, target.leaf_id);
        let log_path = dir.join(
            crate::state::scrollback_store::scrollback_leaf_log_file_name(
                target.panel_id,
                target.leaf_id,
            ),
        );
        let existing = std::fs::read(&log_path)
            .ok()
            .and_then(|bytes| crate::state::scrollback_log::read_frames(&bytes));
        let current = existing
            .as_ref()
            .is_some_and(|(log_generation, _)| *log_generation == generation);
        if !current && crate::state::scrollback_log::reset_log(&log_path, generation).is_err() {
            continue;
        }
        // Renumber to continue from the last durable seq in the existing log.
        // Without this, a fresh daemon process would append seq 1 behind an
        // existing log ending at seq N, creating a gap that invalidates the
        // entire log on the next cold restore.
        let first_seq = if current {
            match existing
                .as_ref()
                .and_then(|(_, frames)| frames.last())
                .map(|frame| frame.seq.checked_add(1))
            {
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
            if let Err(error) = compact_alternate_log(&dir, &target, generation) {
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

fn compact_alternate_log(
    dir: &Path,
    target: &IncrementalTarget,
    generation: u32,
) -> anyhow::Result<()> {
    use crate::state::scrollback_log::{encode_header, FrameKind};
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config, Term, TermMode};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
    let (checkpoint, frames) =
        crate::state::scrollback_store::load_leaf_session(dir, target.panel_id, target.leaf_id);
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
    parser.advance(&mut term, &checkpoint);
    for frame in frames {
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
    let mut primary = crate::terminal::export::scrollback_to_ansi(&term);
    // Store the screen mode inside the same atomic checkpoint as the primary
    // text. A crash before log rotation must still confine later TUI output.
    primary.push_str("\x1b[?1049h\n");
    let next = generation
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("checkpoint generation exhausted"))?;
    crate::state::scrollback_store::save_leaf_scrollback_versioned(
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
        Request::Spawn { spec, id } => {
            let id = match scheduler {
                Some(scheduler) => state.spawn_with_pty(spec, scheduler, id),
                // Sin scheduler (tests del registro) no se espawnea nada.
                None => match id.filter(|id| state.session(*id).is_none()) {
                    Some(id) => {
                        state.insert_session(id, spec);
                        id
                    }
                    None => state.spawn(spec),
                },
            };
            state
                .session_claimants
                .entry(id)
                .or_default()
                .insert(client_id);
            Response::Spawned { id }
        }
        Request::Attach { id } => match state.session_mut(id) {
            Some(session) => {
                let (snapshot, pending_output) = session.attach_boundary();
                let seq = session.seq;
                let response = Response::Attached {
                    id,
                    snapshot,
                    seq,
                    alive: session.alive,
                };
                if let Some((seq, data)) = pending_output {
                    state.broadcast(&[Response::Output { id, seq, data }]);
                }
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
        },
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
            if state.kill(id) {
                Response::Killed { id }
            } else {
                Response::Error {
                    message: format!("sesión desconocida: {id}"),
                }
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

    let state = Arc::new(Mutex::new(DaemonState::new()));
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
                    } else {
                        incremental_targets = state.incremental_scrollback_targets();
                    }
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
                } else {
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
            let idle = state
                .lock()
                .map(|state| state.should_shutdown(Instant::now()))
                .unwrap_or(false);
            let asked = shutdown.lock().map(|flag| *flag).unwrap_or(false);
            if idle || asked {
                let _ = std::fs::remove_file(&socket);
                std::process::exit(0);
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
                                    break;
                                }
                            }
                        });
                        let _ = write_response(
                            &writer,
                            &Response::Welcome {
                                version: PROTOCOL_VERSION,
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
        let mut response_written = false;
        let response = match state.lock() {
            Ok(mut state) => {
                let response = handle_request_for_client(
                    &mut state,
                    request,
                    Some(&scheduler),
                    authenticated_client_id.unwrap_or_default(),
                );
                if matches!(response, Response::Attached { .. }) {
                    // Escribir snapshot y registrar la suscripción mientras el
                    // pump está excluido evita tanto un evento adelantado como
                    // un hueco de salida entre attach y suscripción.
                    response_written = write_response(&writer, &response).is_ok();
                    if let (Some(session_id), Some(sender)) = (attach_id, &event_sender) {
                        let disconnect = writer
                            .lock()
                            .ok()
                            .and_then(|stream| stream.try_clone().ok());
                        state.subscribe(connection_id, session_id, sender.clone(), disconnect);
                    }
                }
                response
            }
            Err(_) => Response::Error {
                message: "estado del daemon envenenado".to_owned(),
            },
        };
        if !response_written {
            let _ = write_response(&writer, &response);
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

fn write_response(writer: &Arc<Mutex<UnixStream>>, response: &Response) -> std::io::Result<()> {
    let mut writer = writer
        .lock()
        .map_err(|_| std::io::Error::other("escritor del daemon envenenado"))?;
    writer.write_all(encode_line(response).as_bytes())?;
    writer.flush()
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
    fn spawning_registers_the_session() {
        let mut state = DaemonState::new();
        let id = spawn_one(&mut state);
        assert_eq!(state.session_ids(), vec![id]);
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

        // El cliente no lista `focused` como viva → la mata reconcile.
        state.reconcile_live(Uuid::new_v4(), &[other]);
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
