//! Protocolo del daemon de PTYs (P3.15, T1).
//!
//! NDJSON sobre un unix socket: una línea = un mensaje. Se eligió NDJSON y no
//! un binario propio porque el daemon es un proceso separado que sobrevive a
//! la app y hay que poder debuggearlo con `nc` cuando algo va mal.
//!
//! La **versión va en el nombre del socket** (`daemon-v1.sock`): un daemon
//! viejo y una app nueva no se pueden ni conectar, en vez de hablar mal.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Versión del protocolo. Cambiarla obliga a un socket nuevo.
pub const PROTOCOL_VERSION: u32 = 1;

/// Nombre del socket, con la versión adentro.
pub fn socket_file_name() -> String {
    format!("daemon-v{PROTOCOL_VERSION}.sock")
}

/// Directorio donde viven socket, token y pid-file.
pub fn daemon_dir() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "terminal-app")?;
    Some(dirs.data_dir().join("daemon"))
}

pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join(socket_file_name())
}

/// Tope del path de un unix socket. En macOS `sun_path` son 104 bytes y en
/// Linux 108: se usa el más chico para que un directorio que funciona en uno
/// funcione en el otro.
pub const MAX_SOCKET_PATH_BYTES: usize = 104;

/// ¿El path del socket entra en `sun_path`? Un path largo hace fallar el
/// `bind` con un error que no dice nada útil ("invalid argument"), así que se
/// chequea antes para poder explicarlo.
pub fn socket_path_fits(path: &Path) -> bool {
    path.as_os_str().len() < MAX_SOCKET_PATH_BYTES
}

pub fn token_path(dir: &Path) -> PathBuf {
    dir.join("token")
}

pub fn pid_path(dir: &Path) -> PathBuf {
    dir.join("daemon.pid")
}

/// Spec de sesión que viaja por el socket (espejo de `runtime::SessionSpec`,
/// desacoplado a propósito: el protocolo no puede romperse porque el struct
/// interno gane un campo).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSpec {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub startup_command: Option<String>,
    #[serde(default)]
    pub panel_id: Option<Uuid>,
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

fn default_cols() -> u16 {
    80
}

fn default_rows() -> u16 {
    24
}

/// Pedido de la app al daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Handshake. Sin un `Hello` válido no se acepta ningún otro mensaje.
    Hello {
        version: u32,
        token: String,
    },
    Spawn {
        spec: WireSpec,
    },
    /// Reengancharse a una sesión viva; devuelve snapshot + seq actual.
    Attach {
        id: Uuid,
    },
    Write {
        id: Uuid,
        data: String,
    },
    Resize {
        id: Uuid,
        cols: u16,
        rows: u16,
    },
    Kill {
        id: Uuid,
    },
    List,
    /// Paneles que la app todavía tiene vivos; el daemon mata los huérfanos.
    ReconcileLive {
        ids: Vec<Uuid>,
    },
    /// Apagate si no te queda nada que hacer.
    ShutdownIfIdle,
}

/// Respuesta o evento del daemon a la app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Handshake aceptado.
    Welcome {
        version: u32,
    },
    Spawned {
        id: Uuid,
    },
    /// Estado de una sesión al reengancharse: el historial ya visto y el `seq`
    /// hasta el que llega. Los eventos con `seq` ≤ este se descartan.
    Attached {
        id: Uuid,
        snapshot: String,
        seq: u64,
    },
    Sessions {
        ids: Vec<Uuid>,
    },
    /// Salida nueva de una sesión.
    Output {
        id: Uuid,
        seq: u64,
        data: String,
    },
    Exit {
        id: Uuid,
    },
    Killed {
        id: Uuid,
    },
    Reconciled {
        killed: Vec<Uuid>,
    },
    ShuttingDown,
    Error {
        message: String,
    },
}

/// Serializa un mensaje como una línea NDJSON (con el `\n` final).
pub fn encode_line<T: Serialize>(message: &T) -> String {
    let mut line = serde_json::to_string(message).unwrap_or_else(|_| "{}".to_owned());
    line.push('\n');
    line
}

/// Parsea una línea NDJSON. `None` si está vacía o no es del tipo esperado:
/// una línea corrupta no puede tumbar la conexión.
pub fn decode_line<T: for<'de> Deserialize<'de>>(line: &str) -> Option<T> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    serde_json::from_str(line).ok()
}

/// ¿El handshake es aceptable? Versión exacta y token igual.
pub fn handshake_ok(version: u32, token: &str, expected_token: &str) -> bool {
    if version != PROTOCOL_VERSION {
        return false;
    }
    if expected_token.is_empty() || token.len() != expected_token.len() {
        return false;
    }
    // Sin early-return por byte, igual que el token de los hooks.
    token
        .bytes()
        .zip(expected_token.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Crea (o reusa) el token del daemon con permisos 0600.
pub fn ensure_token(dir: &Path) -> std::io::Result<String> {
    std::fs::create_dir_all(dir)?;
    let path = token_path(dir);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_owned();
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    let token = Uuid::new_v4().simple().to_string();
    std::fs::write(&path, &token)?;
    restrict_to_owner(&path)?;
    Ok(token)
}

/// 0600: el token del daemon no puede ser legible por otros usuarios del
/// equipo, porque con él se pueden escribir bytes en cualquier terminal.
pub fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Contenido del pid-file: pid y un nonce, para distinguir un daemon nuestro
/// de un proceso cualquiera que reusó el pid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PidFile {
    pub pid: u32,
    pub nonce: String,
}

impl PidFile {
    pub fn new(pid: u32) -> Self {
        Self {
            pid,
            nonce: Uuid::new_v4().simple().to_string(),
        }
    }
}

pub fn write_pid_file(dir: &Path, pid_file: &PidFile) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = pid_path(dir);
    std::fs::write(&path, encode_line(pid_file))?;
    restrict_to_owner(&path)
}

pub fn read_pid_file(dir: &Path) -> Option<PidFile> {
    let raw = std::fs::read_to_string(pid_path(dir)).ok()?;
    decode_line(&raw)
}

#[cfg(test)]
mod tests {
    use super::{
        decode_line, encode_line, ensure_token, handshake_ok, read_pid_file, socket_file_name,
        write_pid_file, PidFile, Request, Response, WireSpec, PROTOCOL_VERSION,
    };
    use uuid::Uuid;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("daemon-{tag}-{}", Uuid::new_v4()))
    }

    #[test]
    fn the_socket_name_carries_the_protocol_version() {
        assert_eq!(
            socket_file_name(),
            format!("daemon-v{PROTOCOL_VERSION}.sock")
        );
    }

    #[test]
    fn requests_round_trip_through_ndjson() {
        let id = Uuid::new_v4();
        let cases = vec![
            Request::Hello {
                version: PROTOCOL_VERSION,
                token: "abc".to_owned(),
            },
            Request::Spawn {
                spec: WireSpec {
                    title: "Terminal".to_owned(),
                    cwd: Some("/tmp".to_owned()),
                    startup_command: Some("claude".to_owned()),
                    panel_id: Some(id),
                    workspace_id: None,
                    cols: 120,
                    rows: 40,
                },
            },
            Request::Attach { id },
            Request::Write {
                id,
                data: "echo hola\n".to_owned(),
            },
            Request::Resize {
                id,
                cols: 100,
                rows: 30,
            },
            Request::Kill { id },
            Request::List,
            Request::ReconcileLive { ids: vec![id] },
            Request::ShutdownIfIdle,
        ];
        for case in cases {
            let line = encode_line(&case);
            assert!(line.ends_with('\n'), "NDJSON necesita el salto: {line:?}");
            assert_eq!(line.lines().count(), 1, "un mensaje = una línea");
            let back: Request = decode_line(&line).expect("decodifica");
            assert_eq!(back, case);
        }
    }

    #[test]
    fn responses_round_trip_through_ndjson() {
        let id = Uuid::new_v4();
        let cases = vec![
            Response::Welcome {
                version: PROTOCOL_VERSION,
            },
            Response::Spawned { id },
            Response::Attached {
                id,
                snapshot: "hola\n".to_owned(),
                seq: 42,
            },
            Response::Sessions { ids: vec![id] },
            Response::Output {
                id,
                seq: 43,
                data: "salida".to_owned(),
            },
            Response::Exit { id },
            Response::Killed { id },
            Response::Reconciled { killed: vec![id] },
            Response::ShuttingDown,
            Response::Error {
                message: "algo".to_owned(),
            },
        ];
        for case in cases {
            let back: Response = decode_line(&encode_line(&case)).expect("decodifica");
            assert_eq!(back, case);
        }
    }

    #[test]
    fn payloads_with_newlines_stay_on_one_line() {
        // Lo que rompería NDJSON: bytes con \n adentro del payload.
        let message = Response::Output {
            id: Uuid::new_v4(),
            seq: 1,
            data: "linea1\nlinea2\r\n".to_owned(),
        };
        let line = encode_line(&message);
        assert_eq!(line.lines().count(), 1, "got {line:?}");
        assert_eq!(decode_line::<Response>(&line), Some(message));
    }

    #[test]
    fn a_corrupt_line_decodes_to_none_instead_of_panicking() {
        assert_eq!(decode_line::<Request>("no json").as_ref(), None);
        assert_eq!(decode_line::<Request>("").as_ref(), None);
        assert_eq!(decode_line::<Request>("{}").as_ref(), None);
        assert_eq!(decode_line::<Request>(r#"{"type":"nope"}"#).as_ref(), None);
    }

    #[test]
    fn the_handshake_needs_the_exact_version_and_token() {
        assert!(handshake_ok(PROTOCOL_VERSION, "tok", "tok"));
        assert!(
            !handshake_ok(PROTOCOL_VERSION + 1, "tok", "tok"),
            "otra versión"
        );
        assert!(!handshake_ok(PROTOCOL_VERSION, "otro", "tok"));
        assert!(!handshake_ok(PROTOCOL_VERSION, "tokk", "tok"), "otro largo");
        assert!(!handshake_ok(PROTOCOL_VERSION, "", ""), "token vacío nunca");
    }

    #[test]
    fn the_token_is_created_once_and_is_owner_only() {
        let dir = temp_dir("token");
        let first = ensure_token(&dir).expect("crea");
        assert!(!first.is_empty());
        let second = ensure_token(&dir).expect("reusa");
        assert_eq!(first, second, "no se rota en cada arranque");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(super::token_path(&dir))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "el token no puede ser legible por otros"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pid_file_round_trips_with_its_nonce() {
        let dir = temp_dir("pid");
        let pid_file = PidFile::new(4242);
        write_pid_file(&dir, &pid_file).expect("escribe");
        assert_eq!(read_pid_file(&dir), Some(pid_file));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_pid_files_never_share_a_nonce() {
        // Sin esto, un pid reciclado por el SO parecería nuestro daemon.
        assert_ne!(PidFile::new(1).nonce, PidFile::new(1).nonce);
    }

    #[test]
    fn a_missing_pid_file_is_none() {
        assert_eq!(read_pid_file(&temp_dir("nada")), None);
    }

    #[test]
    fn a_socket_path_that_does_not_fit_sun_path_is_rejected_early() {
        use super::{socket_path, socket_path_fits, MAX_SOCKET_PATH_BYTES};
        // El caso real que rompió los tests: un temp dir con un uuid adentro
        // pasa los 104 bytes y el bind falla con "invalid argument".
        let long_dir = std::path::PathBuf::from("/var/folders/6l/2ry6qzt53m5490n6crgltkqw0000gn/T")
            .join(format!("daemon-e2e-{}", Uuid::new_v4()));
        let long = socket_path(&long_dir);
        assert!(
            long.as_os_str().len() >= MAX_SOCKET_PATH_BYTES,
            "el caso de prueba tiene que ser largo: {} bytes",
            long.as_os_str().len()
        );
        assert!(!socket_path_fits(&long));

        assert!(socket_path_fits(&socket_path(std::path::Path::new(
            "/tmp/tc"
        ))));
    }
}
