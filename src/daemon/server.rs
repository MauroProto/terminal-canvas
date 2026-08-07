//! Servidor del daemon (P3.15, T2 y T5): tiene los PTYs y los sobrevive a la
//! app. Cada conexión habla NDJSON; los eventos de salida se numeran con un
//! `seq` monotónico por sesión para que el reattach pueda deduplicar (T4).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::protocol::{
    decode_line, encode_line, handshake_ok, Request, Response, WireSpec, PROTOCOL_VERSION,
};

/// Cuánto espera el daemon sin ninguna app conectada antes de apagarse si
/// además no le quedan sesiones (adoption timeout).
pub const ADOPTION_TIMEOUT: Duration = Duration::from_secs(120);

/// Historial que el daemon guarda por sesión para el reattach caliente.
/// Es un tope duro: el checkpoint completo vive en el store de scrollback.
pub const MAX_SNAPSHOT_BYTES: usize = 256 * 1024;

/// Estado de una sesión desde la vista del daemon.
pub struct DaemonSession {
    pub spec: WireSpec,
    /// Todo lo que salió, recortado al tope, para el snapshot del reattach.
    pub snapshot: String,
    /// Último `seq` emitido.
    pub seq: u64,
    pub alive: bool,
}

impl DaemonSession {
    fn new(spec: WireSpec) -> Self {
        Self {
            spec,
            snapshot: String::new(),
            seq: 0,
            alive: true,
        }
    }

    /// Registra salida nueva y devuelve el `seq` que le toca.
    pub fn push_output(&mut self, data: &str) -> u64 {
        self.seq += 1;
        self.snapshot.push_str(data);
        clamp_snapshot(&mut self.snapshot, MAX_SNAPSHOT_BYTES);
        self.seq
    }
}

/// Recorta el snapshot conservando el final y alineado a borde de carácter.
pub fn clamp_snapshot(snapshot: &mut String, max_bytes: usize) {
    if snapshot.len() <= max_bytes {
        return;
    }
    let mut cut = snapshot.len() - max_bytes;
    while cut < snapshot.len() && !snapshot.is_char_boundary(cut) {
        cut += 1;
    }
    snapshot.drain(..cut);
}

/// Registro de sesiones del daemon. Sin I/O: testeable de punta a punta.
#[derive(Default)]
pub struct DaemonState {
    sessions: HashMap<Uuid, DaemonSession>,
    /// Conexiones de app abiertas ahora mismo.
    pub clients: usize,
    /// Desde cuándo no hay ninguna app conectada.
    idle_since: Option<Instant>,
}

impl DaemonState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn(&mut self, spec: WireSpec) -> Uuid {
        let id = Uuid::new_v4();
        self.sessions.insert(id, DaemonSession::new(spec));
        id
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
        self.sessions.remove(&id).is_some()
    }

    /// Mata las sesiones que la app ya no reconoce (T5): si un panel se fue,
    /// su PTY no puede quedar corriendo para siempre.
    pub fn reconcile_live(&mut self, live: &[Uuid]) -> Vec<Uuid> {
        let orphans: Vec<Uuid> = self
            .sessions
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        for id in &orphans {
            self.sessions.remove(id);
        }
        let mut sorted = orphans;
        sorted.sort_by_key(Uuid::as_u128);
        sorted
    }

    pub fn client_connected(&mut self) {
        self.clients += 1;
        self.idle_since = None;
    }

    pub fn client_disconnected(&mut self, now: Instant) {
        self.clients = self.clients.saturating_sub(1);
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

/// Aplica un pedido ya autenticado y devuelve la respuesta. Puro respecto del
/// socket: toda la lógica del daemon se testea por acá.
pub fn handle_request(state: &mut DaemonState, request: Request) -> Response {
    match request {
        Request::Hello { .. } => Response::Error {
            message: "handshake repetido".to_owned(),
        },
        Request::Spawn { spec } => Response::Spawned {
            id: state.spawn(spec),
        },
        Request::Attach { id } => match state.session(id) {
            Some(session) => Response::Attached {
                id,
                snapshot: session.snapshot.clone(),
                seq: session.seq,
            },
            None => Response::Error {
                message: format!("sesión desconocida: {id}"),
            },
        },
        Request::Write { id, data } => match state.session_mut(id) {
            // El eco real lo produce el PTY; acá solo se valida que exista.
            Some(_) => {
                let _ = data;
                Response::Sessions {
                    ids: state.session_ids(),
                }
            }
            None => Response::Error {
                message: format!("sesión desconocida: {id}"),
            },
        },
        Request::Resize { id, cols, rows } => match state.session_mut(id) {
            Some(session) => {
                session.spec.cols = cols;
                session.spec.rows = rows;
                Response::Sessions {
                    ids: state.session_ids(),
                }
            }
            None => Response::Error {
                message: format!("sesión desconocida: {id}"),
            },
        },
        Request::Kill { id } => {
            if state.kill(id) {
                Response::Killed { id }
            } else {
                Response::Error {
                    message: format!("sesión desconocida: {id}"),
                }
            }
        }
        Request::List => Response::Sessions {
            ids: state.session_ids(),
        },
        Request::ReconcileLive { ids } => Response::Reconciled {
            killed: state.reconcile_live(&ids),
        },
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
    // Un socket viejo de un daemon muerto impide el bind.
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    super::protocol::restrict_to_owner(&socket)?;

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let shutdown = Arc::new(Mutex::new(false));

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
        let token = token.clone();
        std::thread::spawn(move || {
            serve_connection(stream, state, shutdown, token);
        });
    }
    Ok(())
}

fn serve_connection(
    stream: UnixStream,
    state: Arc<Mutex<DaemonState>>,
    shutdown: Arc<Mutex<bool>>,
    token: String,
) {
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let mut writer = write_half;
    let reader = BufReader::new(stream);
    let mut authenticated = false;

    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Some(request) = decode_line::<Request>(&line) else {
            let _ = writer.write_all(
                encode_line(&Response::Error {
                    message: "mensaje ilegible".to_owned(),
                })
                .as_bytes(),
            );
            continue;
        };

        // Sin handshake válido no se atiende nada más.
        if !authenticated {
            match request {
                Request::Hello {
                    version,
                    token: provided,
                } => {
                    if handshake_ok(version, &provided, &token) {
                        authenticated = true;
                        if let Ok(mut state) = state.lock() {
                            state.client_connected();
                        }
                        let _ = writer.write_all(
                            encode_line(&Response::Welcome {
                                version: PROTOCOL_VERSION,
                            })
                            .as_bytes(),
                        );
                    } else {
                        let _ = writer.write_all(
                            encode_line(&Response::Error {
                                message: "handshake rechazado".to_owned(),
                            })
                            .as_bytes(),
                        );
                        break;
                    }
                }
                _ => {
                    let _ = writer.write_all(
                        encode_line(&Response::Error {
                            message: "falta el handshake".to_owned(),
                        })
                        .as_bytes(),
                    );
                    break;
                }
            }
            continue;
        }

        let asked_shutdown = matches!(request, Request::ShutdownIfIdle);
        let response = match state.lock() {
            Ok(mut state) => handle_request(&mut state, request),
            Err(_) => Response::Error {
                message: "estado del daemon envenenado".to_owned(),
            },
        };
        let _ = writer.write_all(encode_line(&response).as_bytes());
        let _ = writer.flush();

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

    if authenticated {
        if let Ok(mut state) = state.lock() {
            state.client_disconnected(Instant::now());
        }
    }
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
    };
    use crate::daemon::protocol::{Request, Response, WireSpec};
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    fn spawn_one(state: &mut DaemonState) -> Uuid {
        match handle_request(
            state,
            Request::Spawn {
                spec: WireSpec::default(),
            },
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
    fn attaching_returns_the_snapshot_and_the_current_seq() {
        let mut state = DaemonState::new();
        let id = spawn_one(&mut state);
        let seq = state.session_mut(id).unwrap().push_output("hola\n");
        state.session_mut(id).unwrap().push_output("chau\n");

        match handle_request(&mut state, Request::Attach { id }) {
            Response::Attached {
                snapshot, seq: at, ..
            } => {
                assert_eq!(snapshot, "hola\nchau\n");
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
        let first = session.push_output("a");
        let second = session.push_output("b");
        let third = session.push_output("c");
        assert!(first < second && second < third, "{first} {second} {third}");
    }

    #[test]
    fn attaching_an_unknown_session_is_an_error_not_a_panic() {
        let mut state = DaemonState::new();
        match handle_request(&mut state, Request::Attach { id: Uuid::new_v4() }) {
            Response::Error { message } => assert!(message.contains("desconocida")),
            other => panic!("esperaba Error, got {other:?}"),
        }
    }

    #[test]
    fn killing_removes_the_session() {
        let mut state = DaemonState::new();
        let id = spawn_one(&mut state);
        assert!(matches!(
            handle_request(&mut state, Request::Kill { id }),
            Response::Killed { .. }
        ));
        assert!(state.session_ids().is_empty());
        // Matarla de nuevo es un error, no un panic.
        assert!(matches!(
            handle_request(&mut state, Request::Kill { id }),
            Response::Error { .. }
        ));
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
        );
        let session = state.session(id).unwrap();
        assert_eq!((session.spec.cols, session.spec.rows), (200, 60));
    }

    #[test]
    fn reconcile_kills_only_the_orphans() {
        let mut state = DaemonState::new();
        let keep = spawn_one(&mut state);
        let orphan = spawn_one(&mut state);
        match handle_request(&mut state, Request::ReconcileLive { ids: vec![keep] }) {
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
        match handle_request(&mut state, Request::ReconcileLive { ids: live.clone() }) {
            Response::Reconciled { killed } => assert!(killed.is_empty()),
            other => panic!("got {other:?}"),
        }
        assert_eq!(state.session_ids(), live);
    }

    #[test]
    fn a_daemon_with_live_sessions_never_shuts_down() {
        let mut state = DaemonState::new();
        spawn_one(&mut state);
        state.client_connected();
        let now = Instant::now();
        state.client_disconnected(now);
        assert!(
            !state.should_shutdown(now + ADOPTION_TIMEOUT * 10),
            "con sesiones vivas el daemon se queda: para eso existe"
        );
    }

    #[test]
    fn an_empty_daemon_shuts_down_after_the_adoption_timeout() {
        let mut state = DaemonState::new();
        state.client_connected();
        let now = Instant::now();
        state.client_disconnected(now);
        assert!(!state.should_shutdown(now), "todavía no");
        assert!(
            !state.should_shutdown(now + ADOPTION_TIMEOUT - Duration::from_secs(1)),
            "todavía dentro de la ventana de adopción"
        );
        assert!(state.should_shutdown(now + ADOPTION_TIMEOUT));
    }

    #[test]
    fn a_daemon_with_a_client_connected_never_shuts_down() {
        let mut state = DaemonState::new();
        state.client_connected();
        assert!(!state.should_shutdown(Instant::now() + ADOPTION_TIMEOUT * 10));
    }

    #[test]
    fn reconnecting_cancels_the_shutdown_countdown() {
        let mut state = DaemonState::new();
        state.client_connected();
        let now = Instant::now();
        state.client_disconnected(now);
        state.client_connected();
        state.client_disconnected(now + Duration::from_secs(1));
        // El reloj se reinicia con la reconexión.
        assert!(!state.should_shutdown(now + ADOPTION_TIMEOUT));
    }

    #[test]
    fn the_snapshot_is_clamped_keeping_the_tail() {
        let mut snapshot = "viejo".repeat(1000);
        snapshot.push_str("RECIENTE");
        clamp_snapshot(&mut snapshot, 32);
        assert!(snapshot.len() <= 32, "got {}", snapshot.len());
        assert!(snapshot.ends_with("RECIENTE"), "se conserva el final");
    }

    #[test]
    fn clamping_never_splits_a_multibyte_character() {
        for max in 1..40 {
            let mut snapshot = "ñ".repeat(20);
            clamp_snapshot(&mut snapshot, max);
            // Si cortara al medio, el String sería inválido y esto paniquearía.
            assert!(snapshot.chars().all(|ch| ch == 'ñ'), "got {snapshot:?}");
        }
    }

    #[test]
    fn output_beyond_the_cap_does_not_grow_the_snapshot() {
        let mut state = DaemonState::new();
        let id = spawn_one(&mut state);
        let session = state.session_mut(id).unwrap();
        for _ in 0..50 {
            session.push_output(&"x".repeat(16 * 1024));
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
            },
        );
        assert!(matches!(response, Response::Error { .. }));
    }
}
