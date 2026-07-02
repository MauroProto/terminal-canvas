use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::RngCore;
use url::Url;
use uuid::Uuid;

use std::thread;

use super::auth::hash_passphrase;
use super::http_worker::{CollabHttpJob, CollabHttpOp, CollabHttpWorker};
use super::models::{
    ControlGrant, ControlRequest, ControlRevoke, GuestConnectionState, GuestId, GuestPresence,
    GuestTerminalInput, InviteCode, JoinRequest, ParticipantId, SessionRole, ShareSessionId,
    SharedWorkspaceSnapshot, TerminalControlState, TerminalInputEvent, TrustedDevice,
};
use super::protocol::{
    decode_envelope, decode_invite_code, encode_envelope, encode_invite_code, BrokerControlMessage,
    CollabEnvelope, CreateShareSessionRequest, CreateShareSessionResponse, EndShareSessionRequest,
    JoinDecisionRequest, JoinShareSessionRequest, JoinShareSessionResponse, RotateInviteRequest,
    SessionPayload,
};
use super::server::EmbeddedCollabServer;
use super::tls::generate_tls_material;
use super::transport::{
    broker_message_from_text, json_post, BackgroundTransport, TransportCommand,
};
use crate::utils::platform::default_share_base_url;

const DEFAULT_SHARE_URL: &str = "https://127.0.0.1:8787";
const DEFAULT_INVITE_TTL_HOURS: i64 = 24;

#[derive(Debug, Clone)]
pub struct HostShareOptions {
    pub bind_addr: SocketAddr,
    pub reachable_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CollabMode {
    #[default]
    Inactive,
    Host,
    Guest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CollabSessionState {
    #[default]
    NotSharing,
    Starting,
    Live,
    Disconnected,
    Ended,
}

#[derive(Debug, Clone)]
pub enum CollabEvent {
    RemoteInput {
        guest_id: GuestId,
        input: GuestTerminalInput,
    },
}

#[derive(Debug, Clone, Default)]
pub struct GuestWorkspaceView {
    // Arc so per-frame consumers can take a cheap handle instead of deep
    // cloning the full remote workspace (panel history included).
    pub snapshot: Option<Arc<SharedWorkspaceSnapshot>>,
    pub focused_panel: Option<Uuid>,
    pub my_guest_id: Option<GuestId>,
    pub scroll_offsets: HashMap<Uuid, usize>,
}

#[derive(Debug)]
struct HostSessionContext {
    session_id: ShareSessionId,
    workspace_id: Uuid,
    host_token: String,
    session_secret: String,
    invite_secret: String,
    invite_expires_at: Option<DateTime<Utc>>,
    requires_passphrase: bool,
    tls_cert_pem: String,
    invite_code: String,
    guests: HashMap<GuestId, GuestPresence>,
    pending_joins: Vec<JoinRequest>,
    pending_control_requests: Vec<ControlRequest>,
    terminal_controls: HashMap<Uuid, TerminalControlState>,
    last_snapshot: Option<SharedWorkspaceSnapshot>,
    next_message_seq: u64,
}

#[derive(Debug)]
struct GuestSessionContext {
    session_id: ShareSessionId,
    guest_id: GuestId,
    session_secret: String,
    display_name: String,
    next_message_seq: u64,
}

pub struct CollabManager {
    broker_url: String,
    embedded_server: Option<EmbeddedCollabServer>,
    transport: BackgroundTransport,
    mode: CollabMode,
    session_state: CollabSessionState,
    host: Option<HostSessionContext>,
    guest: Option<GuestSessionContext>,
    guest_view: GuestWorkspaceView,
    remote_inputs: Vec<GuestTerminalInput>,
    remote_input_senders: Vec<GuestId>,
    received_message_seq: HashMap<ParticipantId, u64>,
    last_error: Option<String>,
    http_worker: CollabHttpWorker,
    // Sube en cada stop_session: los resultados HTTP en vuelo de la sesión
    // anterior se descartan al llegar.
    http_generation: u64,
    join_pending: bool,
}

impl Default for CollabManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CollabManager {
    pub fn new() -> Self {
        let broker_url = std::env::var("TERMINAL_CANVAS_SHARE_URL")
            .ok()
            .and_then(|value| normalize_share_url(&value).ok())
            .unwrap_or_else(|| {
                default_share_base_url(8787).unwrap_or_else(|| DEFAULT_SHARE_URL.to_owned())
            });
        Self {
            broker_url,
            embedded_server: None,
            transport: BackgroundTransport::new(),
            mode: CollabMode::Inactive,
            session_state: CollabSessionState::NotSharing,
            host: None,
            guest: None,
            guest_view: GuestWorkspaceView::default(),
            remote_inputs: Vec::new(),
            remote_input_senders: Vec::new(),
            received_message_seq: HashMap::new(),
            last_error: None,
            http_worker: CollabHttpWorker::default(),
            http_generation: 0,
            join_pending: false,
        }
    }

    pub fn mode(&self) -> CollabMode {
        self.mode
    }

    pub fn session_state(&self) -> CollabSessionState {
        self.session_state
    }

    pub fn broker_url(&self) -> &str {
        &self.broker_url
    }

    pub fn set_broker_url(&mut self, broker_url: impl Into<String>) {
        if matches!(self.mode, CollabMode::Inactive) {
            if let Ok(url) = normalize_share_url(&broker_url.into()) {
                self.broker_url = url;
            }
        }
    }

    pub fn invite_code(&self) -> Option<&str> {
        self.host.as_ref().map(|host| host.invite_code.as_str())
    }

    pub fn invite_expires_at(&self) -> Option<DateTime<Utc>> {
        self.host.as_ref().and_then(|host| host.invite_expires_at)
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn pending_joins(&self) -> &[JoinRequest] {
        self.host
            .as_ref()
            .map(|host| host.pending_joins.as_slice())
            .unwrap_or(&[])
    }

    pub fn pending_control_requests(&self) -> &[ControlRequest] {
        self.host
            .as_ref()
            .map(|host| host.pending_control_requests.as_slice())
            .unwrap_or(&[])
    }

    pub fn guests(&self) -> Vec<GuestPresence> {
        self.host
            .as_ref()
            .map(|host| host.guests.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn guest_view(&self) -> &GuestWorkspaceView {
        &self.guest_view
    }

    pub fn shared_workspace_id(&self) -> Option<Uuid> {
        self.host.as_ref().map(|host| host.workspace_id)
    }

    pub fn drain_events(&mut self) -> Vec<CollabEvent> {
        self.handle_transport_events();
        let inputs = std::mem::take(&mut self.remote_inputs);
        let senders = std::mem::take(&mut self.remote_input_senders);
        inputs
            .into_iter()
            .zip(senders)
            .map(|(input, guest_id)| CollabEvent::RemoteInput { guest_id, input })
            .collect()
    }

    pub fn start_host_session(
        &mut self,
        workspace_id: Uuid,
        options: HostShareOptions,
        session_passphrase: Option<String>,
        trusted_devices: Vec<TrustedDevice>,
    ) -> anyhow::Result<()> {
        self.stop_session();
        self.session_state = CollabSessionState::Starting;
        let reachable_url = normalize_share_url(&options.reachable_url)?;
        let session_secret = random_secret();
        let invite_secret = random_secret();
        let invite_expires_at = Some(default_invite_expires_at());
        let tls_material =
            generate_tls_material(certificate_subject_names(&reachable_url, options.bind_addr))?;
        let server = EmbeddedCollabServer::start(
            options.bind_addr,
            tls_material.cert_pem.clone(),
            tls_material.key_pem.clone(),
        )?;
        let local_api_url = server.local_api_url().to_owned();
        let passphrase_hash = session_passphrase
            .as_deref()
            .map(hash_passphrase)
            .transpose()?;
        let response: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", local_api_url.trim_end_matches('/')),
            &CreateShareSessionRequest {
                session_secret: session_secret.clone(),
                invite_secret: invite_secret.clone(),
                invite_expires_at,
                passphrase_hash,
                trusted_devices,
            },
            Some(tls_material.cert_pem.as_str()),
        )?;
        let invite_code = encode_invite_code(&InviteCode {
            broker_url: reachable_url.clone(),
            session_id: response.session_id,
            session_secret: session_secret.clone(),
            invite_secret: Some(invite_secret.clone()),
            expires_at: invite_expires_at,
            requires_passphrase: session_passphrase.is_some(),
            tls_cert_pem: Some(tls_material.cert_pem.clone()),
        })?;
        let websocket_url = broker_ws_url(
            &local_api_url,
            response.session_id,
            &response.host_token,
            SessionRole::Host,
        )?;
        self.transport.send(TransportCommand::Connect {
            websocket_url,
            tls_cert_pem: Some(tls_material.cert_pem.clone()),
        });
        self.broker_url = reachable_url;
        self.embedded_server = Some(server);
        self.host = Some(HostSessionContext {
            session_id: response.session_id,
            workspace_id,
            host_token: response.host_token,
            session_secret,
            invite_secret,
            invite_expires_at,
            requires_passphrase: session_passphrase.is_some(),
            tls_cert_pem: tls_material.cert_pem,
            invite_code,
            guests: HashMap::new(),
            pending_joins: Vec::new(),
            pending_control_requests: Vec::new(),
            terminal_controls: HashMap::new(),
            last_snapshot: None,
            next_message_seq: 1,
        });
        self.mode = CollabMode::Host;
        Ok(())
    }

    pub fn join_session(
        &mut self,
        invite_code: &str,
        display_name: String,
        session_passphrase: Option<String>,
        device_id: String,
    ) -> anyhow::Result<()> {
        self.stop_session();
        let invite = decode_invite_code(invite_code)?;
        if invite
            .expires_at
            .map(|expires_at| expires_at <= Utc::now())
            .unwrap_or(false)
        {
            anyhow::bail!("Invite expired. Pedile al host que rote el invite.");
        }
        validate_invite_broker_transport(&invite)?;
        let invite_secret = required_invite_secret(&invite)?;
        let body = serde_json::to_value(&JoinShareSessionRequest {
            display_name: display_name.clone(),
            invite_secret,
            device_id,
            passphrase: session_passphrase,
        })?;
        // El POST va al broker remoto: se resuelve en el worker HTTP y el
        // resto del alta ocurre en apply_http_outcomes cuando llega.
        self.session_state = CollabSessionState::Starting;
        self.join_pending = true;
        self.http_worker.request(CollabHttpJob {
            url: format!(
                "{}/v1/share-sessions/{}/join",
                invite.broker_url.trim_end_matches('/'),
                invite.session_id.0
            ),
            body,
            tls_cert_pem: invite.tls_cert_pem.clone(),
            generation: self.http_generation,
            op: CollabHttpOp::JoinSession {
                invite,
                display_name,
            },
        });
        Ok(())
    }

    /// Hay un join encolado cuyo resultado todavía no llegó del worker HTTP.
    pub fn join_in_flight(&self) -> bool {
        self.join_pending
    }

    pub fn stop_session(&mut self) {
        // Los resultados HTTP en vuelo pertenecen a la sesión que se cierra.
        self.http_generation = self.http_generation.wrapping_add(1);
        self.join_pending = false;
        let server = self.embedded_server.take();
        let end_request = self.host.as_ref().map(|host| {
            (
                format!(
                    "{}/v1/share-sessions/{}/end",
                    server
                        .as_ref()
                        .map(|server| server.local_api_url().to_owned())
                        .unwrap_or_else(|| self.broker_url.trim_end_matches('/').to_owned()),
                    host.session_id.0
                ),
                EndShareSessionRequest {
                    host_token: host.host_token.clone(),
                },
                host.tls_cert_pem.clone(),
            )
        });
        if end_request.is_some() || server.is_some() {
            // El aviso de cierre puede esperar red y server.stop() joinea el
            // hilo del servidor (hasta ~1s): ambos fuera del hilo de UI. Si
            // el POST no llega, el broker limpia la sesión por grace period.
            let spawned = thread::Builder::new()
                .name("collab-stop".to_owned())
                .spawn(move || {
                    if let Some((url, body, tls_cert_pem)) = end_request {
                        if let Err(err) = json_post::<_, serde_json::Value>(
                            &url,
                            &body,
                            Some(tls_cert_pem.as_str()),
                        ) {
                            log::warn!("Failed to end share session on broker: {err}");
                        }
                    }
                    if let Some(mut server) = server {
                        let _ = server.stop();
                    }
                });
            if let Err(err) = spawned {
                // El closure se descarta y el Drop del server lo detiene acá.
                log::warn!("failed to spawn collab-stop thread: {err}");
            }
        }
        self.transport.send(TransportCommand::Close);
        self.mode = CollabMode::Inactive;
        self.session_state = CollabSessionState::NotSharing;
        self.host = None;
        self.guest = None;
        self.guest_view = GuestWorkspaceView::default();
        self.remote_inputs.clear();
        self.remote_input_senders.clear();
        self.received_message_seq.clear();
        self.last_error = None;
    }

    pub fn approve_join(&mut self, guest_id: GuestId) {
        self.send_join_decision(guest_id, true);
    }

    pub fn deny_join(&mut self, guest_id: GuestId) {
        self.send_join_decision(guest_id, false);
    }

    fn send_join_decision(&mut self, guest_id: GuestId, approve: bool) {
        let base = self.broker_api_base();
        let generation = self.http_generation;
        let Some(host) = &mut self.host else {
            return;
        };
        // Se saca de pending_joins de una para respuesta inmediata en la UI;
        // si el POST falla, apply_http_outcomes lo repone.
        let removed_request = host
            .pending_joins
            .iter()
            .position(|join| join.guest_id == guest_id)
            .map(|index| host.pending_joins.remove(index));
        let body = match serde_json::to_value(&JoinDecisionRequest {
            host_token: host.host_token.clone(),
            guest_id,
        }) {
            Ok(body) => body,
            Err(err) => {
                self.last_error = Some(err.to_string());
                return;
            }
        };
        let action = if approve { "approve" } else { "deny" };
        let op = if approve {
            CollabHttpOp::ApproveJoin {
                guest_id,
                removed_request,
            }
        } else {
            CollabHttpOp::DenyJoin {
                guest_id,
                removed_request,
            }
        };
        self.http_worker.request(CollabHttpJob {
            url: format!(
                "{}/v1/share-sessions/{}/{}",
                base, host.session_id.0, action
            ),
            body,
            tls_cert_pem: Some(host.tls_cert_pem.clone()),
            generation,
            op,
        });
    }

    fn broker_api_base(&self) -> String {
        self.embedded_server
            .as_ref()
            .map(|server| server.local_api_url().to_owned())
            .unwrap_or_else(|| self.broker_url.trim_end_matches('/').to_owned())
    }

    pub fn rotate_invite(&mut self) {
        let base = self.broker_api_base();
        let generation = self.http_generation;
        let Some(host) = &self.host else {
            return;
        };
        let invite_secret = random_secret();
        let invite_expires_at = Some(default_invite_expires_at());
        let body = match serde_json::to_value(&RotateInviteRequest {
            host_token: host.host_token.clone(),
            invite_secret: invite_secret.clone(),
            invite_expires_at,
        }) {
            Ok(body) => body,
            Err(err) => {
                self.last_error = Some(err.to_string());
                return;
            }
        };
        // El invite local recién cambia cuando el broker confirma la
        // rotación; mientras tanto el código viejo sigue siendo el válido.
        self.http_worker.request(CollabHttpJob {
            url: format!(
                "{}/v1/share-sessions/{}/rotate-invite",
                base, host.session_id.0
            ),
            body,
            tls_cert_pem: Some(host.tls_cert_pem.clone()),
            generation,
            op: CollabHttpOp::RotateInvite {
                invite_secret,
                invite_expires_at,
            },
        });
    }

    pub fn publish_snapshot(&mut self, mut snapshot: SharedWorkspaceSnapshot) {
        let Some(host) = &mut self.host else {
            return;
        };
        snapshot.guests = host.guests.values().cloned().collect();
        snapshot.terminal_controls = host.terminal_controls.values().cloned().collect();
        for panel in &mut snapshot.panels {
            if let Some(control) = host.terminal_controls.get(&panel.panel_id) {
                panel.controller = control.controller;
                panel.controller_name = control.controller_name.clone();
                panel.queue_len = control.queue.len();
            }
        }
        if host
            .last_snapshot
            .as_ref()
            .map(|last| snapshots_equivalent(last, &snapshot))
            .unwrap_or(false)
        {
            return;
        }
        host.last_snapshot = Some(snapshot.clone());
        let payload = SessionPayload::WorkspaceSnapshot { snapshot };
        if let Ok(envelope) = encode_envelope(
            host.session_id,
            super::models::ParticipantId::Host,
            host.next_message_seq,
            &host.session_secret,
            &payload,
        ) {
            host.next_message_seq += 1;
            if let Ok(bytes) = rmp_serde::to_vec_named(&envelope) {
                self.transport.send(TransportCommand::SendBinary(bytes));
            }
        }
    }

    pub fn request_control(&mut self, terminal_id: Uuid) {
        if !matches!(self.session_state, CollabSessionState::Live) {
            return;
        }
        let Some(guest) = &mut self.guest else {
            return;
        };
        let payload = SessionPayload::ControlRequest {
            request: ControlRequest {
                terminal_id,
                guest_id: guest.guest_id,
                display_name: guest.display_name.clone(),
                requested_at: Utc::now(),
            },
        };
        if let Ok(envelope) = encode_envelope(
            guest.session_id,
            super::models::ParticipantId::Guest(guest.guest_id),
            guest.next_message_seq,
            &guest.session_secret,
            &payload,
        ) {
            guest.next_message_seq += 1;
            if let Ok(bytes) = rmp_serde::to_vec_named(&envelope) {
                self.transport.send(TransportCommand::SendBinary(bytes));
            }
        }
    }

    pub fn grant_control(&mut self, terminal_id: Uuid, guest_id: GuestId) {
        let Some(host) = &mut self.host else {
            return;
        };
        let controller_name = host
            .guests
            .get(&guest_id)
            .map(|guest| guest.display_name.clone());
        host.terminal_controls.insert(
            terminal_id,
            TerminalControlState {
                terminal_id,
                controller: Some(guest_id),
                controller_name,
                queue: host
                    .terminal_controls
                    .get(&terminal_id)
                    .map(|control| {
                        control
                            .queue
                            .iter()
                            .copied()
                            .filter(|queued| *queued != guest_id)
                            .collect()
                    })
                    .unwrap_or_default(),
            },
        );
        host.pending_control_requests
            .retain(|request| request.terminal_id != terminal_id || request.guest_id != guest_id);
        let payload = SessionPayload::ControlGrant {
            grant: ControlGrant {
                terminal_id,
                guest_id,
                granted_at: Utc::now(),
            },
        };
        self.send_host_payload(payload);
    }

    pub fn revoke_control(&mut self, terminal_id: Uuid, reason: impl Into<String>) {
        let Some(host) = &mut self.host else {
            return;
        };
        let guest_id = host
            .terminal_controls
            .get(&terminal_id)
            .and_then(|control| control.controller);
        if let Some(control) = host.terminal_controls.get_mut(&terminal_id) {
            control.controller = None;
            control.controller_name = None;
        }
        let payload = SessionPayload::ControlRevoke {
            revoke: ControlRevoke {
                terminal_id,
                guest_id,
                reason: reason.into(),
            },
        };
        self.send_host_payload(payload);
    }

    pub fn controller_for(&self, terminal_id: Uuid) -> Option<GuestId> {
        self.host
            .as_ref()
            .and_then(|host| host.terminal_controls.get(&terminal_id))
            .and_then(|control| control.controller)
    }

    pub fn note_control_request(&mut self, request: ControlRequest) {
        let Some(host) = &mut self.host else {
            return;
        };
        if host.pending_control_requests.iter().any(|existing| {
            existing.terminal_id == request.terminal_id && existing.guest_id == request.guest_id
        }) {
            return;
        }
        host.pending_control_requests.push(request.clone());
        host.terminal_controls
            .entry(request.terminal_id)
            .or_insert_with(|| TerminalControlState {
                terminal_id: request.terminal_id,
                controller: None,
                controller_name: None,
                queue: Vec::new(),
            })
            .queue
            .push(request.guest_id);
    }

    pub fn send_guest_input(&mut self, terminal_id: Uuid, events: Vec<TerminalInputEvent>) {
        if events.is_empty() || !matches!(self.session_state, CollabSessionState::Live) {
            return;
        }
        let Some(guest) = &mut self.guest else {
            return;
        };
        let payload = SessionPayload::GuestInput {
            input: GuestTerminalInput {
                terminal_id,
                events,
            },
        };
        if let Ok(envelope) = encode_envelope(
            guest.session_id,
            super::models::ParticipantId::Guest(guest.guest_id),
            guest.next_message_seq,
            &guest.session_secret,
            &payload,
        ) {
            guest.next_message_seq += 1;
            if let Ok(bytes) = rmp_serde::to_vec_named(&envelope) {
                self.transport.send(TransportCommand::SendBinary(bytes));
            }
        }
    }

    pub fn release_control(&mut self, terminal_id: Uuid) {
        if !matches!(self.session_state, CollabSessionState::Live) {
            return;
        }
        let Some(guest) = &mut self.guest else {
            return;
        };
        let payload = SessionPayload::ControlRevoke {
            revoke: ControlRevoke {
                terminal_id,
                guest_id: Some(guest.guest_id),
                reason: "Released by guest".to_owned(),
            },
        };
        if let Ok(envelope) = encode_envelope(
            guest.session_id,
            super::models::ParticipantId::Guest(guest.guest_id),
            guest.next_message_seq,
            &guest.session_secret,
            &payload,
        ) {
            guest.next_message_seq += 1;
            if let Ok(bytes) = rmp_serde::to_vec_named(&envelope) {
                self.transport.send(TransportCommand::SendBinary(bytes));
            }
        }
    }

    pub fn focus_remote_panel(&mut self, panel_id: Uuid) {
        self.guest_view.focused_panel = Some(panel_id);
    }

    pub fn scroll_remote_panel(&mut self, panel_id: Uuid, delta_lines: i32) {
        let offset = self.guest_view.scroll_offsets.entry(panel_id).or_default();
        if delta_lines > 0 {
            *offset = offset.saturating_sub(delta_lines as usize);
        } else {
            *offset = offset.saturating_add((-delta_lines) as usize);
        }
    }

    pub fn release_controls_for_guest(&mut self, guest_id: GuestId) {
        let Some(host) = &self.host else {
            return;
        };
        let controlled_panels = host
            .terminal_controls
            .values()
            .filter(|control| control.controller == Some(guest_id))
            .map(|control| control.terminal_id)
            .collect::<Vec<_>>();
        for terminal_id in controlled_panels {
            self.revoke_control(terminal_id, "Guest disconnected");
        }
        if let Some(host) = &mut self.host {
            host.pending_control_requests
                .retain(|request| request.guest_id != guest_id);
        }
    }

    fn apply_http_outcomes(&mut self) {
        for outcome in self.http_worker.poll() {
            if outcome.generation != self.http_generation {
                // Resultado de una sesión que ya se cerró.
                continue;
            }
            match outcome.op {
                CollabHttpOp::ApproveJoin {
                    guest_id,
                    removed_request,
                } => {
                    if let Err(err) = outcome.result {
                        log::warn!("approve join failed: {err}");
                        self.last_error = Some(format!("No se pudo aprobar el acceso: {err}"));
                        self.restore_pending_join(guest_id, removed_request);
                    }
                }
                CollabHttpOp::DenyJoin {
                    guest_id,
                    removed_request,
                } => {
                    if let Err(err) = outcome.result {
                        log::warn!("deny join failed: {err}");
                        self.last_error = Some(format!("No se pudo rechazar el acceso: {err}"));
                        self.restore_pending_join(guest_id, removed_request);
                    }
                }
                CollabHttpOp::RotateInvite {
                    invite_secret,
                    invite_expires_at,
                } => match outcome.result {
                    Ok(_) => {
                        let broker_url = self.broker_url.clone();
                        if let Some(host) = &mut self.host {
                            host.invite_secret = invite_secret.clone();
                            host.invite_expires_at = invite_expires_at;
                            match encode_invite_code(&InviteCode {
                                broker_url,
                                session_id: host.session_id,
                                session_secret: host.session_secret.clone(),
                                invite_secret: Some(invite_secret),
                                expires_at: invite_expires_at,
                                requires_passphrase: host.requires_passphrase,
                                tls_cert_pem: Some(host.tls_cert_pem.clone()),
                            }) {
                                Ok(code) => host.invite_code = code,
                                Err(err) => self.last_error = Some(err.to_string()),
                            }
                        }
                    }
                    Err(err) => {
                        log::warn!("rotate invite failed: {err}");
                        self.last_error = Some(format!("No se pudo rotar el invite: {err}"));
                    }
                },
                CollabHttpOp::JoinSession {
                    invite,
                    display_name,
                } => {
                    self.join_pending = false;
                    let completed = outcome
                        .result
                        .and_then(|value| {
                            Ok(serde_json::from_value::<JoinShareSessionResponse>(value)?)
                        })
                        .and_then(|response| {
                            let websocket_url = broker_ws_url(
                                &invite.broker_url,
                                invite.session_id,
                                &response.guest_token,
                                SessionRole::Guest,
                            )?;
                            Ok((response, websocket_url))
                        });
                    match completed {
                        Ok((response, websocket_url)) => {
                            self.transport.send(TransportCommand::Connect {
                                websocket_url,
                                tls_cert_pem: invite.tls_cert_pem.clone(),
                            });
                            self.guest = Some(GuestSessionContext {
                                session_id: invite.session_id,
                                guest_id: response.guest_id,
                                session_secret: invite.session_secret,
                                display_name,
                                next_message_seq: 1,
                            });
                            self.guest_view = GuestWorkspaceView {
                                snapshot: None,
                                focused_panel: None,
                                my_guest_id: Some(response.guest_id),
                                scroll_offsets: HashMap::new(),
                            };
                            self.mode = CollabMode::Guest;
                            self.broker_url = invite.broker_url;
                            self.received_message_seq.clear();
                        }
                        Err(err) => {
                            self.session_state = CollabSessionState::NotSharing;
                            self.last_error = Some(err.to_string());
                        }
                    }
                }
            }
        }
    }

    fn restore_pending_join(&mut self, guest_id: GuestId, removed_request: Option<JoinRequest>) {
        let Some(host) = &mut self.host else {
            return;
        };
        if let Some(request) = removed_request {
            if !host
                .pending_joins
                .iter()
                .any(|join| join.guest_id == guest_id)
            {
                host.pending_joins.push(request);
            }
        }
    }

    fn handle_transport_events(&mut self) {
        self.apply_http_outcomes();
        for event in self.transport.drain_events() {
            match event {
                super::transport::TransportEvent::Connected => {
                    if matches!(self.mode, CollabMode::Host) {
                        self.session_state = CollabSessionState::Live;
                    }
                }
                super::transport::TransportEvent::Disconnected => {
                    if !matches!(self.session_state, CollabSessionState::Ended) {
                        self.session_state = CollabSessionState::Disconnected;
                    }
                }
                super::transport::TransportEvent::Error(message) => {
                    self.last_error = Some(message);
                }
                super::transport::TransportEvent::Text(text) => {
                    if let Some(message) = broker_message_from_text(&text) {
                        self.handle_broker_message(message);
                    }
                }
                super::transport::TransportEvent::Binary(binary) => {
                    self.handle_binary_message(&binary);
                }
            }
        }
    }

    fn handle_broker_message(&mut self, message: BrokerControlMessage) {
        match message {
            BrokerControlMessage::Connected { .. } => {
                if matches!(self.mode, CollabMode::Host) {
                    self.session_state = CollabSessionState::Live;
                }
            }
            BrokerControlMessage::JoinRequested { request } => {
                if let Some(host) = &mut self.host {
                    if !host
                        .pending_joins
                        .iter()
                        .any(|join| join.guest_id == request.guest_id)
                    {
                        host.pending_joins.push(request);
                    }
                }
            }
            BrokerControlMessage::JoinApproved { decision } => {
                if self.guest.as_ref().map(|guest| guest.guest_id) == Some(decision.guest_id) {
                    self.session_state = CollabSessionState::Live;
                }
            }
            BrokerControlMessage::JoinDenied { decision } => {
                if self.guest.as_ref().map(|guest| guest.guest_id) == Some(decision.guest_id) {
                    self.session_state = CollabSessionState::Ended;
                    self.last_error = Some("Join request was denied".to_owned());
                }
            }
            BrokerControlMessage::Presence { guests } => {
                if let Some(host) = &mut self.host {
                    host.guests = guests
                        .iter()
                        .cloned()
                        .map(|guest| (guest.id, guest))
                        .collect();
                    let disconnected = host
                        .guests
                        .values()
                        .filter(|guest| {
                            matches!(
                                guest.connection_state,
                                GuestConnectionState::Disconnected | GuestConnectionState::Denied
                            )
                        })
                        .map(|guest| guest.id)
                        .collect::<Vec<_>>();
                    for guest_id in disconnected {
                        self.release_controls_for_guest(guest_id);
                    }
                }
                if let Some(snapshot) = &mut self.guest_view.snapshot {
                    Arc::make_mut(snapshot).guests = guests;
                }
            }
            BrokerControlMessage::HostDisconnected => {
                if matches!(self.mode, CollabMode::Guest) {
                    self.session_state = CollabSessionState::Disconnected;
                    self.last_error =
                        Some("El host perdió la conexión de la sesión compartida".to_owned());
                }
            }
            BrokerControlMessage::HostReconnected => {
                if matches!(self.mode, CollabMode::Guest) {
                    self.session_state = CollabSessionState::Live;
                    self.last_error = None;
                }
            }
            BrokerControlMessage::SessionEnded => {
                self.session_state = CollabSessionState::Ended;
            }
            BrokerControlMessage::Error { message } => {
                self.last_error = Some(message);
            }
        }
    }

    fn handle_binary_message(&mut self, binary: &[u8]) {
        const MAX_ENVELOPE_BYTES: usize = 16 * 1024 * 1024;
        if binary.len() > MAX_ENVELOPE_BYTES {
            log::warn!(
                "Dropping oversized collab envelope ({} bytes)",
                binary.len()
            );
            return;
        }
        let envelope: CollabEnvelope = match rmp_serde::from_slice(binary) {
            Ok(envelope) => envelope,
            Err(err) => {
                self.last_error = Some(err.to_string());
                return;
            }
        };
        let secret = if let Some(host) = &self.host {
            host.session_secret.as_str()
        } else if let Some(guest) = &self.guest {
            guest.session_secret.as_str()
        } else {
            return;
        };
        let payload = match decode_envelope(&envelope, secret) {
            Ok(payload) => payload,
            Err(err) => {
                self.last_error = Some(err.to_string());
                return;
            }
        };
        if let Err(err) = self.validate_message_sequence(envelope.sender_id, envelope.message_seq) {
            self.last_error = Some(err);
            return;
        }

        match payload {
            SessionPayload::WorkspaceSnapshot { snapshot } => {
                // Drop scroll offsets for panels the host no longer shares so
                // the map does not grow across long sessions.
                self.guest_view.scroll_offsets.retain(|panel_id, _| {
                    snapshot
                        .panels
                        .iter()
                        .any(|panel| panel.panel_id == *panel_id)
                });
                self.guest_view.snapshot = Some(Arc::new(snapshot));
            }
            SessionPayload::ControlRequest { request } => {
                self.note_control_request(request);
            }
            SessionPayload::ControlGrant { grant } => {
                self.update_remote_control(grant.terminal_id, Some(grant.guest_id));
            }
            SessionPayload::ControlRevoke { revoke } => {
                if matches!(self.mode, CollabMode::Host) {
                    if let Some(guest_id) = revoke.guest_id {
                        if self.controller_for(revoke.terminal_id) == Some(guest_id) {
                            self.revoke_control(revoke.terminal_id, revoke.reason);
                        }
                    }
                } else {
                    self.update_remote_control(revoke.terminal_id, None);
                }
            }
            SessionPayload::GuestInput { input } => {
                if let super::models::ParticipantId::Guest(guest_id) = envelope.sender_id {
                    self.remote_inputs.push(input);
                    self.remote_input_senders.push(guest_id);
                }
            }
        }
    }

    fn update_remote_control(&mut self, terminal_id: Uuid, controller: Option<GuestId>) {
        if let Some(snapshot) = &mut self.guest_view.snapshot {
            let snapshot = Arc::make_mut(snapshot);
            for panel in &mut snapshot.panels {
                if panel.panel_id == terminal_id {
                    panel.controller = controller;
                    panel.controller_name = controller.and_then(|guest_id| {
                        snapshot
                            .guests
                            .iter()
                            .find(|guest| guest.id == guest_id)
                            .map(|guest| guest.display_name.clone())
                    });
                }
            }
            for control in &mut snapshot.terminal_controls {
                if control.terminal_id == terminal_id {
                    control.controller = controller;
                    control.controller_name = controller.and_then(|guest_id| {
                        snapshot
                            .guests
                            .iter()
                            .find(|guest| guest.id == guest_id)
                            .map(|guest| guest.display_name.clone())
                    });
                }
            }
        }
    }

    fn send_host_payload(&mut self, payload: SessionPayload) {
        let Some(host) = &mut self.host else {
            return;
        };
        if let Ok(envelope) = encode_envelope(
            host.session_id,
            super::models::ParticipantId::Host,
            host.next_message_seq,
            &host.session_secret,
            &payload,
        ) {
            host.next_message_seq += 1;
            if let Ok(bytes) = rmp_serde::to_vec_named(&envelope) {
                self.transport.send(TransportCommand::SendBinary(bytes));
            }
        }
    }

    fn validate_message_sequence(
        &mut self,
        sender_id: ParticipantId,
        message_seq: u64,
    ) -> Result<(), String> {
        let expected = self
            .received_message_seq
            .get(&sender_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        if message_seq != expected {
            return Err(format!(
                "Invalid message sequence for {:?}: expected {}, got {}",
                sender_id, expected, message_seq
            ));
        }
        self.received_message_seq.insert(sender_id, message_seq);
        Ok(())
    }
}

fn broker_ws_url(
    broker_url: &str,
    session_id: ShareSessionId,
    token: &str,
    role: SessionRole,
) -> anyhow::Result<String> {
    let trimmed = broker_url.trim().trim_end_matches('/');
    let parsed = Url::parse(trimmed)?;
    if parsed.scheme() != "https" {
        anyhow::bail!("Trusted Live requires https:// broker URLs");
    }
    let mut url = trimmed.replacen("https://", "wss://", 1);
    url.push_str(&format!(
        "/v1/share-sessions/{}/stream?token={}&role={}",
        session_id.0,
        token,
        match role {
            SessionRole::Host => "host",
            SessionRole::Guest => "guest",
        }
    ));
    Ok(url)
}

fn validate_invite_broker_transport(invite: &InviteCode) -> anyhow::Result<()> {
    let parsed = Url::parse(invite.broker_url.trim())?;
    if parsed.scheme() != "https" {
        anyhow::bail!("Invite broker URL must use https://");
    }
    if parsed.host_str().is_none() {
        anyhow::bail!("Invite broker URL missing host");
    }
    if invite
        .tls_cert_pem
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        anyhow::bail!("Invite missing pinned TLS certificate");
    }
    Ok(())
}

fn normalize_share_url(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let candidate = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let parsed = Url::parse(&candidate)?;
    if parsed.scheme() != "https" {
        anyhow::bail!("Trusted Live directo seguro requiere https://");
    }
    if parsed.host_str().is_none() {
        anyhow::bail!("Missing host in reachable URL");
    }
    if parsed.port().is_none() {
        anyhow::bail!("Reachable URL must include a port");
    }
    Ok(candidate)
}

pub fn bind_addr_for_share_url(share_url: &str) -> anyhow::Result<SocketAddr> {
    let parsed = Url::parse(share_url)?;
    if parsed.scheme() != "https" {
        anyhow::bail!("Trusted Live directo seguro requiere https://");
    }
    let port = parsed
        .port()
        .ok_or_else(|| anyhow::anyhow!("Reachable URL must include a port"))?;
    Ok(SocketAddr::from(([0, 0, 0, 0], port)))
}

fn certificate_subject_names(reachable_url: &str, bind_addr: SocketAddr) -> Vec<String> {
    let mut names = Vec::new();
    names.push("localhost".to_owned());
    names.push("127.0.0.1".to_owned());

    if let Ok(parsed) = Url::parse(reachable_url) {
        if let Some(host) = parsed.host_str() {
            if !names.iter().any(|value| value == host) {
                names.push(host.to_owned());
            }
        }
    }

    if !bind_addr.ip().is_unspecified() {
        let ip = bind_addr.ip().to_string();
        if !names.iter().any(|value| value == &ip) {
            names.push(ip);
        }
    }

    names
}

fn random_secret() -> String {
    let mut secret = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut secret);
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(secret)
}

fn required_invite_secret(invite: &InviteCode) -> anyhow::Result<String> {
    invite
        .invite_secret
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Invite code missing invite secret"))
}

fn default_invite_expires_at() -> DateTime<Utc> {
    Utc::now() + ChronoDuration::hours(DEFAULT_INVITE_TTL_HOURS)
}

fn snapshots_equivalent(left: &SharedWorkspaceSnapshot, right: &SharedWorkspaceSnapshot) -> bool {
    left.workspace_id == right.workspace_id
        && left.workspace_name == right.workspace_name
        && left.guests == right.guests
        && left.terminal_controls == right.terminal_controls
        && left.panels == right.panels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::models::JoinDecision;
    use crate::collab::models::ParticipantId;
    use chrono::{Duration, TimeZone};

    fn sample_snapshot(timestamp_offset_secs: i64) -> SharedWorkspaceSnapshot {
        SharedWorkspaceSnapshot {
            workspace_id: Uuid::new_v4(),
            workspace_name: "Workspace".to_owned(),
            generated_at: Utc
                .timestamp_opt(1_700_000_000 + timestamp_offset_secs, 0)
                .unwrap(),
            guests: Vec::new(),
            terminal_controls: Vec::new(),
            panels: Vec::new(),
        }
    }

    #[test]
    fn snapshots_equivalent_ignores_generated_at() {
        let left = sample_snapshot(0);
        let mut right = left.clone();
        right.generated_at = left.generated_at + Duration::seconds(5);
        assert!(snapshots_equivalent(&left, &right));
    }

    #[test]
    fn guest_stays_starting_until_join_approved() {
        let guest_id = GuestId(Uuid::new_v4());
        let mut manager = CollabManager::new();
        manager.mode = CollabMode::Guest;
        manager.session_state = CollabSessionState::Starting;
        manager.guest = Some(GuestSessionContext {
            session_id: ShareSessionId(Uuid::new_v4()),
            guest_id,
            session_secret: "secret".to_owned(),
            display_name: "Guest".to_owned(),
            next_message_seq: 1,
        });

        manager.handle_broker_message(BrokerControlMessage::Connected {
            role: SessionRole::Guest,
            guest_id: Some(guest_id),
        });
        assert_eq!(manager.session_state, CollabSessionState::Starting);

        manager.handle_broker_message(BrokerControlMessage::JoinApproved {
            decision: JoinDecision {
                guest_id,
                approved: true,
            },
        });
        assert_eq!(manager.session_state, CollabSessionState::Live);
    }

    #[test]
    fn guest_tracks_host_disconnect_and_reconnect() {
        let guest_id = GuestId(Uuid::new_v4());
        let mut manager = CollabManager::new();
        manager.mode = CollabMode::Guest;
        manager.session_state = CollabSessionState::Live;
        manager.guest = Some(GuestSessionContext {
            session_id: ShareSessionId(Uuid::new_v4()),
            guest_id,
            session_secret: "secret".to_owned(),
            display_name: "Guest".to_owned(),
            next_message_seq: 1,
        });

        manager.handle_broker_message(BrokerControlMessage::HostDisconnected);
        assert_eq!(manager.session_state, CollabSessionState::Disconnected);

        manager.handle_broker_message(BrokerControlMessage::HostReconnected);
        assert_eq!(manager.session_state, CollabSessionState::Live);
    }

    #[test]
    fn normalize_share_url_adds_scheme_and_trims_slash() {
        let normalized = normalize_share_url("192.168.1.20:8787/").expect("normalize url");
        assert_eq!(normalized, "https://192.168.1.20:8787");
    }

    #[test]
    fn bind_addr_for_share_url_uses_any_ipv4_and_url_port() {
        let bind_addr =
            bind_addr_for_share_url("https://mi-host.example.com:9123").expect("bind addr");
        assert_eq!(bind_addr, SocketAddr::from(([0, 0, 0, 0], 9123)));
    }

    #[test]
    fn normalize_share_url_rejects_http_for_secure_direct_host_mode() {
        let err =
            normalize_share_url("http://mi-host.example.com:443").expect_err("http should fail");
        assert!(err.to_string().contains("https://"));
    }

    #[test]
    fn join_requires_explicit_invite_secret() {
        let invite = InviteCode {
            broker_url: "https://127.0.0.1:8787".to_owned(),
            session_id: ShareSessionId(Uuid::new_v4()),
            session_secret: random_secret(),
            invite_secret: None,
            expires_at: None,
            requires_passphrase: false,
            tls_cert_pem: None,
        };

        let err = required_invite_secret(&invite).expect_err("missing invite secret should fail");
        assert!(err.to_string().contains("invite secret"));
    }

    #[test]
    fn join_rejects_insecure_invite_broker_url() {
        let invite = InviteCode {
            broker_url: "http://127.0.0.1:8787".to_owned(),
            session_id: ShareSessionId(Uuid::new_v4()),
            session_secret: random_secret(),
            invite_secret: Some("invite-secret".to_owned()),
            expires_at: None,
            requires_passphrase: false,
            tls_cert_pem: None,
        };

        let err =
            validate_invite_broker_transport(&invite).expect_err("http invite broker should fail");
        assert!(err.to_string().contains("https://"));
    }

    #[test]
    fn join_requires_pinned_invite_certificate() {
        let invite = InviteCode {
            broker_url: "https://127.0.0.1:8787".to_owned(),
            session_id: ShareSessionId(Uuid::new_v4()),
            session_secret: random_secret(),
            invite_secret: Some("invite-secret".to_owned()),
            expires_at: None,
            requires_passphrase: false,
            tls_cert_pem: None,
        };

        let err =
            validate_invite_broker_transport(&invite).expect_err("missing pinned cert should fail");
        assert!(err.to_string().contains("certificate"));
    }

    #[test]
    fn broker_ws_url_rejects_plaintext_broker_url() {
        let err = broker_ws_url(
            "http://127.0.0.1:8787",
            ShareSessionId(Uuid::new_v4()),
            "token",
            SessionRole::Guest,
        )
        .expect_err("plain websocket broker should fail");

        assert!(err.to_string().contains("https://"));
    }

    #[test]
    fn guest_rejects_replayed_binary_message_sequence() {
        let secret = random_secret();
        let mut manager = CollabManager::new();
        manager.mode = CollabMode::Guest;
        manager.session_state = CollabSessionState::Live;
        manager.guest = Some(GuestSessionContext {
            session_id: ShareSessionId(Uuid::new_v4()),
            guest_id: GuestId(Uuid::new_v4()),
            session_secret: secret.clone(),
            display_name: "Guest".to_owned(),
            next_message_seq: 1,
        });

        let payload = SessionPayload::WorkspaceSnapshot {
            snapshot: sample_snapshot(0),
        };
        let envelope = encode_envelope(
            manager.guest.as_ref().unwrap().session_id,
            ParticipantId::Host,
            1,
            &secret,
            &payload,
        )
        .unwrap();
        let binary = rmp_serde::to_vec_named(&envelope).unwrap();

        manager.handle_binary_message(&binary);
        assert!(manager.guest_view.snapshot.is_some());
        assert!(manager.last_error.is_none());

        manager.handle_binary_message(&binary);
        assert!(manager
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("sequence"));
    }
}
