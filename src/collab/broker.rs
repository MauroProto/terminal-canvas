//! Núcleo del broker de colaboración: sesiones, relay WebSocket, presencia y
//! limpieza. Es la única implementación; los dos despliegues la parametrizan
//! con [`BrokerConfig`]:
//!
//! - el servidor embebido ([`super::server::EmbeddedCollabServer`]) la sirve
//!   sobre TLS y restringe la creación de sesiones a loopback;
//! - el binario `collab-broker` la sirve como HTTP plano detrás de un proxy.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

use super::auth::{constant_time_str_eq, verify_passphrase};
use super::models::{
    GuestConnectionState, GuestId, GuestPresence, JoinDecision, JoinRequest, SessionRole,
    ShareSessionId, TrustedDevice,
};
use super::protocol::{
    BrokerControlMessage, CreateShareSessionRequest, CreateShareSessionResponse,
    EndShareSessionRequest, JoinDecisionRequest, JoinShareSessionRequest, JoinShareSessionResponse,
    RotateInviteRequest,
};

const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);
const HOST_RECONNECT_GRACE: Duration = Duration::from_secs(20);
const GUEST_RECONNECT_GRACE: Duration = Duration::from_secs(45);
const PENDING_JOIN_TTL: Duration = Duration::from_secs(120);
const DENIED_GUEST_TTL: Duration = Duration::from_secs(30);
const JOIN_BACKOFF_MAX_SECS: u64 = 30;
// Clients ping every 10s, so a quiet-but-healthy connection never gets close
// to this limit; connections that stop pinging are reaped instead of pinning
// resources forever.
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(75);
const MAX_WS_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
// Bounds per-connection memory: a consumer that stops reading gets its
// oldest-undelivered messages dropped instead of growing the queue without
// limit. The protocol is snapshot-based, so a later snapshot supersedes any
// dropped one.
const OUTBOUND_QUEUE_CAPACITY: usize = 512;

/// Diferencias de política entre el broker embebido y el standalone.
#[derive(Clone, Copy, Debug)]
pub struct BrokerConfig {
    /// Solo conexiones loopback pueden crear sesiones. El servidor embebido lo
    /// activa porque su API puede quedar expuesta a la LAN para los guests.
    pub require_loopback_session_creation: bool,
}

#[derive(Clone)]
pub struct BrokerState {
    config: BrokerConfig,
    inner: Arc<Mutex<Sessions>>,
}

impl BrokerState {
    pub fn new(config: BrokerConfig) -> Self {
        Self {
            config,
            inner: Arc::new(Mutex::new(Sessions::default())),
        }
    }
}

#[derive(Default)]
struct Sessions {
    sessions: HashMap<ShareSessionId, SessionRecord>,
}

struct SessionRecord {
    #[allow(dead_code)]
    session_secret: String,
    invite_secret: String,
    invite_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    passphrase_hash: Option<String>,
    host_token: String,
    host_tx: Option<mpsc::Sender<Message>>,
    host_connection_id: Option<Uuid>,
    host_last_seen: Instant,
    host_disconnected_at: Option<Instant>,
    failed_join_attempts: u32,
    join_locked_until: Option<Instant>,
    trusted_devices: HashMap<String, TrustedDevice>,
    guests: HashMap<GuestId, GuestRecord>,
}

struct GuestRecord {
    token: String,
    display_name: String,
    #[allow(dead_code)]
    device_id: String,
    joined_at: chrono::DateTime<chrono::Utc>,
    connection_state: GuestConnectionState,
    tx: Option<mpsc::Sender<Message>>,
    connection_id: Option<Uuid>,
    last_seen: Instant,
    disconnected_at: Option<Instant>,
}

#[derive(Debug, Deserialize)]
struct StreamQuery {
    token: String,
    role: String,
}

#[derive(Debug, Serialize)]
struct OkResponse {
    ok: bool,
}

#[derive(Clone, Copy)]
enum StreamAuth {
    Host,
    Guest(GuestId),
}

pub fn build_router(state: BrokerState) -> Router {
    Router::new()
        .route("/v1/share-sessions", post(create_share_session))
        .route("/v1/share-sessions/:id/join", post(join_share_session))
        .route("/v1/share-sessions/:id/approve", post(approve_join))
        .route("/v1/share-sessions/:id/deny", post(deny_join))
        .route("/v1/share-sessions/:id/rotate-invite", post(rotate_invite))
        .route("/v1/share-sessions/:id/end", post(end_share_session))
        .route("/v1/share-sessions/:id/stream", get(stream_session))
        .with_state(state)
}

pub fn spawn_cleanup_task(state: BrokerState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        loop {
            ticker.tick().await;
            cleanup_expired_sessions(&state, Instant::now()).await;
        }
    });
}

async fn create_share_session(
    peer_addr: Option<ConnectInfo<SocketAddr>>,
    State(state): State<BrokerState>,
    Json(body): Json<CreateShareSessionRequest>,
) -> Result<Json<CreateShareSessionResponse>, (StatusCode, String)> {
    if state.config.require_loopback_session_creation {
        // Fail closed: sin ConnectInfo (router servido sin
        // into_make_service_with_connect_info) también se rechaza.
        let is_loopback = peer_addr
            .map(|ConnectInfo(addr)| addr.ip().is_loopback())
            .unwrap_or(false);
        if !is_loopback {
            return Err((
                StatusCode::FORBIDDEN,
                "Session creation is restricted to the local host".to_owned(),
            ));
        }
    }
    let session_id = ShareSessionId(Uuid::new_v4());
    let host_token = random_token();
    let session = SessionRecord {
        session_secret: body.session_secret,
        invite_secret: body.invite_secret,
        invite_expires_at: body.invite_expires_at,
        passphrase_hash: body.passphrase_hash,
        host_token: host_token.clone(),
        host_tx: None,
        host_connection_id: None,
        host_last_seen: Instant::now(),
        host_disconnected_at: Some(Instant::now()),
        failed_join_attempts: 0,
        join_locked_until: None,
        trusted_devices: body
            .trusted_devices
            .into_iter()
            .map(|device| (device.device_id.clone(), device))
            .collect(),
        guests: HashMap::new(),
    };
    state
        .inner
        .lock()
        .await
        .sessions
        .insert(session_id, session);
    Ok(Json(CreateShareSessionResponse {
        session_id,
        host_token,
    }))
}

async fn join_share_session(
    State(state): State<BrokerState>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<JoinShareSessionRequest>,
) -> Result<Json<JoinShareSessionResponse>, (StatusCode, String)> {
    let body = sanitize_join_request(body)
        .map_err(|reason| (StatusCode::BAD_REQUEST, reason.to_owned()))?;
    let session_id = ShareSessionId(session_id);
    let mut guard = state.inner.lock().await;
    let session = guard
        .sessions
        .get_mut(&session_id)
        .ok_or((StatusCode::NOT_FOUND, "Session not found".to_owned()))?;
    let now = Instant::now();
    if let Some(locked_until) = session.join_locked_until {
        if locked_until > now {
            let wait_secs = locked_until.saturating_duration_since(now).as_secs().max(1);
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                format!("Too many failed attempts. Wait {wait_secs}s and try again."),
            ));
        }
        session.join_locked_until = None;
    }
    if session
        .invite_expires_at
        .map(|expires_at| expires_at <= Utc::now())
        .unwrap_or(false)
    {
        return Err((StatusCode::GONE, "Invite expired".to_owned()));
    }
    if !constant_time_str_eq(&session.invite_secret, &body.invite_secret) {
        return Err((StatusCode::UNAUTHORIZED, "Invalid invite secret".to_owned()));
    }
    if let Some(passphrase_hash) = &session.passphrase_hash {
        let Some(passphrase) = body.passphrase.as_deref() else {
            register_failed_join_attempt(session, now);
            return Err((
                StatusCode::UNAUTHORIZED,
                "Session passphrase required".to_owned(),
            ));
        };
        match verify_passphrase(passphrase_hash, passphrase) {
            Ok(true) => {}
            Ok(false) => {
                register_failed_join_attempt(session, now);
                return Err((
                    StatusCode::UNAUTHORIZED,
                    "Invalid session passphrase".to_owned(),
                ));
            }
            Err(_) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to verify session passphrase".to_owned(),
                ));
            }
        }
    }
    session.failed_join_attempts = 0;
    session.join_locked_until = None;

    if active_guest_count(session) >= 3 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Participant limit reached".to_owned(),
        ));
    }

    let guest_id = GuestId(Uuid::new_v4());
    let guest_token = random_token();
    let auto_approved = session.trusted_devices.contains_key(&body.device_id);
    if auto_approved {
        if let Some(trusted) = session.trusted_devices.get_mut(&body.device_id) {
            trusted.last_display_name = body.display_name.clone();
            trusted.last_seen_at = Utc::now();
        }
    }
    session.guests.insert(
        guest_id,
        GuestRecord {
            token: guest_token.clone(),
            display_name: body.display_name.clone(),
            device_id: body.device_id.clone(),
            joined_at: Utc::now(),
            connection_state: if auto_approved {
                GuestConnectionState::Approved
            } else {
                GuestConnectionState::Pending
            },
            tx: None,
            connection_id: None,
            last_seen: Instant::now(),
            disconnected_at: None,
        },
    );

    if !auto_approved {
        if let Some(host_tx) = &session.host_tx {
            let request = JoinRequest {
                guest_id,
                display_name: body.display_name,
                device_id: body.device_id,
                requested_at: Utc::now(),
            };
            send_json(host_tx, &BrokerControlMessage::JoinRequested { request });
        }
    }

    Ok(Json(JoinShareSessionResponse {
        guest_id,
        guest_token,
        auto_approved,
    }))
}

async fn rotate_invite(
    State(state): State<BrokerState>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<RotateInviteRequest>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    let session_id = ShareSessionId(session_id);
    let mut guard = state.inner.lock().await;
    let session = guard
        .sessions
        .get_mut(&session_id)
        .ok_or((StatusCode::NOT_FOUND, "Session not found".to_owned()))?;
    if !constant_time_str_eq(&session.host_token, &body.host_token) {
        return Err((StatusCode::UNAUTHORIZED, "Invalid host token".to_owned()));
    }
    session.invite_secret = body.invite_secret;
    session.invite_expires_at = body.invite_expires_at;
    session.failed_join_attempts = 0;
    session.join_locked_until = None;

    Ok(Json(OkResponse { ok: true }))
}

async fn approve_join(
    State(state): State<BrokerState>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<JoinDecisionRequest>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    let session_id = ShareSessionId(session_id);
    let mut guard = state.inner.lock().await;
    let session = guard
        .sessions
        .get_mut(&session_id)
        .ok_or((StatusCode::NOT_FOUND, "Session not found".to_owned()))?;
    if !constant_time_str_eq(&session.host_token, &body.host_token) {
        return Err((StatusCode::UNAUTHORIZED, "Invalid host token".to_owned()));
    }
    let guest = session
        .guests
        .get_mut(&body.guest_id)
        .ok_or((StatusCode::NOT_FOUND, "Guest not found".to_owned()))?;
    guest.connection_state = GuestConnectionState::Approved;
    guest.disconnected_at = None;
    guest.last_seen = Instant::now();
    if let Some(tx) = &guest.tx {
        send_json(
            tx,
            &BrokerControlMessage::JoinApproved {
                decision: JoinDecision {
                    guest_id: body.guest_id,
                    approved: true,
                },
            },
        );
    }
    broadcast_presence(session);
    Ok(Json(OkResponse { ok: true }))
}

async fn deny_join(
    State(state): State<BrokerState>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<JoinDecisionRequest>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    let session_id = ShareSessionId(session_id);
    let mut guard = state.inner.lock().await;
    let session = guard
        .sessions
        .get_mut(&session_id)
        .ok_or((StatusCode::NOT_FOUND, "Session not found".to_owned()))?;
    if !constant_time_str_eq(&session.host_token, &body.host_token) {
        return Err((StatusCode::UNAUTHORIZED, "Invalid host token".to_owned()));
    }
    let guest = session
        .guests
        .get_mut(&body.guest_id)
        .ok_or((StatusCode::NOT_FOUND, "Guest not found".to_owned()))?;
    guest.connection_state = GuestConnectionState::Denied;
    guest.disconnected_at = Some(Instant::now());
    if let Some(tx) = &guest.tx {
        send_json(
            tx,
            &BrokerControlMessage::JoinDenied {
                decision: JoinDecision {
                    guest_id: body.guest_id,
                    approved: false,
                },
            },
        );
    }
    broadcast_presence(session);
    Ok(Json(OkResponse { ok: true }))
}

async fn end_share_session(
    State(state): State<BrokerState>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<EndShareSessionRequest>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    let session_id = ShareSessionId(session_id);
    let ended = {
        let mut guard = state.inner.lock().await;
        let session = guard
            .sessions
            .get(&session_id)
            .ok_or((StatusCode::NOT_FOUND, "Session not found".to_owned()))?;
        if !constant_time_str_eq(&session.host_token, &body.host_token) {
            return Err((StatusCode::UNAUTHORIZED, "Invalid host token".to_owned()));
        }
        guard.sessions.remove(&session_id)
    };

    if let Some(session) = ended {
        notify_session_ended(&session);
    }
    Ok(Json(OkResponse { ok: true }))
}

async fn stream_session(
    State(state): State<BrokerState>,
    Path(session_id): Path<Uuid>,
    Query(query): Query<StreamQuery>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let session_id = ShareSessionId(session_id);
    let role = match query.role.as_str() {
        "host" => SessionRole::Host,
        "guest" => SessionRole::Guest,
        _ => return Err((StatusCode::BAD_REQUEST, "Invalid role".to_owned())),
    };

    let auth = {
        let mut guard = state.inner.lock().await;
        let session = guard
            .sessions
            .get_mut(&session_id)
            .ok_or((StatusCode::NOT_FOUND, "Session not found".to_owned()))?;
        match role {
            SessionRole::Host => {
                if !constant_time_str_eq(&session.host_token, &query.token) {
                    return Err((StatusCode::UNAUTHORIZED, "Invalid token".to_owned()));
                }
                StreamAuth::Host
            }
            SessionRole::Guest => {
                let (guest_id, _) = session
                    .guests
                    .iter()
                    .find(|(_, guest)| constant_time_str_eq(&guest.token, &query.token))
                    .ok_or((StatusCode::UNAUTHORIZED, "Invalid token".to_owned()))?;
                StreamAuth::Guest(*guest_id)
            }
        }
    };

    Ok(ws
        .max_message_size(MAX_WS_MESSAGE_BYTES)
        .max_frame_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(state, session_id, auth, socket)))
}

async fn handle_socket(
    state: BrokerState,
    session_id: ShareSessionId,
    auth: StreamAuth,
    socket: WebSocket,
) {
    let (tx, mut rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_CAPACITY);
    let connection_id = Uuid::new_v4();

    {
        let mut guard = state.inner.lock().await;
        let Some(session) = guard.sessions.get_mut(&session_id) else {
            return;
        };
        match auth {
            StreamAuth::Host => {
                let was_reconnected = session.host_disconnected_at.take().is_some();
                session.host_tx = Some(tx.clone());
                session.host_connection_id = Some(connection_id);
                session.host_last_seen = Instant::now();
                if was_reconnected {
                    notify_host_reconnected(session);
                }
            }
            StreamAuth::Guest(guest_id) => {
                let Some(guest) = session.guests.get_mut(&guest_id) else {
                    return;
                };
                guest.tx = Some(tx.clone());
                guest.connection_id = Some(connection_id);
                guest.last_seen = Instant::now();
                guest.disconnected_at = None;
                if matches!(
                    guest.connection_state,
                    GuestConnectionState::Approved | GuestConnectionState::Disconnected
                ) {
                    guest.connection_state = GuestConnectionState::Connected;
                }
            }
        }
        broadcast_presence(session);
    }

    send_json(
        &tx,
        &BrokerControlMessage::Connected {
            role: match auth {
                StreamAuth::Host => SessionRole::Host,
                StreamAuth::Guest(_) => SessionRole::Guest,
            },
            guest_id: match auth {
                StreamAuth::Host => None,
                StreamAuth::Guest(guest_id) => Some(guest_id),
            },
        },
    );

    {
        let guard = state.inner.lock().await;
        if let Some(session) = guard.sessions.get(&session_id) {
            if let StreamAuth::Guest(guest_id) = auth {
                if let Some(guest) = session.guests.get(&guest_id) {
                    if matches!(
                        guest.connection_state,
                        GuestConnectionState::Approved | GuestConnectionState::Connected
                    ) {
                        send_json(
                            &tx,
                            &BrokerControlMessage::JoinApproved {
                                decision: JoinDecision {
                                    guest_id,
                                    approved: true,
                                },
                            },
                        );
                    }
                }
            }
        }
    }

    let (mut sender, mut receiver) = socket.split();
    let mut send_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if sender.send(message).await.is_err() {
                break;
            }
        }
    });

    loop {
        let message = match tokio::time::timeout(CONNECTION_IDLE_TIMEOUT, receiver.next()).await {
            Ok(Some(Ok(message))) => message,
            // Read error or stream closed.
            Ok(_) => break,
            // No traffic (not even pings) for the whole idle window.
            Err(_) => break,
        };
        mark_connection_activity(&state, session_id, auth, connection_id).await;
        match message {
            Message::Binary(payload) => {
                relay_payload(&state, session_id, auth, Message::Binary(payload)).await;
            }
            Message::Text(payload) => {
                relay_payload(&state, session_id, auth, Message::Text(payload)).await;
            }
            Message::Close(_) => break,
            Message::Ping(payload) => {
                if tx.try_send(Message::Pong(payload)).is_err() {
                    break;
                }
            }
            Message::Pong(_) => {}
        }
    }

    handle_disconnect(&state, session_id, auth, connection_id).await;
    // With the stored sender clones cleared by the disconnect, dropping ours
    // closes the channel so the send task can flush what is queued and exit.
    drop(tx);
    if tokio::time::timeout(Duration::from_secs(5), &mut send_task)
        .await
        .is_err()
    {
        send_task.abort();
    }
}

async fn relay_payload(
    state: &BrokerState,
    session_id: ShareSessionId,
    auth: StreamAuth,
    payload: Message,
) {
    let guard = state.inner.lock().await;
    let Some(session) = guard.sessions.get(&session_id) else {
        return;
    };
    match auth {
        StreamAuth::Host => {
            for guest in session.guests.values() {
                if matches!(
                    guest.connection_state,
                    GuestConnectionState::Approved | GuestConnectionState::Connected
                ) {
                    if let Some(tx) = &guest.tx {
                        let _ = tx.try_send(payload.clone());
                    }
                }
            }
        }
        StreamAuth::Guest(guest_id) => {
            let Some(guest) = session.guests.get(&guest_id) else {
                return;
            };
            if !matches!(
                guest.connection_state,
                GuestConnectionState::Approved | GuestConnectionState::Connected
            ) {
                return;
            }
            if let Some(host_tx) = &session.host_tx {
                let _ = host_tx.try_send(payload);
            }
        }
    }
}

async fn mark_connection_activity(
    state: &BrokerState,
    session_id: ShareSessionId,
    auth: StreamAuth,
    connection_id: Uuid,
) {
    let mut guard = state.inner.lock().await;
    let Some(session) = guard.sessions.get_mut(&session_id) else {
        return;
    };
    match auth {
        StreamAuth::Host => {
            if session.host_connection_id == Some(connection_id) {
                session.host_last_seen = Instant::now();
            }
        }
        StreamAuth::Guest(guest_id) => {
            if let Some(guest) = session.guests.get_mut(&guest_id) {
                if guest.connection_id == Some(connection_id) {
                    guest.last_seen = Instant::now();
                }
            }
        }
    }
}

async fn handle_disconnect(
    state: &BrokerState,
    session_id: ShareSessionId,
    auth: StreamAuth,
    connection_id: Uuid,
) {
    let mut guard = state.inner.lock().await;
    match auth {
        StreamAuth::Host => {
            if let Some(session) = guard.sessions.get_mut(&session_id) {
                if session.host_connection_id != Some(connection_id) {
                    return;
                }
                session.host_tx = None;
                session.host_connection_id = None;
                session.host_disconnected_at = Some(Instant::now());
                notify_host_disconnected(session);
            }
        }
        StreamAuth::Guest(guest_id) => {
            if let Some(session) = guard.sessions.get_mut(&session_id) {
                if let Some(guest) = session.guests.get_mut(&guest_id) {
                    if guest.connection_id != Some(connection_id) {
                        return;
                    }
                    guest.tx = None;
                    guest.connection_id = None;
                    guest.disconnected_at = Some(Instant::now());
                    if !matches!(guest.connection_state, GuestConnectionState::Denied) {
                        guest.connection_state = GuestConnectionState::Disconnected;
                    }
                }
                broadcast_presence(session);
            }
        }
    }
}

fn broadcast_presence(session: &SessionRecord) {
    let guests = session
        .guests
        .iter()
        .map(|(guest_id, guest)| GuestPresence {
            id: *guest_id,
            display_name: guest.display_name.clone(),
            joined_at: guest.joined_at,
            connection_state: guest.connection_state,
        })
        .collect::<Vec<_>>();
    let message = BrokerControlMessage::Presence { guests };
    if let Some(host_tx) = &session.host_tx {
        send_json(host_tx, &message);
    }
    for guest in session.guests.values() {
        if let Some(tx) = &guest.tx {
            send_json(tx, &message);
        }
    }
}

fn notify_host_disconnected(session: &SessionRecord) {
    for guest in session.guests.values() {
        if let Some(tx) = &guest.tx {
            send_json(tx, &BrokerControlMessage::HostDisconnected);
        }
    }
}

fn notify_host_reconnected(session: &SessionRecord) {
    for guest in session.guests.values() {
        if let Some(tx) = &guest.tx {
            send_json(tx, &BrokerControlMessage::HostReconnected);
        }
    }
}

fn notify_session_ended(session: &SessionRecord) {
    for guest in session.guests.values() {
        if let Some(tx) = &guest.tx {
            send_json(tx, &BrokerControlMessage::SessionEnded);
        }
    }
}

fn send_json<T: Serialize>(tx: &mpsc::Sender<Message>, value: &T) {
    if let Ok(text) = serde_json::to_string(value) {
        let _ = tx.try_send(Message::Text(text));
    }
}

fn active_guest_count(session: &SessionRecord) -> usize {
    session
        .guests
        .values()
        .filter(|guest| !matches!(guest.connection_state, GuestConnectionState::Denied))
        .count()
}

fn register_failed_join_attempt(session: &mut SessionRecord, now: Instant) {
    session.failed_join_attempts = session.failed_join_attempts.saturating_add(1);
    let shift = session.failed_join_attempts.saturating_sub(1).min(5);
    let delay_secs = (1u64 << shift).min(JOIN_BACKOFF_MAX_SECS);
    session.join_locked_until = Some(now + Duration::from_secs(delay_secs));
}

async fn cleanup_expired_sessions(state: &BrokerState, now: Instant) {
    let mut ended_sessions = Vec::new();

    {
        let mut guard = state.inner.lock().await;
        let session_ids = guard.sessions.keys().copied().collect::<Vec<_>>();
        for session_id in session_ids {
            let Some(session) = guard.sessions.get_mut(&session_id) else {
                continue;
            };

            if session.host_tx.is_some()
                && now.saturating_duration_since(session.host_last_seen) > HEARTBEAT_TIMEOUT
            {
                session.host_tx = None;
                session.host_connection_id = None;
                if session.host_disconnected_at.is_none() {
                    session.host_disconnected_at = Some(now);
                    notify_host_disconnected(session);
                }
            }

            let mut guest_presence_changed = false;
            let guest_ids = session.guests.keys().copied().collect::<Vec<_>>();
            for guest_id in guest_ids {
                let Some(guest) = session.guests.get_mut(&guest_id) else {
                    continue;
                };

                if guest.tx.is_some()
                    && now.saturating_duration_since(guest.last_seen) > HEARTBEAT_TIMEOUT
                {
                    guest.tx = None;
                    guest.connection_id = None;
                    guest.disconnected_at = Some(now);
                    if !matches!(guest.connection_state, GuestConnectionState::Denied) {
                        guest.connection_state = GuestConnectionState::Disconnected;
                    }
                    guest_presence_changed = true;
                }
            }

            if guest_presence_changed {
                broadcast_presence(session);
            }

            session.guests.retain(|_, guest| {
                if matches!(guest.connection_state, GuestConnectionState::Denied) {
                    return guest
                        .disconnected_at
                        .map(|at| now.saturating_duration_since(at) <= DENIED_GUEST_TTL)
                        .unwrap_or(false);
                }
                if matches!(guest.connection_state, GuestConnectionState::Pending)
                    && guest.tx.is_none()
                {
                    let age = now.saturating_duration_since(guest.last_seen);
                    return age <= PENDING_JOIN_TTL;
                }
                if matches!(guest.connection_state, GuestConnectionState::Disconnected)
                    && guest.tx.is_none()
                {
                    return guest
                        .disconnected_at
                        .map(|at| now.saturating_duration_since(at) <= GUEST_RECONNECT_GRACE)
                        .unwrap_or(false);
                }
                true
            });

            if session
                .host_disconnected_at
                .map(|at| now.saturating_duration_since(at) > HOST_RECONNECT_GRACE)
                .unwrap_or(false)
            {
                if let Some(ended) = guard.sessions.remove(&session_id) {
                    ended_sessions.push(ended);
                }
            }
        }
    }

    for session in ended_sessions {
        notify_session_ended(&session);
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD_NO_PAD, bytes)
}

const MAX_DISPLAY_NAME_LEN: usize = 64;
const MAX_DEVICE_ID_LEN: usize = 128;
const MAX_INVITE_SECRET_LEN: usize = 128;
const MAX_PASSPHRASE_LEN: usize = 1024;

fn sanitize_join_request(
    mut body: JoinShareSessionRequest,
) -> Result<JoinShareSessionRequest, &'static str> {
    body.display_name = body.display_name.trim().to_owned();
    if body.display_name.is_empty() {
        return Err("Display name is required");
    }
    if body.display_name.len() > MAX_DISPLAY_NAME_LEN {
        return Err("Display name too long");
    }
    if body
        .display_name
        .chars()
        .any(|c| c.is_control() || c == '\u{0000}')
    {
        return Err("Display name contains invalid characters");
    }
    if body.device_id.len() > MAX_DEVICE_ID_LEN {
        return Err("Device id too long");
    }
    if body.invite_secret.len() > MAX_INVITE_SECRET_LEN {
        return Err("Invite secret too long");
    }
    if let Some(passphrase) = &body.passphrase {
        if passphrase.len() > MAX_PASSPHRASE_LEN {
            return Err("Passphrase too long");
        }
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use serde::de::DeserializeOwned;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    fn open_state() -> BrokerState {
        BrokerState::new(BrokerConfig {
            require_loopback_session_creation: false,
        })
    }

    fn test_app() -> Router {
        build_router(open_state())
    }

    async fn decode_json<T: DeserializeOwned>(response: axum::response::Response) -> T {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        serde_json::from_slice(&bytes).expect("decode response body")
    }

    fn create_session_request() -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/share-sessions")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&CreateShareSessionRequest {
                    session_secret: "secret".to_owned(),
                    invite_secret: "invite-secret".to_owned(),
                    invite_expires_at: None,
                    passphrase_hash: None,
                    trusted_devices: Vec::new(),
                })
                .expect("serialize create body"),
            ))
            .expect("create request")
    }

    async fn create_session(app: &Router) -> CreateShareSessionResponse {
        let response = app
            .clone()
            .oneshot(create_session_request())
            .await
            .expect("create session response");
        assert_eq!(response.status(), StatusCode::OK);
        decode_json(response).await
    }

    #[tokio::test]
    async fn create_session_returns_session_id_and_host_token() {
        let app = test_app();
        let response = create_session(&app).await;
        assert_ne!(response.session_id.0, Uuid::nil());
        assert!(!response.host_token.is_empty());
    }

    #[tokio::test]
    async fn create_session_fails_closed_without_peer_info_when_loopback_required() {
        let app = build_router(BrokerState::new(BrokerConfig {
            require_loopback_session_creation: true,
        }));
        let response = app
            .oneshot(create_session_request())
            .await
            .expect("create session response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn join_session_returns_guest_credentials() {
        let app = test_app();
        let session = create_session(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/share-sessions/{}/join", session.session_id.0))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&JoinShareSessionRequest {
                            display_name: "Mauro".to_owned(),
                            invite_secret: "invite-secret".to_owned(),
                            device_id: "device-1".to_owned(),
                            passphrase: None,
                        })
                        .expect("serialize join body"),
                    ))
                    .expect("join request"),
            )
            .await
            .expect("join response");
        assert_eq!(response.status(), StatusCode::OK);
        let join: JoinShareSessionResponse = decode_json(response).await;
        assert_ne!(join.guest_id.0, Uuid::nil());
        assert!(!join.guest_token.is_empty());
    }

    #[tokio::test]
    async fn join_session_rejects_invalid_invite_secret() {
        let app = test_app();
        let session = create_session(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/share-sessions/{}/join", session.session_id.0))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&JoinShareSessionRequest {
                            display_name: "Mauro".to_owned(),
                            invite_secret: "wrong-secret".to_owned(),
                            device_id: "device-1".to_owned(),
                            passphrase: None,
                        })
                        .expect("serialize join body"),
                    ))
                    .expect("join request"),
            )
            .await
            .expect("join response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn join_session_enforces_participant_limit() {
        let app = test_app();
        let session = create_session(&app).await;
        for idx in 0..3 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/v1/share-sessions/{}/join", session.session_id.0))
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::to_vec(&JoinShareSessionRequest {
                                display_name: format!("Guest {idx}"),
                                invite_secret: "invite-secret".to_owned(),
                                device_id: format!("device-{idx}"),
                                passphrase: None,
                            })
                            .expect("serialize join body"),
                        ))
                        .expect("join request"),
                )
                .await
                .expect("join response");
            assert_eq!(response.status(), StatusCode::OK);
        }

        let overflow = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/share-sessions/{}/join", session.session_id.0))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&JoinShareSessionRequest {
                            display_name: "Overflow".to_owned(),
                            invite_secret: "invite-secret".to_owned(),
                            device_id: "device-overflow".to_owned(),
                            passphrase: None,
                        })
                        .expect("serialize join body"),
                    ))
                    .expect("overflow request"),
            )
            .await
            .expect("overflow response");
        assert_eq!(overflow.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn approve_join_rejects_invalid_host_token() {
        let app = test_app();
        let session = create_session(&app).await;
        let join_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/share-sessions/{}/join", session.session_id.0))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&JoinShareSessionRequest {
                            display_name: "Guest".to_owned(),
                            invite_secret: "invite-secret".to_owned(),
                            device_id: "device-1".to_owned(),
                            passphrase: None,
                        })
                        .expect("serialize join body"),
                    ))
                    .expect("join request"),
            )
            .await
            .expect("join response");
        assert_eq!(join_response.status(), StatusCode::OK);
        let join: JoinShareSessionResponse = decode_json(join_response).await;

        let approve_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/share-sessions/{}/approve",
                        session.session_id.0
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&JoinDecisionRequest {
                            host_token: "bad-token".to_owned(),
                            guest_id: join.guest_id,
                        })
                        .expect("serialize approve body"),
                    ))
                    .expect("approve request"),
            )
            .await
            .expect("approve response");
        assert_eq!(approve_response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn end_session_requires_valid_host_token() {
        let app = test_app();
        let session = create_session(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/share-sessions/{}/end", session.session_id.0))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&EndShareSessionRequest {
                            host_token: "bad-token".to_owned(),
                        })
                        .expect("serialize end body"),
                    ))
                    .expect("end request"),
            )
            .await
            .expect("end response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn end_session_removes_session_immediately() {
        let app = test_app();
        let session = create_session(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/share-sessions/{}/end", session.session_id.0))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&EndShareSessionRequest {
                            host_token: session.host_token.clone(),
                        })
                        .expect("serialize end body"),
                    ))
                    .expect("end request"),
            )
            .await
            .expect("end response");
        assert_eq!(response.status(), StatusCode::OK);

        let join_again = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/share-sessions/{}/join", session.session_id.0))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&JoinShareSessionRequest {
                            display_name: "Guest".to_owned(),
                            invite_secret: "invite-secret".to_owned(),
                            device_id: "device-1".to_owned(),
                            passphrase: None,
                        })
                        .expect("serialize join body"),
                    ))
                    .expect("join request"),
            )
            .await
            .expect("join response");
        assert_eq!(join_again.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn host_disconnect_keeps_session_until_grace_period_expires() {
        let session_id = ShareSessionId(Uuid::new_v4());
        let host_connection_id = Uuid::new_v4();
        let guest_id = GuestId(Uuid::new_v4());
        let (_host_tx, _host_rx) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
        let (guest_tx, mut guest_rx) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
        let state = open_state();
        state.inner.lock().await.sessions.insert(
            session_id,
            SessionRecord {
                session_secret: "secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: None,
                passphrase_hash: None,
                host_token: "host-token".to_owned(),
                host_tx: Some(_host_tx),
                host_connection_id: Some(host_connection_id),
                host_last_seen: Instant::now(),
                host_disconnected_at: None,
                failed_join_attempts: 0,
                join_locked_until: None,
                trusted_devices: HashMap::new(),
                guests: HashMap::from([(
                    guest_id,
                    GuestRecord {
                        token: "guest-token".to_owned(),
                        display_name: "Guest".to_owned(),
                        device_id: "device-1".to_owned(),
                        joined_at: Utc::now(),
                        connection_state: GuestConnectionState::Connected,
                        tx: Some(guest_tx),
                        connection_id: Some(Uuid::new_v4()),
                        last_seen: Instant::now(),
                        disconnected_at: None,
                    },
                )]),
            },
        );

        handle_disconnect(&state, session_id, StreamAuth::Host, host_connection_id).await;

        let host_disconnected = guest_rx.recv().await.expect("host disconnected message");
        let host_disconnected = match host_disconnected {
            Message::Text(text) => {
                serde_json::from_str::<BrokerControlMessage>(&text).expect("decode broker message")
            }
            other => panic!("unexpected broker message: {other:?}"),
        };
        assert!(matches!(
            host_disconnected,
            BrokerControlMessage::HostDisconnected
        ));

        {
            let guard = state.inner.lock().await;
            assert!(guard.sessions.contains_key(&session_id));
        }

        cleanup_expired_sessions(
            &state,
            Instant::now() + HOST_RECONNECT_GRACE + Duration::from_secs(1),
        )
        .await;

        let session_ended = guest_rx.recv().await.expect("session ended message");
        let session_ended = match session_ended {
            Message::Text(text) => {
                serde_json::from_str::<BrokerControlMessage>(&text).expect("decode broker message")
            }
            other => panic!("unexpected broker message: {other:?}"),
        };
        assert!(matches!(session_ended, BrokerControlMessage::SessionEnded));

        let guard = state.inner.lock().await;
        assert!(!guard.sessions.contains_key(&session_id));
    }

    #[tokio::test]
    async fn stale_disconnect_does_not_clear_newer_host_connection() {
        let session_id = ShareSessionId(Uuid::new_v4());
        let stale_connection_id = Uuid::new_v4();
        let current_connection_id = Uuid::new_v4();
        let (host_tx, _host_rx) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
        let state = open_state();
        state.inner.lock().await.sessions.insert(
            session_id,
            SessionRecord {
                session_secret: "secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: None,
                passphrase_hash: None,
                host_token: "host-token".to_owned(),
                host_tx: Some(host_tx),
                host_connection_id: Some(current_connection_id),
                host_last_seen: Instant::now(),
                host_disconnected_at: None,
                failed_join_attempts: 0,
                join_locked_until: None,
                trusted_devices: HashMap::new(),
                guests: HashMap::new(),
            },
        );

        handle_disconnect(&state, session_id, StreamAuth::Host, stale_connection_id).await;

        let guard = state.inner.lock().await;
        let session = guard.sessions.get(&session_id).expect("session exists");
        assert_eq!(session.host_connection_id, Some(current_connection_id));
        assert!(session.host_tx.is_some());
        assert!(session.host_disconnected_at.is_none());
    }
}
