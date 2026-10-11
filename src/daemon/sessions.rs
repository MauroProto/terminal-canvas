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
use super::protocol::{
    birth_rejected_from_response, decode_line, encode_line, read_protocol_line,
    snapshot_failure_from_response, Request, Response, WireSpec,
};
use crate::runtime::{
    RemoteCloseIntent, RemoteRecoveryError, RemoteSpawnIntent, RemoteSpawnResult,
    SharedRuntimeScheduler,
};
use crate::terminal::pty::PtyHandle;

/// Datos para abrir conexiones nuevas al daemon.
#[derive(Debug, Clone)]
pub struct DaemonEndpoint {
    pub dir: std::path::PathBuf,
    pub token: String,
    pub client_id: Uuid,
}

impl DaemonEndpoint {
    pub fn new(
        dir: impl Into<std::path::PathBuf>,
        token: impl Into<String>,
        client_id: Uuid,
    ) -> Self {
        Self {
            dir: dir.into(),
            token: token.into(),
            client_id,
        }
    }
}

/// Spec de la app traducido al del protocolo.
pub fn wire_spec_from(spec: &crate::runtime::SessionSpec, cols: u16, rows: u16) -> WireSpec {
    WireSpec {
        memory_task_id: spec.memory_task_id,
        title: spec.title.clone(),
        cwd: spec
            .cwd
            .as_ref()
            .map(|cwd| cwd.to_string_lossy().into_owned()),
        startup_command: spec.startup_command.clone(),
        panel_id: spec.panel_id,
        workspace_id: spec.workspace_id,
        leaf_id: spec.leaf_id,
        cols: cols.max(1),
        rows: rows.max(1),
        hook_generation: None,

        hook_owner: None,
    }
}

/// Preserve the identity before Spawn, including an ambiguous reply.
pub fn spawn_remote(
    endpoint: &DaemonEndpoint,
    spec: &crate::runtime::SessionSpec,
    cols: u16,
    rows: u16,
    scheduler: SharedRuntimeScheduler,
    desired_id: Option<Uuid>,
) -> anyhow::Result<(Uuid, PtyHandle)> {
    let id = desired_id.unwrap_or_else(Uuid::new_v4);
    let intent = if desired_id.is_none() {
        RemoteSpawnIntent::NewBirth
    } else {
        RemoteSpawnIntent::AdoptExisting
    };
    recover_remote(endpoint, spec, cols, rows, scheduler, id, intent)
        .map(|result| (result.session_id, result.handle))
}

fn recovery_error(endpoint: &DaemonEndpoint, error: RemoteRecoveryError) -> anyhow::Error {
    let endpoint = endpoint.clone();
    error
        .with_closer(std::sync::Arc::new(move |id, intent| {
            close_remote(&endpoint, id, intent)
        }))
        .into()
}

/// A completed Error can be followed by List; transport failures cannot.
fn existing_snapshot(
    writer: &mut impl Write,
    reader: &mut impl BufRead,
    id: Uuid,
) -> anyhow::Result<
    Option<(
        Vec<u8>,
        u64,
        bool,
        bool,
        crate::hook_authority::HostEvidence,
    )>,
> {
    let result = (|| -> anyhow::Result<_> {
        match request(writer, reader, &Request::Attach { id })? {
            Response::Attached {
                id: actual,
                snapshot,
                seq,
                alive,
                output_ready,
                hook_generation,
                hook_target,
                ..
            } if actual == id => Ok(Some((
                snapshot,
                seq,
                alive,
                output_ready,
                hook_evidence(id, hook_generation, hook_target),
            ))),
            error @ Response::Error { .. } => {
                if let Some((failed_id, kind)) = snapshot_failure_from_response(&error) {
                    anyhow::ensure!(
                        failed_id == id,
                        "El daemon devolvió un error de otra identidad."
                    );
                    anyhow::bail!(
                        "La sesión remota existe, pero su pantalla sigue pendiente ({kind:?})."
                    );
                }
                match request(writer, reader, &Request::List)? {
                    Response::Sessions { ids } if !ids.contains(&id) => Ok(None),
                    Response::Sessions { .. } => anyhow::bail!(
                        "La sesión remota existe, pero su pantalla no está disponible."
                    ),
                    _ => anyhow::bail!("El daemon no confirmó si la sesión remota existe."),
                }
            }
            _ => anyhow::bail!("Respuesta inválida al recuperar la identidad remota."),
        }
    })();
    result.map_err(|error| {
        if error.downcast_ref::<RemoteRecoveryError>().is_some() {
            error
        } else {
            RemoteRecoveryError::protected(id, error.to_string()).into()
        }
    })
}

fn spawn_then_snapshot(
    writer: &mut impl Write,
    reader: &mut impl BufRead,
    spec: WireSpec,
    id: Uuid,
) -> anyhow::Result<(
    Vec<u8>,
    u64,
    bool,
    bool,
    crate::hook_authority::HostEvidence,
)> {
    let result = (|| -> anyhow::Result<_> {
        let spawned = request(writer, reader, &Request::Spawn { spec, id: Some(id) })?;
        if birth_rejected_from_response(&spawned) == Some(id) {
            anyhow::bail!(RemoteRecoveryError::birth_rejected(id,
            "La identidad de nacimiento fue rechazada; no se adquieren derechos sobre otra sesión."));
        }
        anyhow::ensure!(
            matches!(spawned, Response::Spawned { id: actual } if actual == id),
            "No se confirmó la creación remota; se recuperará la misma identidad."
        );
        match request(writer, reader, &Request::Attach { id })? {
            Response::Attached {
                id: actual,
                snapshot,
                seq,
                alive,
                output_ready,
                hook_generation,
                hook_target,
                ..
            } if actual == id => Ok((
                snapshot,
                seq,
                alive,
                output_ready,
                hook_evidence(id, hook_generation, hook_target),
            )),
            error @ Response::Error { .. } => {
                if let Some((failed_id, kind)) = snapshot_failure_from_response(&error) {
                    anyhow::ensure!(
                        failed_id == id,
                        "El daemon devolvió un error de otra identidad."
                    );
                    anyhow::bail!(
                        "La sesión fue creada, pero su pantalla sigue pendiente ({kind:?})."
                    );
                }
                anyhow::bail!("La sesión fue creada, pero su pantalla sigue pendiente.");
            }
            _ => anyhow::bail!("Respuesta inválida al recuperar la sesión creada."),
        }
    })();
    result.map_err(|error| {
        if error.downcast_ref::<RemoteRecoveryError>().is_some() {
            error
        } else {
            RemoteRecoveryError::birth_pending(id, error.to_string(), true).into()
        }
    })
}

/// Real application backend. Only a genuinely fresh pre-Spawn failure may
/// degrade locally; recovery of an existing or ambiguous identity is protected.
pub fn recover_remote(
    endpoint: &DaemonEndpoint,
    spec: &crate::runtime::SessionSpec,
    cols: u16,
    rows: u16,
    scheduler: SharedRuntimeScheduler,
    id: Uuid,
    intent: RemoteSpawnIntent,
) -> anyhow::Result<RemoteSpawnResult> {
    let (stream, capabilities) = match DaemonConn::connect_raw_with_capabilities_as(
        &endpoint.dir,
        &endpoint.token,
        endpoint.client_id,
    ) {
        Some(connection) => connection,
        None if intent == RemoteSpawnIntent::NewBirth => {
            return Err(RemoteRecoveryError::before_creation(
                id,
                "No se pudo conectar al daemon antes de crear la sesión.",
            )
            .into())
        }
        None => {
            return Err(recovery_error(
                endpoint,
                RemoteRecoveryError::protected(
                    id,
                    "La sesión remota sigue pendiente: el daemon no responde.",
                ),
            ))
        }
    };
    let mut birth = intent == RemoteSpawnIntent::NewBirth;
    let operation = || -> anyhow::Result<RemoteSpawnResult> {
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let existing = if birth {
            None
        } else {
            match request(
                &mut writer,
                &mut reader,
                &Request::Resize {
                    id,
                    cols: cols.max(1),
                    rows: rows.max(1),
                },
            )? {
                Response::Sessions { .. } | Response::Error { .. } => {}
                _ => anyhow::bail!("Respuesta inválida al redimensionar la sesión."),
            }
            existing_snapshot(&mut writer, &mut reader, id)?
        };
        let (snapshot, seq, alive, output_ready, evidence) = if let Some(existing) = existing {
            existing
        } else {
            birth = true;
            if !capabilities.permits_hook_birth() {
                anyhow::bail!(RemoteRecoveryError::birth_rejected(id,
                    "La creación remota requiere actualizar el daemon para confirmar idempotencia y cancelación por UUID."));
            }
            let runtime = crate::hook_runtime::process_runtime()
                .ok_or_else(|| anyhow::anyhow!("hook endpoint unavailable"))?;
            anyhow::ensure!(
                runtime.owner == endpoint.client_id,
                "remote endpoint owner mismatch"
            );
            let key =
                spec_key(spec).ok_or_else(|| anyhow::anyhow!("new birth target incomplete"))?;
            let claim = runtime.authorize_birth(key, id)?;
            let mut wire = wire_spec_from(spec, cols, rows);
            wire.hook_generation = claim.generation;
            spawn_then_snapshot(&mut writer, &mut reader, wire, id)?
        };
        let runtime = crate::hook_runtime::process_runtime()
            .ok_or_else(|| anyhow::anyhow!("hook endpoint unavailable"))?;
        let key = spec_key(spec).ok_or_else(|| anyhow::anyhow!("attachment target incomplete"))?;
        runtime.attach(key, evidence, spec.hook_legacy_root)?;
        let mut handle = PtyHandle::attach_remote(
            id,
            stream.try_clone()?,
            reader,
            &snapshot,
            seq,
            true,
            alive,
            cols.max(1),
            rows.max(1),
            scheduler,
        )?;
        handle.set_hook_host_evidence(evidence);
        Ok(RemoteSpawnResult {
            session_id: id,
            handle,
            output_ready,
            owned_birth: birth,
        })
    };
    let result = operation();
    result.map_err(|error| {
        let refusal = if let Some(typed) = error.downcast_ref::<RemoteRecoveryError>() {
            typed.clone()
        } else if birth {
            RemoteRecoveryError::birth_pending(id, error.to_string(), capabilities.permits_birth())
        } else {
            RemoteRecoveryError::protected(id, error.to_string())
        };
        recovery_error(endpoint, refusal)
    })
}

/// Strict hot attach. It never requests Spawn or invents a missing identity.
pub fn attach_existing(
    endpoint: &DaemonEndpoint,
    session_id: Uuid,
    cols: u16,
    rows: u16,
    scheduler: SharedRuntimeScheduler,
) -> anyhow::Result<PtyHandle> {
    let operation = || -> anyhow::Result<PtyHandle> {
        let stream = DaemonConn::connect_raw_as(&endpoint.dir, &endpoint.token, endpoint.client_id)
            .ok_or_else(|| anyhow::anyhow!("no se pudo conectar al daemon"))?;
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream.try_clone()?);
        match request(
            &mut writer,
            &mut reader,
            &Request::Resize {
                id: session_id,
                cols: cols.max(1),
                rows: rows.max(1),
            },
        )? {
            Response::Sessions { .. } | Response::Error { .. } => {}
            _ => anyhow::bail!("Respuesta inválida al redimensionar la sesión."),
        }
        let (snapshot, seq, alive, evidence) = match request(
            &mut writer,
            &mut reader,
            &Request::Attach { id: session_id },
        )? {
            Response::Attached {
                id,
                snapshot,
                seq,
                alive,
                hook_generation,
                hook_target,
                ..
            } if id == session_id => (
                snapshot,
                seq,
                alive,
                hook_evidence(id, hook_generation, hook_target),
            ),
            _ => anyhow::bail!("La pantalla de la sesión remota sigue pendiente."),
        };
        let mut handle = PtyHandle::attach_remote(
            session_id,
            stream.try_clone()?,
            reader,
            &snapshot,
            seq,
            true,
            alive,
            cols.max(1),
            rows.max(1),
            scheduler,
        )?;
        handle.set_hook_host_evidence(evidence);
        Ok(handle)
    };
    operation().map_err(|error| {
        recovery_error(
            endpoint,
            RemoteRecoveryError::protected(session_id, error.to_string()),
        )
    })
}

/// Confirm removal without relying on a PtyHandle. An authoritative List is
/// permitted only after a completed Error, never after I/O or corrupt framing.
pub fn close_remote(
    endpoint: &DaemonEndpoint,
    id: Uuid,
    intent: RemoteCloseIntent,
) -> anyhow::Result<()> {
    let runtime = crate::hook_runtime::process_runtime()
        .ok_or_else(|| anyhow::anyhow!("managed endpoint unavailable"))?;
    anyhow::ensure!(
        runtime.owner == endpoint.client_id,
        "close endpoint owner mismatch"
    );
    runtime.prepare_physical_close(id)?;
    close_remote_fenced(endpoint, id, intent)?;
    runtime.physical_close_confirmed(id);
    Ok(())
}

fn close_remote_fenced(
    endpoint: &DaemonEndpoint,
    id: Uuid,
    _intent: RemoteCloseIntent,
) -> anyhow::Result<()> {
    let (stream, capabilities) = DaemonConn::connect_raw_with_capabilities_as(
        &endpoint.dir,
        &endpoint.token,
        endpoint.client_id,
    )
    .ok_or_else(|| anyhow::anyhow!("No se pudo confirmar el cierre remoto."))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    if capabilities.permits_hook_birth() {
        anyhow::ensure!(
            capabilities.permits_hook_birth(),
            "El cierre de un nacimiento pendiente requiere actualizar el daemon."
        );
        // A matching Killed includes the host's late-Spawn tombstone. List is
        // never enough to cancel a birth that might still arrive on another fd.
        return require_birth_closed(
            request(&mut writer, &mut reader, &Request::CloseBirth { id })?,
            id,
        );
    }
    anyhow::bail!("physical hook-generation closure requires the stronger host capability")
}

#[cfg(test)]
fn close_legacy_protocol_only_fixture(endpoint: &DaemonEndpoint, id: Uuid) -> anyhow::Result<()> {
    // The legacy wire fixture remains available for protocol assertions. It is
    // never a managed retirement ACK in the application call graph.
    let (stream, _) = DaemonConn::connect_raw_with_capabilities_as(
        &endpoint.dir,
        &endpoint.token,
        endpoint.client_id,
    )
    .ok_or_else(|| anyhow::anyhow!("fixture connection unavailable"))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    // Adopt only an explicitly existing UUID. Snapshot refusal grants adoption
    // in the new server without turning this close into a fresh Spawn.
    match request(&mut writer, &mut reader, &Request::Attach { id })? {
        Response::Attached { id: actual, .. } if actual == id => {}
        error @ Response::Error { .. } => {
            if let Some((failed_id, _)) = snapshot_failure_from_response(&error) {
                anyhow::ensure!(failed_id == id, "Error de cierre para otra identidad.");
            } else {
                return match request(&mut writer, &mut reader, &Request::List)? {
                    Response::Sessions { ids } if !ids.contains(&id) => Ok(()),
                    _ => anyhow::bail!("No se confirmó la adopción para cerrar {id}."),
                };
            }
        }
        _ => anyhow::bail!("Respuesta inválida al adoptar para cierre."),
    }
    match request(&mut writer, &mut reader, &Request::Kill { id })? {
        Response::Killed { id: actual } if actual == id => Ok(()),
        _ => anyhow::bail!("El daemon todavía no confirmó el cierre durable de {id}."),
    }
}

/// Confirm creator-owned birth retirement by the host ACK, including its
/// late-Spawn tombstone. SL2 callers must additionally drain their hook effects;
/// this API does not prove ledger/bootstrap admission fencing by itself.
pub fn close_birth(endpoint: &DaemonEndpoint, id: Uuid) -> anyhow::Result<()> {
    close_remote(endpoint, id, RemoteCloseIntent::OwnedBirth)
}

fn require_birth_closed(response: Response, id: Uuid) -> anyhow::Result<()> {
    match response {
        Response::Killed { id: actual } if actual == id => Ok(()),
        _ => anyhow::bail!("El daemon todavía no confirmó el cierre durable del nacimiento {id}."),
    }
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
        let Some(line) = read_protocol_line(reader)? else {
            anyhow::bail!("el daemon cerró la conexión");
        };
        let response = decode_line::<Response>(&line).ok_or_else(|| {
            anyhow::anyhow!("El daemon devolvió una línea inválida; se abandona esta conexión.")
        })?;
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
    Some(DaemonEndpoint::new(dir, token, Uuid::new_v4()))
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
        let memory_task = Uuid::new_v4();
        let spec = SessionSpec {
            title: "Claude".to_owned(),
            cwd: Some(std::path::PathBuf::from("/tmp/repo")),
            startup_command: Some("claude".to_owned()),
            startup_input: None,
            panel_id: Some(panel),
            workspace_id: Some(workspace),
            leaf_id: Some(Uuid::new_v4()),
            memory_task_id: Some(memory_task),

            hook_legacy_root: false,
        };
        let wire = wire_spec_from(&spec, 120, 40);
        assert_eq!(wire.title, "Claude");
        assert_eq!(wire.cwd.as_deref(), Some("/tmp/repo"));
        assert_eq!(wire.startup_command.as_deref(), Some("claude"));
        assert_eq!(wire.panel_id, Some(panel));
        assert_eq!(wire.workspace_id, Some(workspace));
        assert_eq!(wire.memory_task_id, Some(memory_task));
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
        let client_id = Uuid::new_v4();
        let endpoint = DaemonEndpoint::new("/tmp/x", "tok", client_id);
        assert_eq!(endpoint.dir, std::path::PathBuf::from("/tmp/x"));
        assert_eq!(endpoint.token, "tok");
        assert_eq!(endpoint.client_id, client_id);
    }

    // In-memory transcripts only: these fixtures open no socket and spawn no PTY.
    fn recovery_fixture_reader(
        responses: &[crate::daemon::protocol::Response],
    ) -> std::io::Cursor<Vec<u8>> {
        let mut transcript = Vec::new();
        for response in responses {
            transcript.extend_from_slice(crate::daemon::protocol::encode_line(response).as_bytes());
        }
        std::io::Cursor::new(transcript)
    }

    fn recovery_fixture_sent_requests(bytes: &[u8]) -> Vec<crate::daemon::protocol::Request> {
        std::str::from_utf8(bytes)
            .expect("requests are UTF-8 NDJSON")
            .lines()
            .map(|line| {
                crate::daemon::protocol::decode_line(line)
                    .expect("request transcript is well framed")
            })
            .collect()
    }

    fn recovery_fixture_assert_attach_only(bytes: &[u8], id: Uuid) {
        let sent = recovery_fixture_sent_requests(bytes);
        assert_eq!(
            sent.len(),
            1,
            "a protected refusal must not send List or Spawn"
        );
        assert!(
            matches!(&sent[0], crate::daemon::protocol::Request::Attach { id: actual } if *actual == id)
        );
    }

    fn recovery_fixture_assert_attach_then_list(bytes: &[u8], id: Uuid) {
        let sent = recovery_fixture_sent_requests(bytes);
        assert_eq!(
            sent.len(),
            2,
            "a completed legacy Error permits one authoritative List"
        );
        assert!(
            matches!(&sent[0], crate::daemon::protocol::Request::Attach { id: actual } if *actual == id)
        );
        assert!(matches!(&sent[1], crate::daemon::protocol::Request::List));
    }

    #[test]
    fn consumer_snapshot_too_large_refusal_never_lists_or_spawns() {
        use crate::daemon::protocol::{snapshot_failure_response, Response, SnapshotFailureKind};

        let id = Uuid::from_u128(0x5101);
        let mut reader = recovery_fixture_reader(&[
            snapshot_failure_response(id, SnapshotFailureKind::TooLarge),
            Response::Sessions { ids: Vec::new() },
        ]);
        let mut writer = Vec::new();

        let error = super::existing_snapshot(&mut writer, &mut reader, id).unwrap_err();

        let protected = error
            .downcast_ref::<crate::runtime::RemoteRecoveryError>()
            .expect("typed snapshot refusal protects the same identity");
        assert_eq!(protected.session_id, id);
        assert!(!protected.safe_local_fallback);
        recovery_fixture_assert_attach_only(&writer, id);
        assert!(
            reader.position() < reader.get_ref().len() as u64,
            "the later Sessions frame must remain unread"
        );
    }

    #[test]
    fn consumer_snapshot_unknown_refusal_never_lists_or_spawns() {
        use crate::daemon::protocol::{snapshot_failure_response, Response, SnapshotFailureKind};

        let id = Uuid::from_u128(0x5102);
        let Response::Error { message } =
            snapshot_failure_response(id, SnapshotFailureKind::TooLarge)
        else {
            unreachable!("snapshot failure uses the legacy Error envelope");
        };
        // Exercise a future reason code, rather than serializing today's Unknown variant.
        let future_refusal = Response::Error {
            message: message.replace("too_large", "future_snapshot_reason"),
        };
        assert_eq!(
            crate::daemon::protocol::snapshot_failure_from_response(&future_refusal),
            Some((id, SnapshotFailureKind::Unknown))
        );
        let mut reader =
            recovery_fixture_reader(&[future_refusal, Response::Sessions { ids: Vec::new() }]);
        let mut writer = Vec::new();

        let error = super::existing_snapshot(&mut writer, &mut reader, id).unwrap_err();

        let protected = error
            .downcast_ref::<crate::runtime::RemoteRecoveryError>()
            .expect("unknown future snapshot refusal remains protected");
        assert_eq!(protected.session_id, id);
        assert!(!protected.safe_local_fallback);
        recovery_fixture_assert_attach_only(&writer, id);
        assert!(reader.position() < reader.get_ref().len() as u64);
    }

    #[test]
    fn consumer_snapshot_legacy_error_with_existing_identity_is_protected() {
        use crate::daemon::protocol::Response;

        let id = Uuid::from_u128(0x5103);
        let mut reader = recovery_fixture_reader(&[
            Response::Error {
                message: "legacy snapshot unavailable".to_owned(),
            },
            Response::Sessions { ids: vec![id] },
        ]);
        let mut writer = Vec::new();

        let error = super::existing_snapshot(&mut writer, &mut reader, id).unwrap_err();

        let protected = error
            .downcast_ref::<crate::runtime::RemoteRecoveryError>()
            .expect("presence is not permission to create a replacement");
        assert_eq!(protected.session_id, id);
        assert!(!protected.safe_local_fallback);
        recovery_fixture_assert_attach_then_list(&writer, id);
    }

    #[test]
    fn consumer_snapshot_legacy_error_requires_authoritative_absence() {
        use crate::daemon::protocol::Response;

        let id = Uuid::from_u128(0x5104);
        let other = Uuid::from_u128(0x5204);
        let mut reader = recovery_fixture_reader(&[
            Response::Error {
                message: "legacy unknown session".to_owned(),
            },
            Response::Sessions { ids: vec![other] },
        ]);
        let mut writer = Vec::new();

        let result = super::existing_snapshot(&mut writer, &mut reader, id).unwrap();

        assert!(
            result.is_none(),
            "only the completed List proves this identity absent"
        );
        recovery_fixture_assert_attach_then_list(&writer, id);
        assert_eq!(reader.position(), reader.get_ref().len() as u64);
    }

    #[test]
    fn consumer_snapshot_corrupt_frame_never_reaches_later_list() {
        use crate::daemon::protocol::Response;

        let id = Uuid::from_u128(0x5105);
        let mut transcript = b"{corrupt-response}\n".to_vec();
        transcript.extend_from_slice(
            crate::daemon::protocol::encode_line(&Response::Sessions { ids: Vec::new() })
                .as_bytes(),
        );
        let mut reader = std::io::Cursor::new(transcript);
        let mut writer = Vec::new();

        assert!(super::existing_snapshot(&mut writer, &mut reader, id).is_err());

        recovery_fixture_assert_attach_only(&writer, id);
        assert!(
            reader.position() < reader.get_ref().len() as u64,
            "corrupt framing must not be skipped to a later valid response"
        );
    }

    #[test]
    fn consumer_snapshot_wrong_attached_identity_is_protected() {
        use crate::daemon::protocol::Response;

        let id = Uuid::from_u128(0x5106);
        let wrong = Uuid::from_u128(0x5206);
        let mut reader = recovery_fixture_reader(&[Response::Attached {
            id: wrong,
            snapshot: b"unrelated terminal".to_vec(),
            seq: 17,
            alive: true,
            output_ready: true,
            hook_generation: None,
            hook_target: None,
        }]);
        let mut writer = Vec::new();

        let error = super::existing_snapshot(&mut writer, &mut reader, id).unwrap_err();

        let protected = error
            .downcast_ref::<crate::runtime::RemoteRecoveryError>()
            .expect("a mismatched identity cannot justify replacement");
        assert_eq!(protected.session_id, id);
        assert!(!protected.safe_local_fallback);
        recovery_fixture_assert_attach_only(&writer, id);
    }

    #[test]
    fn consumer_spawn_snapshot_refusal_keeps_proposed_identity_without_recreation() {
        use crate::daemon::protocol::{
            snapshot_failure_response, Request, Response, SnapshotFailureKind, WireSpec,
        };

        for kind in [
            SnapshotFailureKind::TooLarge,
            SnapshotFailureKind::Unavailable,
            SnapshotFailureKind::Unknown,
        ] {
            let id = Uuid::from_u128(0x5107);
            let mut reader = recovery_fixture_reader(&[
                Response::Spawned { id },
                snapshot_failure_response(id, kind),
            ]);
            let mut writer = Vec::new();

            let error =
                super::spawn_then_snapshot(&mut writer, &mut reader, WireSpec::default(), id)
                    .unwrap_err();

            let protected = error
                .downcast_ref::<crate::runtime::RemoteRecoveryError>()
                .expect("post-Spawn refusal retains the proposed remote identity");
            assert_eq!(protected.session_id, id);
            assert!(!protected.safe_local_fallback);
            let sent = recovery_fixture_sent_requests(&writer);
            assert_eq!(
                sent.len(),
                2,
                "no List or second creation follows a post-Spawn refusal"
            );
            assert!(matches!(&sent[0], Request::Spawn { id: Some(actual), .. } if *actual == id));
            assert!(matches!(&sent[1], Request::Attach { id: actual } if *actual == id));
        }
    }

    #[test]
    fn consumer_request_discards_pushed_events_before_response() {
        use crate::daemon::protocol::{Request, Response};

        let id = Uuid::from_u128(0x5108);
        let mut reader = recovery_fixture_reader(&[
            Response::Output {
                id,
                seq: 9,
                data: b"pending output".to_vec(),
            },
            Response::Exit { id },
            Response::InputError {
                id,
                message: "pending input failure".to_owned(),
            },
            Response::Sessions { ids: vec![id] },
        ]);
        let mut writer = Vec::new();

        let result = super::request(&mut writer, &mut reader, &Request::List).unwrap();

        assert!(
            matches!(result, Response::Sessions { ids } if ids == vec![id]),
            "all three pushed event variants must be skipped"
        );
        let sent = recovery_fixture_sent_requests(&writer);
        assert_eq!(sent.len(), 1);
        assert!(matches!(&sent[0], Request::List));
        assert_eq!(reader.position(), reader.get_ref().len() as u64);
    }
}

#[cfg(test)]
mod birth_close_ack_fixtures {
    use super::*;
    #[test]
    fn empty_list_and_pending_close_never_confirm_birth_cancellation() {
        let id = Uuid::from_u128(0x6601);
        assert!(require_birth_closed(Response::Sessions { ids: Vec::new() }, id).is_err());
        assert!(
            require_birth_closed(super::super::protocol::close_pending_response(id), id).is_err()
        );
    }
    #[test]
    fn only_matching_killed_ack_confirms_the_requested_birth() {
        let id = Uuid::from_u128(0x6602);
        assert!(require_birth_closed(
            Response::Killed {
                id: Uuid::from_u128(0x6603)
            },
            id
        )
        .is_err());
        assert!(require_birth_closed(Response::Killed { id }, id).is_ok());
    }
}

fn hook_evidence(
    runtime: Uuid,
    generation: Option<Uuid>,
    target: Option<super::protocol::HostHookTarget>,
) -> crate::hook_authority::HostEvidence {
    crate::hook_authority::HostEvidence {
        runtime,
        generation,
        target: target.map(|t| crate::hook_authority::HostHookTarget {
            workspace_id: t.workspace_id,
            panel_id: t.panel_id,
            leaf_id: t.leaf_id,
        }),
    }
}
fn spec_key(spec: &crate::runtime::SessionSpec) -> Option<crate::hook_authority::LeafKey> {
    Some(crate::hook_authority::LeafKey {
        workspace: spec.workspace_id?,
        panel: spec.panel_id?,
        leaf: spec.leaf_id?,
    })
}
