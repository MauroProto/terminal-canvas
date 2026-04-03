use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::thread;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_server::from_tcp_rustls;
use axum_server::tls_rustls::RustlsConfig;
use axum_server::Handle;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

use super::auth::verify_passphrase;
use super::models::{
    GuestConnectionState, GuestId, GuestPresence, JoinDecision, JoinRequest, SessionRole,
    ShareSessionId, TrustedDevice,
};
use super::protocol::{
    BrokerControlMessage, CreateShareSessionRequest, CreateShareSessionResponse,
    EndShareSessionRequest, JoinDecisionRequest, JoinShareSessionRequest, JoinShareSessionResponse,
    RotateInviteRequest,
};
use super::tls::ensure_crypto_provider;

const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);
const HOST_RECONNECT_GRACE: Duration = Duration::from_secs(20);
const GUEST_RECONNECT_GRACE: Duration = Duration::from_secs(45);
const PENDING_JOIN_TTL: Duration = Duration::from_secs(120);
const DENIED_GUEST_TTL: Duration = Duration::from_secs(30);
const JOIN_BACKOFF_MAX_SECS: u64 = 30;

#[derive(Clone)]
struct AppState {
    inner: std::sync::Arc<Mutex<BrokerState>>,
}

#[derive(Default)]
struct BrokerState {
    sessions: HashMap<ShareSessionId, SessionRecord>,
}

struct SessionRecord {
    session_secret: String,
    invite_secret: String,
    invite_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    passphrase_hash: Option<String>,
    host_token: String,
    host_tx: Option<mpsc::UnboundedSender<Message>>,
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
    device_id: String,
    joined_at: chrono::DateTime<chrono::Utc>,
    connection_state: GuestConnectionState,
    tx: Option<mpsc::UnboundedSender<Message>>,
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

pub struct EmbeddedCollabServer {
    local_api_url: String,
    handle: Handle,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl EmbeddedCollabServer {
    pub fn start(bind_addr: SocketAddr, cert_pem: String, key_pem: String) -> anyhow::Result<Self> {
        ensure_crypto_provider();
        let listener = TcpListener::bind(bind_addr)?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;

        let state = AppState {
            inner: std::sync::Arc::new(Mutex::new(BrokerState::default())),
        };
        let router = build_router(state.clone());
        let handle = Handle::new();
        let handle_for_thread = handle.clone();

        let thread_handle = thread::spawn(move || {
            let runtime = Runtime::new().expect("collab runtime");
            runtime.block_on(async move {
                spawn_cleanup_task(state);
                let tls_config =
                    RustlsConfig::from_pem(cert_pem.into_bytes(), key_pem.into_bytes())
                        .await
                        .expect("build rustls config for embedded collab server");
                if let Err(err) = from_tcp_rustls(listener, tls_config)
                    .handle(handle_for_thread)
                    .serve(router.into_make_service())
                    .await
                {
                    log::error!("embedded collab server stopped with error: {err}");
                }
            });
        });

        Ok(Self {
            local_api_url: format!("https://127.0.0.1:{}", local_addr.port()),
            handle,
            thread_handle: Some(thread_handle),
        })
    }

    pub fn local_api_url(&self) -> &str {
        &self.local_api_url
    }

    pub fn stop(&mut self) -> anyhow::Result<()> {
        self.handle.graceful_shutdown(Some(Duration::from_secs(1)));
        if let Some(thread_handle) = self.thread_handle.take() {
            let _ = thread_handle.join();
        }
        Ok(())
    }
}

impl Drop for EmbeddedCollabServer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn build_router(state: AppState) -> Router {
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

async fn create_share_session(
    State(state): State<AppState>,
    Json(body): Json<CreateShareSessionRequest>,
) -> Result<Json<CreateShareSessionResponse>, (StatusCode, String)> {
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
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<JoinShareSessionRequest>,
) -> Result<Json<JoinShareSessionResponse>, (StatusCode, String)> {
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
    if session.invite_secret != body.invite_secret {
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
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<RotateInviteRequest>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    let session_id = ShareSessionId(session_id);
    let mut guard = state.inner.lock().await;
    let session = guard
        .sessions
        .get_mut(&session_id)
        .ok_or((StatusCode::NOT_FOUND, "Session not found".to_owned()))?;
    if session.host_token != body.host_token {
        return Err((StatusCode::UNAUTHORIZED, "Invalid host token".to_owned()));
    }
    session.invite_secret = body.invite_secret;
    session.invite_expires_at = body.invite_expires_at;
    session.failed_join_attempts = 0;
    session.join_locked_until = None;

    Ok(Json(OkResponse { ok: true }))
}

async fn approve_join(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<JoinDecisionRequest>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    let session_id = ShareSessionId(session_id);
    let mut guard = state.inner.lock().await;
    let session = guard
        .sessions
        .get_mut(&session_id)
        .ok_or((StatusCode::NOT_FOUND, "Session not found".to_owned()))?;
    if session.host_token != body.host_token {
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
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<JoinDecisionRequest>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    let session_id = ShareSessionId(session_id);
    let mut guard = state.inner.lock().await;
    let session = guard
        .sessions
        .get_mut(&session_id)
        .ok_or((StatusCode::NOT_FOUND, "Session not found".to_owned()))?;
    if session.host_token != body.host_token {
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
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<EndShareSessionRequest>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    let session_id = ShareSessionId(session_id);
    let session = {
        let mut guard = state.inner.lock().await;
        let session = guard
            .sessions
            .get(&session_id)
            .ok_or((StatusCode::NOT_FOUND, "Session not found".to_owned()))?;
        if session.host_token != body.host_token {
            return Err((StatusCode::UNAUTHORIZED, "Invalid host token".to_owned()));
        }
        guard.sessions.remove(&session_id).expect("session exists")
    };

    notify_session_ended(&session);
    Ok(Json(OkResponse { ok: true }))
}

async fn stream_session(
    State(state): State<AppState>,
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
                if session.host_token != query.token {
                    return Err((StatusCode::UNAUTHORIZED, "Invalid token".to_owned()));
                }
                StreamAuth::Host
            }
            SessionRole::Guest => {
                let (guest_id, _) = session
                    .guests
                    .iter()
                    .find(|(_, guest)| guest.token == query.token)
                    .ok_or((StatusCode::UNAUTHORIZED, "Invalid token".to_owned()))?;
                StreamAuth::Guest(*guest_id)
            }
        }
    };

    Ok(ws.on_upgrade(move |socket| handle_socket(state, session_id, auth, socket)))
}

async fn handle_socket(
    state: AppState,
    session_id: ShareSessionId,
    auth: StreamAuth,
    socket: WebSocket,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
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
    let send_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if sender.send(message).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(message)) = receiver.next().await {
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
                if tx.send(Message::Pong(payload)).is_err() {
                    break;
                }
            }
            Message::Pong(_) => {}
        }
    }

    send_task.abort();
    handle_disconnect(&state, session_id, auth, connection_id).await;
}

async fn relay_payload(
    state: &AppState,
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
                        let _ = tx.send(payload.clone());
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
                let _ = host_tx.send(payload);
            }
        }
    }
}

async fn mark_connection_activity(
    state: &AppState,
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
    state: &AppState,
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

fn send_json<T: Serialize>(tx: &mpsc::UnboundedSender<Message>, value: &T) {
    if let Ok(text) = serde_json::to_string(value) {
        let _ = tx.send(Message::Text(text.into()));
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

fn spawn_cleanup_task(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        loop {
            ticker.tick().await;
            cleanup_expired_sessions(&state, Instant::now()).await;
        }
    });
}

async fn cleanup_expired_sessions(state: &AppState, now: Instant) {
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
                let ended = guard.sessions.remove(&session_id).expect("session exists");
                ended_sessions.push(ended);
            }
        }
    }

    for session in ended_sessions {
        notify_session_ended(&session);
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD_NO_PAD, bytes)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::time::Duration;

    use crate::collab::auth::hash_passphrase;
    use crate::collab::protocol::{
        CreateShareSessionRequest, CreateShareSessionResponse, EndShareSessionRequest,
        JoinShareSessionRequest, JoinShareSessionResponse, RotateInviteRequest,
    };
    use crate::collab::tls::{generate_tls_material, http_client};
    use crate::collab::transport::json_post;
    use chrono::Utc;

    use super::EmbeddedCollabServer;

    fn start_test_server() -> (EmbeddedCollabServer, String) {
        let tls_material =
            generate_tls_material(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])
                .expect("generate tls material");
        let cert_pem = tls_material.cert_pem.clone();
        let server = EmbeddedCollabServer::start(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            tls_material.cert_pem,
            tls_material.key_pem,
        )
        .expect("start embedded collab server");
        (server, cert_pem)
    }

    fn join_status(
        server: &EmbeddedCollabServer,
        cert_pem: &str,
        session_id: uuid::Uuid,
        body: &JoinShareSessionRequest,
    ) -> reqwest::StatusCode {
        http_client(Some(cert_pem))
            .expect("http client")
            .post(format!(
                "{}/v1/share-sessions/{}/join",
                server.local_api_url(),
                session_id
            ))
            .json(body)
            .send()
            .expect("join response")
            .status()
    }

    #[test]
    fn embedded_server_serves_create_join_and_end_session() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: None,
                passphrase_hash: None,
                trusted_devices: Vec::new(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");
        let join: JoinShareSessionResponse = json_post(
            &format!(
                "{}/v1/share-sessions/{}/join",
                server.local_api_url(),
                create.session_id.0
            ),
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
            Some(cert_pem.as_str()),
        )
        .expect("join session");
        assert_ne!(join.guest_id.0, uuid::Uuid::nil());
        assert!(!join.auto_approved);

        let end_response: serde_json::Value = json_post(
            &format!(
                "{}/v1/share-sessions/{}/end",
                server.local_api_url(),
                create.session_id.0
            ),
            &EndShareSessionRequest {
                host_token: create.host_token.clone(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("end session");
        assert_eq!(end_response["ok"], true);

        server.stop().expect("stop server");
    }

    #[test]
    fn embedded_server_rejects_join_with_invalid_invite_secret() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: None,
                passphrase_hash: None,
                trusted_devices: Vec::new(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");
        let join_result = json_post::<_, JoinShareSessionResponse>(
            &format!(
                "{}/v1/share-sessions/{}/join",
                server.local_api_url(),
                create.session_id.0
            ),
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "wrong-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
            Some(cert_pem.as_str()),
        );
        assert!(join_result.is_err());
        server.stop().expect("stop server");
    }

    #[test]
    fn embedded_server_requires_matching_session_passphrase_when_configured() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: None,
                passphrase_hash: Some(hash_passphrase("clave-super-segura").expect("hash")),
                trusted_devices: Vec::new(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");

        let missing = join_status(
            &server,
            cert_pem.as_str(),
            create.session_id.0,
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
        );
        assert_eq!(missing, reqwest::StatusCode::UNAUTHORIZED);

        std::thread::sleep(Duration::from_millis(1100));

        let joined: JoinShareSessionResponse = json_post(
            &format!(
                "{}/v1/share-sessions/{}/join",
                server.local_api_url(),
                create.session_id.0
            ),
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: Some("clave-super-segura".to_owned()),
            },
            Some(cert_pem.as_str()),
        )
        .expect("join with passphrase");
        assert_ne!(joined.guest_id.0, uuid::Uuid::nil());
        server.stop().expect("stop server");
    }

    #[test]
    fn embedded_server_rate_limits_repeated_failed_joins() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: None,
                passphrase_hash: Some(hash_passphrase("clave-super-segura").expect("hash")),
                trusted_devices: Vec::new(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");

        let first = join_status(
            &server,
            cert_pem.as_str(),
            create.session_id.0,
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: Some("incorrecta".to_owned()),
            },
        );
        assert_eq!(first, reqwest::StatusCode::UNAUTHORIZED);

        let second = join_status(
            &server,
            cert_pem.as_str(),
            create.session_id.0,
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: Some("incorrecta".to_owned()),
            },
        );
        assert_eq!(second, reqwest::StatusCode::TOO_MANY_REQUESTS);
        server.stop().expect("stop server");
    }

    #[test]
    fn embedded_server_rejects_expired_invites() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "session-secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: Some(Utc::now() - chrono::Duration::minutes(1)),
                passphrase_hash: None,
                trusted_devices: Vec::new(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");

        let status = join_status(
            &server,
            cert_pem.as_str(),
            create.session_id.0,
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
        );
        assert_eq!(status, reqwest::StatusCode::GONE);
        server.stop().expect("stop server");
    }

    #[test]
    fn embedded_server_rotates_invite_without_breaking_session() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "session-secret".to_owned(),
                invite_secret: "invite-secret-1".to_owned(),
                invite_expires_at: None,
                passphrase_hash: None,
                trusted_devices: Vec::new(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");

        let _: serde_json::Value = json_post(
            &format!(
                "{}/v1/share-sessions/{}/rotate-invite",
                server.local_api_url(),
                create.session_id.0
            ),
            &RotateInviteRequest {
                host_token: create.host_token.clone(),
                invite_secret: "invite-secret-2".to_owned(),
                invite_expires_at: Some(Utc::now() + chrono::Duration::hours(24)),
            },
            Some(cert_pem.as_str()),
        )
        .expect("rotate invite");

        let old_status = join_status(
            &server,
            cert_pem.as_str(),
            create.session_id.0,
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret-1".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
        );
        assert_eq!(old_status, reqwest::StatusCode::UNAUTHORIZED);

        let joined: JoinShareSessionResponse = json_post(
            &format!(
                "{}/v1/share-sessions/{}/join",
                server.local_api_url(),
                create.session_id.0
            ),
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret-2".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
            Some(cert_pem.as_str()),
        )
        .expect("join with rotated invite");
        assert_ne!(joined.guest_id.0, uuid::Uuid::nil());
        server.stop().expect("stop server");
    }

    #[test]
    fn embedded_server_auto_approves_trusted_devices() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "session-secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: None,
                passphrase_hash: None,
                trusted_devices: vec![crate::collab::TrustedDevice {
                    device_id: "device-1".to_owned(),
                    last_display_name: "Mauro".to_owned(),
                    approved_at: Utc::now(),
                    last_seen_at: Utc::now(),
                }],
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");

        let joined: JoinShareSessionResponse = json_post(
            &format!(
                "{}/v1/share-sessions/{}/join",
                server.local_api_url(),
                create.session_id.0
            ),
            &JoinShareSessionRequest {
                display_name: "Mauro".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
            Some(cert_pem.as_str()),
        )
        .expect("join trusted device");
        assert!(joined.auto_approved);
        server.stop().expect("stop server");
    }
}
