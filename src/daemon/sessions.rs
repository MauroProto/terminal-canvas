//! Sesiones del daemon vistas como sesiones normales de la app (P3.15, T3).
//!
//! `spawn_remote` hace el ida y vuelta que hace falta para que un panel quede
//! atado a un PTY que vive en el daemon: crear la sesión, engancharse (para
//! traerse el historial y el `seq`), y armar un `PtyHandle` remoto sobre la
//! misma conexión. De ahí en adelante el panel no distingue local de remoto.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use uuid::Uuid;

use super::client::DaemonConn;
use super::protocol::{decode_line, encode_line, Request, Response, WireSpec};
use crate::runtime::SharedRuntimeScheduler;
use crate::terminal::pty::PtyHandle;

/// Datos para abrir conexiones nuevas al daemon.
#[derive(Debug, Clone)]
pub struct DaemonEndpoint {
    pub dir: std::path::PathBuf,
    pub token: String,
}

impl DaemonEndpoint {
    pub fn new(dir: impl Into<std::path::PathBuf>, token: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            token: token.into(),
        }
    }
}

/// Spec de la app traducido al del protocolo.
pub fn wire_spec_from(spec: &crate::runtime::SessionSpec, cols: u16, rows: u16) -> WireSpec {
    WireSpec {
        title: spec.title.clone(),
        cwd: spec
            .cwd
            .as_ref()
            .map(|cwd| cwd.to_string_lossy().into_owned()),
        startup_command: spec.startup_command.clone(),
        panel_id: spec.panel_id,
        workspace_id: spec.workspace_id,
        cols: cols.max(1),
        rows: rows.max(1),
    }
}

/// Crea la sesión en el daemon y devuelve un `PtyHandle` atado a ella.
pub fn spawn_remote(
    endpoint: &DaemonEndpoint,
    spec: &crate::runtime::SessionSpec,
    cols: u16,
    rows: u16,
    scheduler: SharedRuntimeScheduler,
    desired_id: Option<Uuid>,
) -> anyhow::Result<(Uuid, PtyHandle)> {
    let stream = DaemonConn::connect_raw(&endpoint.dir, &endpoint.token)
        .ok_or_else(|| anyhow::anyhow!("no se pudo conectar al daemon"))?;
    let mut writer = stream
        .try_clone()
        .map_err(|err| anyhow::anyhow!("no se pudo clonar el socket: {err}"))?;
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|err| anyhow::anyhow!("no se pudo clonar el socket: {err}"))?,
    );

    // Crear la sesión.
    let wire = wire_spec_from(spec, cols, rows);
    let session_id = match request(
        &mut writer,
        &mut reader,
        &Request::Spawn {
            spec: wire,
            id: desired_id,
        },
    )? {
        Response::Spawned { id } => id,
        other => anyhow::bail!("el daemon no espawneó la sesión: {other:?}"),
    };

    // Engancharse: trae el historial ya emitido y desde qué `seq` es nuevo.
    let (snapshot, seq) = match request(
        &mut writer,
        &mut reader,
        &Request::Attach { id: session_id },
    )? {
        Response::Attached { snapshot, seq, .. } => (snapshot, seq),
        other => anyhow::bail!("el daemon no dejó engancharse: {other:?}"),
    };

    // El mismo socket sirve de canal de control y de stream de eventos.
    let control = stream
        .try_clone()
        .map_err(|err| anyhow::anyhow!("no se pudo clonar el socket: {err}"))?;
    let handle = PtyHandle::attach_remote(
        session_id,
        control,
        stream,
        &snapshot,
        seq,
        cols.max(1),
        rows.max(1),
        scheduler,
    )?;
    Ok((session_id, handle))
}

/// Reengancha una sesión que **ya existe** en el daemon (por ejemplo, la que
/// sobrevivió al cierre de la app anterior).
pub fn attach_existing(
    endpoint: &DaemonEndpoint,
    session_id: Uuid,
    cols: u16,
    rows: u16,
    scheduler: SharedRuntimeScheduler,
) -> anyhow::Result<PtyHandle> {
    let stream = DaemonConn::connect_raw(&endpoint.dir, &endpoint.token)
        .ok_or_else(|| anyhow::anyhow!("no se pudo conectar al daemon"))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let (snapshot, seq) = match request(
        &mut writer,
        &mut reader,
        &Request::Attach { id: session_id },
    )? {
        Response::Attached { snapshot, seq, .. } => (snapshot, seq),
        other => anyhow::bail!("la sesión {session_id} no existe en el daemon: {other:?}"),
    };
    let control = stream.try_clone()?;
    PtyHandle::attach_remote(
        session_id,
        control,
        stream,
        &snapshot,
        seq,
        cols.max(1),
        rows.max(1),
        scheduler,
    )
}

/// Pedido/respuesta salteando los eventos que el daemon empuja en el medio.
fn request(
    writer: &mut impl Write,
    reader: &mut impl BufRead,
    request: &Request,
) -> anyhow::Result<Response> {
    writer.write_all(encode_line(request).as_bytes())?;
    writer.flush()?;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            anyhow::bail!("el daemon cerró la conexión");
        }
        let Some(response) = decode_line::<Response>(&line) else {
            continue;
        };
        if super::client::is_pushed_event(&response) {
            // Evento empujado mientras esperábamos: se descarta acá porque
            // todavía no hay reader que lo consuma (el snapshot del attach ya
            // trae ese historial).
            continue;
        }
        return Ok(response);
    }
}

/// Path del directorio del daemon y token, si están disponibles.
pub fn endpoint_from_env() -> Option<DaemonEndpoint> {
    let dir = super::server::resolve_dir()?;
    let token = super::protocol::ensure_token(&dir).ok()?;
    Some(DaemonEndpoint::new(dir, token))
}

/// ¿Existe un socket de daemon al que valga la pena intentar conectarse?
pub fn daemon_socket_exists(dir: &Path) -> bool {
    super::protocol::socket_path(dir).exists()
}

#[cfg(test)]
mod tests {
    use super::{daemon_socket_exists, wire_spec_from, DaemonEndpoint};
    use crate::runtime::SessionSpec;
    use uuid::Uuid;

    #[test]
    fn the_wire_spec_carries_what_the_daemon_needs() {
        let panel = Uuid::new_v4();
        let workspace = Uuid::new_v4();
        let spec = SessionSpec {
            title: "Claude".to_owned(),
            cwd: Some(std::path::PathBuf::from("/tmp/repo")),
            startup_command: Some("claude".to_owned()),
            startup_input: None,
            panel_id: Some(panel),
            workspace_id: Some(workspace),
        };
        let wire = wire_spec_from(&spec, 120, 40);
        assert_eq!(wire.title, "Claude");
        assert_eq!(wire.cwd.as_deref(), Some("/tmp/repo"));
        assert_eq!(wire.startup_command.as_deref(), Some("claude"));
        assert_eq!(wire.panel_id, Some(panel));
        assert_eq!(wire.workspace_id, Some(workspace));
        assert_eq!((wire.cols, wire.rows), (120, 40));
    }

    #[test]
    fn a_zero_geometry_is_clamped_so_the_pty_is_valid() {
        let wire = wire_spec_from(&SessionSpec::default(), 0, 0);
        assert_eq!((wire.cols, wire.rows), (1, 1), "un PTY 0x0 no es válido");
    }

    #[test]
    fn a_missing_socket_is_reported_without_connecting() {
        let dir = std::env::temp_dir().join(format!("tc-nosock-{}", Uuid::new_v4()));
        assert!(!daemon_socket_exists(&dir));
    }

    #[test]
    fn the_endpoint_keeps_dir_and_token() {
        let endpoint = DaemonEndpoint::new("/tmp/x", "tok");
        assert_eq!(endpoint.dir, std::path::PathBuf::from("/tmp/x"));
        assert_eq!(endpoint.token, "tok");
    }
}
