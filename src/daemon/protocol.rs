//! Protocolo del daemon de PTYs (P3.15, T1).
//!
//! NDJSON sobre un unix socket: una línea = un mensaje. Se eligió NDJSON y no
//! un binario propio porque el daemon es un proceso separado que sobrevive a
//! la app y hay que poder debuggearlo con `nc` cuando algo va mal.
//!
//! La **versión va en el nombre del socket** (`daemon-v3.sock`): un daemon
//! viejo y una app nueva no se pueden ni conectar, en vez de hablar mal.

use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Versión del protocolo. Cambiarla obliga a un socket nuevo.
pub const PROTOCOL_VERSION: u32 = 4;

/// Tope duro de una línea NDJSON completa, incluido el salto final.
///
/// Tanto el socket como el token son locales, pero una conexión defectuosa o
/// un proceso del mismo usuario nunca debe poder hacer crecer un `String` sin
/// límite dentro de la app o del daemon. Ocho MiB deja margen amplio para
/// snapshots y ráfagas de salida, que hoy están acotados muy por debajo.
pub const MAX_PROTOCOL_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Tamaño máximo de cada bloque crudo de input que se encapsula en un
/// `Request::Write`. Mantenerlo pequeño garantiza que hasta un paste enorme se
/// divida en líneas holgadamente inferiores al límite del protocolo.
pub const MAX_WIRE_INPUT_BYTES: usize = 64 * 1024;

/// Nombre del socket, con la versión adentro.
pub fn socket_file_name() -> String {
    format!("daemon-v{PROTOCOL_VERSION}.sock")
}

/// Directorio donde viven socket, token y pid-file.
pub fn daemon_dir() -> Option<PathBuf> {
    Some(crate::utils::app_paths::data_dir()?.join("daemon"))
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_generation: Option<Uuid>,
    /// Captured from authenticated Hello by the host, never deserialized.
    #[serde(skip)]
    pub(crate) hook_owner: Option<Uuid>,
    #[serde(default)]
    pub memory_task_id: Option<Uuid>,
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
    #[serde(default)]
    pub leaf_id: Option<Uuid>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

// Rust construction and deserialization must use the same terminal geometry.
// Deriving Default silently creates a 0x0 PTY, later clamped to 1x1.
impl Default for WireSpec {
    fn default() -> Self {
        Self {
            memory_task_id: None,
            hook_generation: None,
            hook_owner: None,
            title: String::new(),
            cwd: None,
            startup_command: None,
            panel_id: None,
            workspace_id: None,
            leaf_id: None,
            cols: default_cols(),
            rows: default_rows(),
        }
    }
}

fn default_cols() -> u16 {
    80
}

fn default_rows() -> u16 {
    24
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostHookTarget {
    pub workspace_id: Uuid,
    pub panel_id: Uuid,
    pub leaf_id: Uuid,
}

/// Pedido de la app al daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Handshake. Sin un `Hello` válido no se acepta ningún otro mensaje.
    Hello {
        version: u32,
        token: String,
        /// Identidad de la instancia de app. Todas sus conexiones (control y
        /// una por PTY remoto) comparten este id para que el daemon nunca
        /// reconcilie sesiones pertenecientes a otra ventana/proceso.
        client_id: Uuid,
    },
    Spawn {
        spec: WireSpec,
        /// Id que propone el cliente, para que el id de la app y el del daemon
        /// sean el mismo (si no, la app no puede mapear panel ↔ sesión).
        #[serde(default)]
        id: Option<Uuid>,
    },
    /// Reengancharse a una sesión viva; devuelve snapshot + seq actual.
    Attach {
        id: Uuid,
    },
    Write {
        id: Uuid,
        /// Bytes crudos del PTY, codificados como base64 dentro del JSON.
        /// Un terminal no es un stream UTF-8: convertirlo a `String` puede
        /// corromper tanto input binario como secuencias multibyte partidas.
        #[serde(with = "wire_bytes")]
        data: Vec<u8>,
    },
    Resize {
        id: Uuid,
        cols: u16,
        rows: u16,
    },
    Kill {
        id: Uuid,
    },
    /// Creator cancellation fences a late Spawn without adopting a foreign UUID.
    CloseBirth {
        id: Uuid,
    },
    /// Sesión enfocada en la UI. El daemon usa esta señal para no retrasar su
    /// reader PTY mientras aplica contrapresión breve a los readers de fondo.
    SetPriority {
        #[serde(default)]
        id: Option<Uuid>,
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
        /// Absent on an older daemon. Ambiguous births may only be retried and
        /// closed by UUID when the server advertises creator-aware idempotency.
        #[serde(default)]
        birth_idempotent: bool,
        #[serde(default)]
        birth_cancel_safe: bool,
        #[serde(default)]
        hook_birth_authority: bool,
    },
    Spawned {
        id: Uuid,
    },
    /// Estado de una sesión al reengancharse: el historial ya visto y el `seq`
    /// hasta el que llega. Los eventos con `seq` ≤ este se descartan.
    Attached {
        id: Uuid,
        #[serde(with = "wire_bytes")]
        snapshot: Vec<u8>,
        seq: u64,
        alive: bool,
        /// Actual raw PTY output has been observed. Semantic replay, metadata
        /// and sequence advancement alone cannot prove startup readiness.
        #[serde(default)]
        output_ready: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hook_generation: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hook_target: Option<HostHookTarget>,
    },
    Sessions {
        ids: Vec<Uuid>,
    },
    /// Salida nueva de una sesión.
    Output {
        id: Uuid,
        seq: u64,
        #[serde(with = "wire_bytes")]
        data: Vec<u8>,
    },
    Exit {
        id: Uuid,
    },
    InputError {
        id: Uuid,
        message: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BirthCapabilities {
    pub idempotent: bool,
    pub cancel_safe: bool,
    pub hook_authority: bool,
}

impl BirthCapabilities {
    pub fn permits_birth(self) -> bool {
        self.idempotent && self.cancel_safe
    }
    pub fn permits_hook_birth(self) -> bool {
        self.permits_birth() && self.hook_authority
    }
}

/// Adaptador binario para mantener NDJSON observable sin fingir que el PTY es
/// texto UTF-8. El formato de wire es una cadena base64 estable.
mod wire_bytes {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(serde::de::Error::custom)
    }
}

/// Serializa un mensaje como una línea NDJSON (con el `\n` final).
pub fn encode_line<T: Serialize>(message: &T) -> String {
    let mut line = serde_json::to_string(message).unwrap_or_else(|_| "{}".to_owned());
    line.push('\n');
    line
}

/// The serializer's base64 expansion and final LF belong to the wire budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotWireTooLarge;

impl std::fmt::Display for SnapshotWireTooLarge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("el snapshot y su metadata exceden el límite del protocolo")
    }
}

impl std::error::Error for SnapshotWireTooLarge {}

/// Safe for both Attach and the semantic Output repair at the same boundary.
/// UUID length, maximum sequence and alive=false are reserved before capture.
pub fn snapshot_payload_budget(
    id: Uuid,
    max_line_bytes: usize,
) -> Result<usize, SnapshotWireTooLarge> {
    let attached = encode_line(&Response::Attached {
        id,
        snapshot: Vec::new(),
        seq: u64::MAX,
        alive: false,
        output_ready: false,
        hook_generation: Some(Uuid::nil()),
        hook_target: Some(HostHookTarget {
            workspace_id: Uuid::nil(),
            panel_id: Uuid::nil(),
            leaf_id: Uuid::nil(),
        }),
    })
    .len();
    let repair = encode_line(&Response::Output {
        id,
        seq: u64::MAX,
        data: Vec::new(),
    })
    .len();
    let remaining = max_line_bytes
        .checked_sub(attached.max(repair))
        .ok_or(SnapshotWireTooLarge)?;
    Ok((remaining / 4) * 3)
}

/// Reserve the exact metadata bytes before exporting the semantic ANSI state.
pub fn snapshot_ansi_budget(
    id: Uuid,
    metadata_bytes: usize,
    max_line_bytes: usize,
) -> Result<usize, SnapshotWireTooLarge> {
    snapshot_payload_budget(id, max_line_bytes)?
        .checked_sub(metadata_bytes)
        .ok_or(SnapshotWireTooLarge)
}

/// Serialize into a bounded sink and reserve the final LF. An error returns
/// no partial line; the caller must not write any bytes to its socket.
pub fn encode_response_line_with_limit(
    response: &Response,
    max_line_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    // Reject oversized binary payloads before wire_bytes allocates base64.
    // The final serializer remains authoritative for all other fields.
    match response {
        Response::Attached { id, snapshot, .. }
        | Response::Output {
            id, data: snapshot, ..
        } if snapshot.len() > snapshot_payload_budget(*id, max_line_bytes).unwrap_or(0) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "respuesta binaria del daemon demasiado grande",
            ));
        }
        _ => {}
    }
    struct LineBytes {
        bytes: Vec<u8>,
        max_bytes: usize,
    }
    impl std::io::Write for LineBytes {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.max_bytes.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "respuesta del daemon demasiado grande",
                ));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let max_bytes = max_line_bytes.checked_sub(1).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "límite del protocolo inválido",
        )
    })?;
    let mut line = LineBytes {
        bytes: Vec::new(),
        max_bytes,
    };
    serde_json::to_writer(&mut line, response).map_err(std::io::Error::other)?;
    line.bytes.push(b'\n');
    Ok(line.bytes)
}

/// A compatible reason inside the existing Error envelope. Unknown future
/// reason strings remain parseable; adding a Response variant is unnecessary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotFailureKind {
    TooLarge,
    Unavailable,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Serialize, Deserialize)]
struct SnapshotFailureDetails {
    id: Uuid,
    kind: SnapshotFailureKind,
}

const SNAPSHOT_FAILURE_PREFIX: &str = "terminal_canvas_snapshot_failure_v1:";

pub fn snapshot_failure_response(id: Uuid, kind: SnapshotFailureKind) -> Response {
    let details = SnapshotFailureDetails { id, kind };
    let encoded = serde_json::to_string(&details).expect("snapshot failure fields serialize");
    Response::Error {
        message: format!("{SNAPSHOT_FAILURE_PREFIX}{encoded}"),
    }
}

pub fn snapshot_failure_from_response(response: &Response) -> Option<(Uuid, SnapshotFailureKind)> {
    let Response::Error { message } = response else {
        return None;
    };
    let details: SnapshotFailureDetails =
        serde_json::from_str(message.strip_prefix(SNAPSHOT_FAILURE_PREFIX)?).ok()?;
    Some((details.id, details.kind))
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

/// A definite foreign UUID collision cannot confer adoption or Kill rights.
/// The legacy Error envelope remains readable by older binaries.
const BIRTH_REJECTED_PREFIX: &str = "terminal_canvas_birth_rejected_v1:";

pub fn birth_rejected_response(id: Uuid) -> Response {
    Response::Error {
        message: format!("{BIRTH_REJECTED_PREFIX}{id}"),
    }
}

pub fn birth_rejected_from_response(response: &Response) -> Option<Uuid> {
    let Response::Error { message } = response else {
        return None;
    };
    message.strip_prefix(BIRTH_REJECTED_PREFIX)?.parse().ok()
}

pub fn close_pending_response(id: Uuid) -> Response {
    Response::Error {
        message: format!("terminal_canvas_close_pending_v1:{id}"),
    }
}

/// Lee una línea del protocolo con un límite de memoria estricto.
///
/// `BufRead::read_line` no impone ningún tope: ante un peer que nunca envía
/// `\n`, el buffer crecería hasta agotar el proceso. La conexión debe cerrarse
/// cuando este helper devuelve `InvalidData`.
pub fn read_protocol_line(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    read_protocol_line_with_limit(reader, MAX_PROTOCOL_LINE_BYTES)
}

fn read_protocol_line_with_limit(
    reader: &mut impl BufRead,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
    let limit = max_bytes
        .checked_add(1)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "límite inválido"))?;
    let read = (&mut *reader)
        .take(limit as u64)
        .read_until(b'\n', &mut bytes)?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "línea del protocolo demasiado grande",
        ));
    }
    String::from_utf8(bytes).map(Some).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "línea del protocolo no es UTF-8",
        )
    })
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
            // Reparar permisos también al reutilizarlo. Un backup, una copia
            // manual o una versión anterior pudo haberlos ensanchado.
            restrict_to_owner(&path)?;
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
    /// Inicio de esta encarnación del proceso. El PID solo puede reciclarse;
    /// `(pid, start_ticks, nonce)` identifica al daemon exacto.
    #[serde(default)]
    pub start_ticks: Option<u64>,
}

impl PidFile {
    pub fn new(pid: u32) -> Self {
        Self {
            pid,
            nonce: Uuid::new_v4().simple().to_string(),
            start_ticks: process_start_ticks(pid),
        }
    }
}

#[cfg(target_os = "linux")]
fn process_start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = stat.rsplit_once(')')?.1.trim();
    // El resto empieza en el campo 3 (`state`); starttime es el campo 22.
    after_name.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(target_os = "macos")]
fn process_start_ticks(pid: u32) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: `info` apunta a un buffer del tamaño exacto solicitado por
    // PROC_PIDTBSDINFO y sólo se asume inicializado si proc_pidinfo lo llenó.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int,
        )
    };
    if written != std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int {
        return None;
    }
    // SAFETY: el tamaño devuelto arriba confirma que toda la estructura fue
    // inicializada por el kernel.
    let info = unsafe { info.assume_init() };
    Some(
        info.pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.pbi_start_tvusec),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_ticks(_pid: u32) -> Option<u64> {
    None
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
        decode_line, encode_line, ensure_token, handshake_ok, read_pid_file,
        read_protocol_line_with_limit, socket_file_name, write_pid_file, PidFile, Request,
        Response, WireSpec, PROTOCOL_VERSION,
    };
    use std::io::{Cursor, ErrorKind};
    use uuid::Uuid;

    #[test]
    fn default_spec_matches_missing_wire_fields() {
        let decoded: WireSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(WireSpec::default(), decoded);
        assert_eq!((decoded.cols, decoded.rows), (80, 24));
    }

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
                client_id: Uuid::new_v4(),
            },
            Request::Spawn {
                id: None,
                spec: WireSpec {
                    memory_task_id: None,
                    title: "Terminal".to_owned(),
                    cwd: Some("/tmp".to_owned()),
                    startup_command: Some("claude".to_owned()),
                    panel_id: Some(id),
                    workspace_id: None,
                    leaf_id: Some(Uuid::new_v4()),
                    cols: 120,
                    rows: 40,
                    hook_generation: None,

                    hook_owner: None,
                },
            },
            Request::Attach { id },
            Request::SetPriority { id: Some(id) },
            Request::Write {
                id,
                data: b"echo hola\n".to_vec(),
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
                birth_idempotent: true,
                birth_cancel_safe: false,
                hook_birth_authority: false,
            },
            Response::Spawned { id },
            Response::Attached {
                id,
                snapshot: b"hola\n".to_vec(),
                seq: 42,
                alive: true,
                output_ready: false,
                hook_generation: None,
                hook_target: None,
            },
            Response::Sessions { ids: vec![id] },
            Response::Output {
                id,
                seq: 43,
                data: b"salida".to_vec(),
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
    fn arbitrary_binary_payloads_stay_on_one_line_and_round_trip_exactly() {
        // Incluye saltos, NUL y bytes que no son UTF-8 válido.
        let message = Response::Output {
            id: Uuid::new_v4(),
            seq: 1,
            data: vec![b'l', b'1', b'\n', 0, 0xff, 0x80, b'\r', b'\n'],
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
    fn protocol_reader_rejects_a_line_over_its_limit() {
        let mut reader = Cursor::new(b"123456789\n");
        let err = read_protocol_line_with_limit(&mut reader, 8).expect_err("debe rechazarla");
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn protocol_reader_accepts_a_bounded_line_and_clean_eof() {
        let mut reader = Cursor::new(b"1234567\n");
        assert_eq!(
            read_protocol_line_with_limit(&mut reader, 8).unwrap(),
            Some("1234567\n".to_owned())
        );
        assert_eq!(read_protocol_line_with_limit(&mut reader, 8).unwrap(), None);
    }

    #[test]
    fn protocol_reader_rejects_non_utf8_input() {
        let mut reader = Cursor::new([0xff, b'\n']);
        let err = read_protocol_line_with_limit(&mut reader, 8).expect_err("debe rechazarla");
        assert_eq!(err.kind(), ErrorKind::InvalidData);
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

    #[cfg(unix)]
    #[test]
    fn reusing_a_token_repairs_overly_broad_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("token-mode-repair");
        let token = ensure_token(&dir).expect("crea");
        let path = super::token_path(&dir);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(ensure_token(&dir).expect("reusa"), token);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "el reuse debe reparar el modo");
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
    fn the_current_process_pid_file_records_its_start_time() {
        let pid_file = PidFile::new(std::process::id());
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(
            pid_file.start_ticks.is_some(),
            "la identidad no puede depender sólo de un PID reciclable"
        );
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

    #[test]
    fn snapshot_wire_budget_fits_maximum_sequence_and_base64_padding() {
        let id = Uuid::from_u128(1);
        for limit in [512, 513, 514, 1024] {
            let budget = super::snapshot_payload_budget(id, limit).unwrap();
            for removed in 0..3 {
                let data = vec![0xff; budget - removed];
                let attached = Response::Attached {
                    id,
                    snapshot: data.clone(),
                    seq: u64::MAX,
                    alive: false,
                    output_ready: false,
                    hook_generation: None,
                    hook_target: None,
                };
                let repair = Response::Output {
                    id,
                    seq: u64::MAX,
                    data,
                };
                for response in [attached, repair] {
                    let line = super::encode_response_line_with_limit(&response, limit).unwrap();
                    assert!(line.len() <= limit);
                    assert_eq!(
                        super::decode_line::<Response>(std::str::from_utf8(&line).unwrap()),
                        Some(response)
                    );
                }
            }
            let oversized = Response::Attached {
                id,
                snapshot: vec![0; budget + 1],
                seq: u64::MAX,
                alive: false,
                output_ready: false,
                hook_generation: None,
                hook_target: None,
            };
            assert!(super::encode_response_line_with_limit(&oversized, limit).is_err());
        }
    }

    #[test]
    fn snapshot_wire_budget_reserves_every_metadata_byte_before_capture() {
        let id = Uuid::from_u128(2);
        let limit = 512;
        let payload = super::snapshot_payload_budget(id, limit).unwrap();
        let metadata = b"\x1b]2;title\x07\x1b]7;/tmp/repo\x07";
        let ansi = super::snapshot_ansi_budget(id, metadata.len(), limit).unwrap();
        assert_eq!(ansi + metadata.len(), payload);
        let mut snapshot = vec![b'X'; ansi];
        snapshot.extend_from_slice(metadata);
        let response = Response::Attached {
            id,
            snapshot,
            seq: u64::MAX,
            alive: false,
            output_ready: false,
            hook_generation: None,
            hook_target: None,
        };
        assert!(super::encode_response_line_with_limit(&response, limit).is_ok());
        assert!(super::snapshot_ansi_budget(id, payload + 1, limit).is_err());
        assert!(super::snapshot_payload_budget(id, 1).is_err());
    }

    #[test]
    fn bounded_response_encoder_refuses_oversize_without_returning_partial_line() {
        let response = Response::Error {
            message: "long".repeat(64),
        };
        assert!(super::encode_response_line_with_limit(&response, 64).is_err());
        assert!(super::encode_response_line_with_limit(&response, 0).is_err());
        let expected = super::encode_line(&response);
        let line = super::encode_response_line_with_limit(&response, expected.len()).unwrap();
        assert_eq!(line, expected.as_bytes());
        assert!(super::encode_response_line_with_limit(&response, expected.len() - 1).is_err());
    }

    #[test]
    fn snapshot_failure_remains_a_legacy_error_and_accepts_unknown_reason_codes() {
        let id = Uuid::from_u128(3);
        let response = super::snapshot_failure_response(id, super::SnapshotFailureKind::TooLarge);
        assert!(matches!(response, Response::Error { .. }));
        let line = super::encode_line(&response);
        let decoded: Response = super::decode_line(&line).unwrap();
        assert_eq!(
            super::snapshot_failure_from_response(&decoded),
            Some((id, super::SnapshotFailureKind::TooLarge))
        );
        let Response::Error { message } = response else {
            unreachable!()
        };
        let future = Response::Error {
            message: message.replace("too_large", "future_reason"),
        };
        assert_eq!(
            super::snapshot_failure_from_response(&future),
            Some((id, super::SnapshotFailureKind::Unknown))
        );
        assert_eq!(
            super::snapshot_failure_from_response(&Response::Error {
                message: "unstructured refusal".to_owned()
            }),
            None
        );
    }
}

#[cfg(test)]
mod birth_capability_contract_tests {
    use super::*;

    #[test]
    fn legacy_welcome_defaults_to_no_birth_ownership_guarantee() {
        assert_eq!(
            decode_line::<Response>("{\"type\":\"welcome\",\"version\":4}\n"),
            Some(Response::Welcome {
                version: 4,
                birth_idempotent: false,
                birth_cancel_safe: false,
                hook_birth_authority: false,
            })
        );
    }

    #[test]
    fn idempotent_birth_capability_roundtrips_without_changing_protocol_version() {
        let response = Response::Welcome {
            version: PROTOCOL_VERSION,
            birth_idempotent: true,
            birth_cancel_safe: false,
            hook_birth_authority: false,
        };
        assert_eq!(
            decode_line::<Response>(&encode_line(&response)),
            Some(response)
        );
    }

    #[test]
    fn birth_rejection_is_typed_and_remains_a_legacy_error_envelope() {
        let id = Uuid::new_v4();
        let response = birth_rejected_response(id);
        assert!(matches!(response, Response::Error { .. }));
        assert_eq!(birth_rejected_from_response(&response), Some(id));
        assert_eq!(
            decode_line::<Response>(&encode_line(&response)),
            Some(response)
        );
        assert_eq!(
            birth_rejected_from_response(&Response::Error {
                message: "unknown".into()
            }),
            None
        );
        assert_eq!(
            birth_rejected_from_response(&Response::Error {
                message: format!("{BIRTH_REJECTED_PREFIX}bad uuid")
            }),
            None
        );
    }
}

#[cfg(test)]
mod attached_readiness_contract_tests {
    use super::*;

    #[test]
    fn legacy_attached_without_readiness_is_conservatively_not_ready() {
        let id = Uuid::new_v4();
        let line = format!("{{\"type\":\"attached\",\"id\":\"{id}\",\"snapshot\":\"\",\"seq\":99,\"alive\":true}}\n");
        assert_eq!(
            decode_line::<Response>(&line),
            Some(Response::Attached {
                id,
                snapshot: Vec::new(),
                seq: 99,
                alive: true,
                output_ready: false,
                hook_generation: None,
                hook_target: None,
            })
        );
    }
}

#[cfg(test)]
mod full_attach_capability_fixtures {
    use super::*;
    #[test]
    fn legacy_welcome_never_proves_birth_cancellation() {
        let response = decode_line::<Response>(r#"{"type":"welcome","version":4}"#).unwrap();
        assert!(matches!(
            response,
            Response::Welcome {
                birth_idempotent: false,
                birth_cancel_safe: false,
                ..
            }
        ));
        assert!(!BirthCapabilities {
            idempotent: true,
            cancel_safe: false,
            hook_authority: false
        }
        .permits_birth());
        assert!(BirthCapabilities {
            idempotent: true,
            cancel_safe: true,
            hook_authority: false
        }
        .permits_birth());
    }
    #[test]
    fn close_birth_roundtrips_the_original_uuid() {
        let id = Uuid::from_u128(0x6101);
        assert_eq!(
            decode_line::<Request>(&encode_line(&Request::CloseBirth { id })),
            Some(Request::CloseBirth { id })
        );
    }
    #[test]
    fn binary_refusal_precedes_base64_serialization() {
        let id = Uuid::from_u128(0x6102);
        let cap = 512;
        let bytes = vec![0xff; snapshot_payload_budget(id, cap).unwrap() + 1];
        let error = encode_response_line_with_limit(
            &Response::Output {
                id,
                seq: 1,
                data: bytes,
            },
            cap,
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("respuesta binaria"));
    }
}
