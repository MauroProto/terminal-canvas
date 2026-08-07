//! Cliente del daemon (P3.15, T3 y T4).
//!
//! `DaemonConn` habla NDJSON por el unix socket. Si el daemon no está
//! (`ECONNREFUSED` o socket ausente) lo levanta con `fork+setsid` y reintenta
//! **una sola vez**: un loop de respawn sería peor que no tener daemon.
//!
//! Si aun así no arranca, quien llama cae al modo in-process (el
//! `PtyManager` de siempre): el daemon es una mejora, no un requisito.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::protocol::{decode_line, encode_line, socket_path, Request, Response, PROTOCOL_VERSION};

/// Cuánto se espera a que el daemon recién lanzado abra el socket.
pub const SPAWN_WAIT: Duration = Duration::from_secs(5);

/// ¿Hay que descartar este evento porque ya venía en el snapshot del attach?
/// (T4) El daemon numera la salida; al reengancharse, todo lo que tenga
/// `seq` ≤ el del snapshot ya se vio.
pub fn is_duplicate_after_attach(event_seq: u64, attached_seq: u64) -> bool {
    event_seq <= attached_seq
}

pub struct DaemonConn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl DaemonConn {
    /// Conecta al daemon, levantándolo si hace falta. `None` si no se pudo
    /// (quien llama cae al modo in-process).
    pub fn connect(dir: &Path, token: &str, daemon_binary: &Path) -> Option<Self> {
        if let Some(conn) = Self::try_connect(dir, token) {
            return Some(conn);
        }
        // Un solo reintento, después de levantarlo.
        spawn_daemon(daemon_binary, dir).ok()?;
        let deadline = Instant::now() + SPAWN_WAIT;
        while Instant::now() < deadline {
            if let Some(conn) = Self::try_connect(dir, token) {
                return Some(conn);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    /// Conecta sin levantar nada.
    pub fn try_connect(dir: &Path, token: &str) -> Option<Self> {
        let stream = UnixStream::connect(socket_path(dir)).ok()?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .ok()?;
        let writer = stream.try_clone().ok()?;
        let mut conn = Self {
            reader: BufReader::new(stream),
            writer,
        };
        match conn.request(&Request::Hello {
            version: PROTOCOL_VERSION,
            token: token.to_owned(),
        }) {
            Some(Response::Welcome { version }) if version == PROTOCOL_VERSION => Some(conn),
            _ => None,
        }
    }

    /// Manda un pedido y espera la respuesta.
    pub fn request(&mut self, request: &Request) -> Option<Response> {
        self.writer
            .write_all(encode_line(request).as_bytes())
            .ok()?;
        self.writer.flush().ok()?;
        let mut line = String::new();
        self.reader.read_line(&mut line).ok()?;
        decode_line(&line)
    }

    pub fn spawn_session(&mut self, spec: super::protocol::WireSpec) -> Option<Uuid> {
        match self.request(&Request::Spawn { spec })? {
            Response::Spawned { id } => Some(id),
            _ => None,
        }
    }

    /// Reattach caliente: devuelve el historial ya visto y el `seq` desde el
    /// cual los eventos son nuevos.
    pub fn attach(&mut self, id: Uuid) -> Option<(String, u64)> {
        match self.request(&Request::Attach { id })? {
            Response::Attached { snapshot, seq, .. } => Some((snapshot, seq)),
            _ => None,
        }
    }

    pub fn list(&mut self) -> Vec<Uuid> {
        match self.request(&Request::List) {
            Some(Response::Sessions { ids }) => ids,
            _ => Vec::new(),
        }
    }

    pub fn kill(&mut self, id: Uuid) -> bool {
        matches!(
            self.request(&Request::Kill { id }),
            Some(Response::Killed { .. })
        )
    }

    pub fn reconcile_live(&mut self, ids: Vec<Uuid>) -> Vec<Uuid> {
        match self.request(&Request::ReconcileLive { ids }) {
            Some(Response::Reconciled { killed }) => killed,
            _ => Vec::new(),
        }
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
}
