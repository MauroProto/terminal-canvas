use super::*;

async fn fixture() -> (BrokerState, ShareSessionId, GuestId, String) {
    let state = BrokerState::new(BrokerConfig {
        require_loopback_session_creation: false,
    });
    let Json(created) = create_share_session(
        None,
        State(state.clone()),
        Json(CreateShareSessionRequest {
            invite_secret: "fixture-invite".to_owned(),
            invite_expires_at: None,
            passphrase_hash: None,
            trusted_devices: Vec::new(),
        }),
    )
    .await
    .unwrap();
    let Json(joined) = join_share_session(
        State(state.clone()),
        Path(created.session_id.0),
        Json(JoinShareSessionRequest {
            display_name: "Pending fixture".to_owned(),
            invite_secret: "fixture-invite".to_owned(),
            device_id: "fixture-device".to_owned(),
            passphrase: None,
        }),
    )
    .await
    .unwrap();
    assert!(!joined.auto_approved);
    (state, created.session_id, joined.guest_id, created.host_token)
}

async fn reconnect(
    state: &BrokerState,
    session_id: ShareSessionId,
    guest_id: GuestId,
) -> (Uuid, mpsc::Receiver<Message>) {
    let (tx, rx) = mpsc::channel(8);
    let id = Uuid::new_v4();
    let mut guard = state.inner.lock().await;
    let guest = guard.sessions.get_mut(&session_id).unwrap().guests.get_mut(&guest_id).unwrap();
    assert!(guest.connect(tx, id, Instant::now()));
    (id, rx)
}

#[tokio::test]
async fn security_pending_guest_cannot_gain_data_access_by_reconnecting() {
    let (state, session_id, guest_id, _) = fixture().await;
    let (first_id, _first_rx) = reconnect(&state, session_id, guest_id).await;
    handle_disconnect(&state, session_id, StreamAuth::Guest(guest_id), first_id).await;
    let (_, mut guest_rx) = reconnect(&state, session_id, guest_id).await;
    let (host_tx, mut host_rx) = mpsc::channel(8);
    {
        let mut guard = state.inner.lock().await;
        let session = guard.sessions.get_mut(&session_id).unwrap();
        session.host_tx = Some(host_tx);
        let guest = session.guests.get(&guest_id).unwrap();
        assert_eq!(guest.connection_state, GuestConnectionState::Pending);
        assert!(!guest.approved);
        assert!(!guest.may_relay());
    }
    relay_payload(&state, session_id, StreamAuth::Host, Message::Binary(b"private-fixture".to_vec())).await;
    relay_payload(&state, session_id, StreamAuth::Guest(guest_id), Message::Binary(b"unapproved-input".to_vec())).await;
    assert!(guest_rx.try_recv().is_err(), "pending guest received shared data");
    assert!(host_rx.try_recv().is_err(), "pending guest relayed input");
}

#[tokio::test]
async fn security_heartbeat_disconnect_preserves_pending_authorization() {
    let (state, session_id, guest_id, _) = fixture().await;
    let (_, _rx) = reconnect(&state, session_id, guest_id).await;
    let now = Instant::now();
    {
        let mut guard = state.inner.lock().await;
        let session = guard.sessions.get_mut(&session_id).unwrap();
        session.host_disconnected_at = None;
        session.guests.get_mut(&guest_id).unwrap().last_seen = now - HEARTBEAT_TIMEOUT - Duration::from_secs(1);
    }
    cleanup_expired_sessions(&state, now).await;
    let (_, _rx) = reconnect(&state, session_id, guest_id).await;
    let guard = state.inner.lock().await;
    let guest = guard.sessions[&session_id].guests.get(&guest_id).unwrap();
    assert_eq!(guest.connection_state, GuestConnectionState::Pending);
    assert!(!guest.may_relay());
}

#[tokio::test]
async fn security_approved_reconnect_works_but_revocation_is_terminal() {
    let (state, session_id, guest_id, host_token) = fixture().await;
    approve_join(State(state.clone()), Path(session_id.0), Json(JoinDecisionRequest { host_token: host_token.clone(), guest_id })).await.unwrap();
    let (id, _rx) = reconnect(&state, session_id, guest_id).await;
    handle_disconnect(&state, session_id, StreamAuth::Guest(guest_id), id).await;
    let (_, mut rx) = reconnect(&state, session_id, guest_id).await;
    relay_payload(&state, session_id, StreamAuth::Host, Message::Binary(b"allowed-fixture".to_vec())).await;
    assert_eq!(rx.try_recv().unwrap(), Message::Binary(b"allowed-fixture".to_vec()));
    deny_join(State(state.clone()), Path(session_id.0), Json(JoinDecisionRequest { host_token: host_token.clone(), guest_id })).await.unwrap();
    let (tx, _rx) = mpsc::channel(8);
    {
        let mut guard = state.inner.lock().await;
        let guest = guard.sessions.get_mut(&session_id).unwrap().guests.get_mut(&guest_id).unwrap();
        guest.disconnect(Instant::now());
        assert!(!guest.connect(tx, Uuid::new_v4(), Instant::now()));
        assert!(!guest.may_relay());
    }
    assert!(approve_join(State(state.clone()), Path(session_id.0), Json(JoinDecisionRequest { host_token, guest_id })).await.is_err());
}
