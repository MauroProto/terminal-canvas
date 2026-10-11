//! Cliente del daemon (P3.15, T3 y T4).
//!
//! `DaemonConn` habla NDJSON por el unix socket. Si el daemon no está
//! (`ECONNREFUSED` o socket ausente) lo levanta con `fork+setsid` y reintenta
//! **una sola vez**: un loop de respawn sería peor que no tener daemon.
//!
//! Si aun así no arranca, quien llama cae al modo in-process (el
//! `PtyManager` de siempre): el daemon es una mejora, no un requisito.

use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::protocol::{
    decode_line, encode_line, read_protocol_line, socket_path, BirthCapabilities, Request,
    Response, PROTOCOL_VERSION,
};

/// Cuánto se espera a que el daemon recién lanzado abra el socket.
pub const SPAWN_WAIT: Duration = Duration::from_secs(5);

/// ¿Hay que descartar este evento porque ya venía en el snapshot del attach?
/// (T4) El daemon numera la salida; al reengancharse, todo lo que tenga
/// `seq` ≤ el del snapshot ya se vio.
pub fn is_duplicate_after_attach(event_seq: u64, attached_seq: u64) -> bool {
    event_seq <= attached_seq
}

/// ¿Esta respuesta es un **evento empujado** por el daemon (y no la respuesta
/// a un pedido)? El daemon manda `Output`/`Exit` cuando quiere, así que se
/// mezclan con las respuestas en el mismo socket.
pub fn is_pushed_event(response: &Response) -> bool {
    matches!(
        response,
        Response::Output { .. } | Response::Exit { .. } | Response::InputError { .. }
    )
}

pub struct DaemonConn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    client_id: Uuid,
    /// Eventos que llegaron mientras se esperaba la respuesta a un pedido.
    /// Sin esta cola, un `Output` en el momento equivocado se leía como si
    /// fuera la respuesta y el pedido devolvía basura.
    pending_events: std::collections::VecDeque<Response>,
    pending_event_bytes: usize,
    invalid: bool,
}

impl DaemonConn {
    /// Conecta al daemon, levantándolo si hace falta. `None` si no se pudo
    /// (quien llama cae al modo in-process).
    pub fn connect(dir: &Path, token: &str, daemon_binary: &Path) -> Option<Self> {
        Self::connect_as(dir, token, daemon_binary, Uuid::new_v4())
    }

    pub fn connect_as(
        dir: &Path,
        token: &str,
        daemon_binary: &Path,
        client_id: Uuid,
    ) -> Option<Self> {
        if let Some(conn) = Self::try_connect_as(dir, token, client_id) {
            return Some(conn);
        }
        // Un solo reintento, después de levantarlo.
        spawn_daemon(daemon_binary, dir).ok()?;
        let deadline = Instant::now() + SPAWN_WAIT;
        while Instant::now() < deadline {
            if let Some(conn) = Self::try_connect_as(dir, token, client_id) {
                return Some(conn);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    /// Conexión cruda ya autenticada, para atar una sesión remota (T3).
    ///
    /// Se devuelve el stream pelado a propósito: el `PtyHandle` remoto usa el
    /// **mismo** socket para escribir input y para leer sus eventos `Output`,
    /// así no hace falta multiplexar ni drenar una segunda conexión.
    pub fn connect_raw(dir: &Path, token: &str) -> Option<UnixStream> {
        Self::connect_raw_as(dir, token, Uuid::new_v4())
    }

    pub fn connect_raw_as(dir: &Path, token: &str, client_id: Uuid) -> Option<UnixStream> {
        Self::connect_raw_with_capabilities_as(dir, token, client_id).map(|(stream, _)| stream)
    }

    pub fn connect_raw_with_capabilities_as(
        dir: &Path,
        token: &str,
        client_id: Uuid,
    ) -> Option<(UnixStream, BirthCapabilities)> {
        let stream = UnixStream::connect(socket_path(dir)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .ok()?;
        let mut writer = stream.try_clone().ok()?;
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        writer
            .write_all(
                encode_line(&Request::Hello {
                    version: PROTOCOL_VERSION,
                    token: token.to_owned(),
                    client_id,
                })
                .as_bytes(),
            )
            .ok()?;
        writer.flush().ok()?;
        let line = read_protocol_line(&mut reader).ok()??;
        match decode_line::<Response>(&line) {
            Some(Response::Welcome {
                version,
                birth_idempotent,
                birth_cancel_safe,
                hook_birth_authority,
            }) if version == PROTOCOL_VERSION => Some((
                stream,
                BirthCapabilities {
                    idempotent: birth_idempotent,
                    cancel_safe: birth_cancel_safe,
                    hook_authority: hook_birth_authority,
                },
            )),
            _ => None,
        }
    }

    /// Conecta sin levantar nada.
    pub fn try_connect(dir: &Path, token: &str) -> Option<Self> {
        Self::try_connect_as(dir, token, Uuid::new_v4())
    }

    pub fn try_connect_as(dir: &Path, token: &str, client_id: Uuid) -> Option<Self> {
        let stream = UnixStream::connect(socket_path(dir)).ok()?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .ok()?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .ok()?;
        let writer = stream.try_clone().ok()?;
        let mut conn = Self {
            reader: BufReader::new(stream),
            writer,
            client_id,
            pending_events: std::collections::VecDeque::new(),
            pending_event_bytes: 0,
            invalid: false,
        };
        match conn.request(&Request::Hello {
            version: PROTOCOL_VERSION,
            token: token.to_owned(),
            client_id,
        }) {
            Some(Response::Welcome { version, .. }) if version == PROTOCOL_VERSION => Some(conn),
            _ => None,
        }
    }

    /// Compatibility wrapper. None is unavailable, never proof of absence.
    pub fn request(&mut self, request: &Request) -> Option<Response> {
        self.request_checked(request).ok()
    }

    /// A framing or transport error retires the stream so a late reply can
    /// never be mistaken for the response to a subsequent request.
    pub fn request_checked(&mut self, request: &Request) -> std::io::Result<Response> {
        if self.invalid {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "La conexión al daemon ya no es válida.",
            ));
        }
        let result = self.request_on_stream(request);
        if result.is_err() {
            self.invalid = true;
            let _ = self.writer.shutdown(std::net::Shutdown::Both);
        }
        result
    }

    fn request_on_stream(&mut self, request: &Request) -> std::io::Result<Response> {
        self.writer.write_all(encode_line(request).as_bytes())?;
        self.writer.flush()?;
        loop {
            let line = read_protocol_line(&mut self.reader)?.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "El daemon cerró la conexión.",
                )
            })?;
            let response = decode_line::<Response>(&line).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "El daemon devolvió una línea inválida.",
                )
            })?;
            if is_pushed_event(&response) {
                let bytes = match &response {
                    Response::Output { data, .. } => data.len(),
                    Response::InputError { message, .. } => message.len(),
                    _ => 0,
                };
                // The queue is bounded by count and decoded payload bytes.
                // Closing the stream triggers recovery rather than silently
                // losing sequence-bearing events.
                if self.pending_events.len() >= 128
                    || self.pending_event_bytes.saturating_add(bytes) > 8 * 1024 * 1024
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "La cola de eventos del daemon superó su presupuesto.",
                    ));
                }
                self.pending_event_bytes += bytes;
                self.pending_events.push_back(response);
                continue;
            }
            return Ok(response);
        }
    }

    /// Eventos que llegaron mientras se atendían pedidos.
    pub fn drain_events(&mut self) -> Vec<Response> {
        self.pending_event_bytes = 0;
        self.pending_events.drain(..).collect()
    }

    pub fn spawn_session(&mut self, spec: super::protocol::WireSpec) -> Option<Uuid> {
        let proposed_id = Uuid::new_v4();
        match self.request(&Request::Spawn {
            spec,
            id: Some(proposed_id),
        })? {
            Response::Spawned { id } if id == proposed_id => Some(id),
            _ => None,
        }
    }

    /// Reattach caliente: devuelve el historial ya visto y el `seq` desde el
    /// cual los eventos son nuevos.
    pub fn attach(&mut self, id: Uuid) -> Option<(Vec<u8>, u64)> {
        match self.request(&Request::Attach { id })? {
            Response::Attached {
                id: actual,
                snapshot,
                seq,
                ..
            } if actual == id => Some((snapshot, seq)),
            _ => None,
        }
    }

    /// Legacy presentation helper. Empty may mean unavailable; creation and
    /// recovery decisions must use list_checked instead.
    pub fn list(&mut self) -> Vec<Uuid> {
        self.list_checked().unwrap_or_default()
    }

    pub fn list_checked(&mut self) -> std::io::Result<Vec<Uuid>> {
        match self.request_checked(&Request::List)? {
            Response::Sessions { ids } => Ok(ids),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "El daemon no confirmó la lista de sesiones.",
            )),
        }
    }

    pub fn kill(&mut self, id: Uuid) -> bool {
        matches!(
            self.request(&Request::Kill { id }),
            Some(Response::Killed { id: actual }) if actual == id
        )
    }

    pub fn set_priority_session(&mut self, id: Option<Uuid>) -> bool {
        matches!(
            self.request(&Request::SetPriority { id }),
            Some(Response::Sessions { .. })
        )
    }

    pub fn reconcile_live(&mut self, ids: Vec<Uuid>) -> Vec<Uuid> {
        match self.request(&Request::ReconcileLive { ids }) {
            Some(Response::Reconciled { killed }) => killed,
            _ => Vec::new(),
        }
    }

    pub fn client_id(&self) -> Uuid {
        self.client_id
    }
}

/// Lanza el daemon desacoplado de la app: `setsid` para que no muera con
/// nosotros ni herede el terminal.
pub fn spawn_daemon(binary: &Path, dir: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut command = Command::new(binary);
    command
        .env("MI_TERMINAL_DAEMON_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid() en el hijo, entre fork y exec, es async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()?;
    Ok(())
}

/// Path del binario del daemon, asumiéndolo al lado del ejecutable actual
/// (es como quedan en el bundle y en `target/`).
pub fn daemon_binary_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.with_file_name("mi-terminal-daemon"))
}

#[cfg(test)]
mod tests {
    use super::{daemon_binary_path, is_duplicate_after_attach, DaemonConn};
    use std::path::Path;

    use crate::daemon::protocol::Response as WireResponse;
    #[allow(unused_imports)]
    use WireResponse as _;

    #[test]
    fn events_already_in_the_snapshot_are_dropped() {
        // El attach devolvió hasta el seq 10: todo lo ≤ 10 ya se vio.
        assert!(is_duplicate_after_attach(1, 10));
        assert!(is_duplicate_after_attach(10, 10));
        assert!(!is_duplicate_after_attach(11, 10));
    }

    #[test]
    fn a_fresh_attach_drops_nothing() {
        assert!(!is_duplicate_after_attach(1, 0));
    }

    #[test]
    fn connecting_without_a_daemon_yields_none_instead_of_hanging() {
        let dir = std::env::temp_dir().join(format!("daemon-none-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            DaemonConn::try_connect(&dir, "tok").is_none(),
            "sin socket no hay conexión"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_daemon_binary_sits_next_to_the_app() {
        let path = daemon_binary_path().expect("hay current_exe en tests");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("mi-terminal-daemon")
        );
        assert!(path.parent().is_some());
    }

    #[test]
    fn spawning_a_missing_binary_fails_cleanly() {
        let missing = Path::new("/definitivamente/no/existe/mi-terminal-daemon");
        let dir = std::env::temp_dir();
        assert!(super::spawn_daemon(missing, &dir).is_err());
    }

    #[test]
    fn pushed_events_are_told_apart_from_responses() {
        use crate::daemon::protocol::Response;
        use uuid::Uuid;
        let id = Uuid::new_v4();
        // Lo que el daemon empuja cuando quiere.
        assert!(super::is_pushed_event(&Response::Output {
            id,
            seq: 1,
            data: b"x".to_vec()
        }));
        assert!(super::is_pushed_event(&Response::Exit { id }));
        // Lo que contesta a un pedido.
        assert!(!super::is_pushed_event(&Response::Sessions {
            ids: vec![id]
        }));
        assert!(!super::is_pushed_event(&Response::Spawned { id }));
        assert!(!super::is_pushed_event(&Response::ShuttingDown));
        assert!(!super::is_pushed_event(&Response::Welcome {
            version: 1,
            birth_idempotent: false,
            birth_cancel_safe: false,
            hook_birth_authority: false,
        }));
    }
}
