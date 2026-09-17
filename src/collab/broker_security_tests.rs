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

#[tokio::test]
async fn security_session_creation_rejects_unbounded_phc_policy() {
    let state = BrokerState::new(BrokerConfig { require_loopback_session_creation: false });
    let valid = super::super::auth::hash_passphrase("fixture-passphrase").unwrap();
    let result = create_share_session(None, State(state.clone()), Json(CreateShareSessionRequest {
        invite_secret: "fixture-invite".to_owned(), invite_expires_at: None,
        passphrase_hash: Some(valid.replace("m=19456", "m=4294967295")), trusted_devices: Vec::new(),
    })).await;
    assert_eq!(result.err().unwrap().0, StatusCode::BAD_REQUEST);
    assert!(state.inner.lock().await.sessions.is_empty());
}

#[tokio::test]
async fn security_password_work_is_bounded_and_never_holds_the_session_registry() {
    let (state, session_id, _, _) = fixture().await;
    let hash = super::super::auth::hash_passphrase("fixture-passphrase").unwrap();
    state.inner.lock().await.sessions.get_mut(&session_id).unwrap().passphrase_hash = Some(hash);
    let body = JoinShareSessionRequest {
        display_name: "Second fixture".to_owned(), invite_secret: "fixture-invite".to_owned(),
        device_id: "second".to_owned(), passphrase: Some("fixture-passphrase".to_owned()),
    };
    let all_slots = state.verification_slots.clone()
        .try_acquire_many_owned(MAX_PASSWORD_VERIFICATIONS as u32).unwrap();
    assert_eq!(prepare_join_verification(&state, session_id, &body).await.err().unwrap().0, StatusCode::TOO_MANY_REQUESTS);
    drop(all_slots);
    let preparation = prepare_join_verification(&state, session_id, &body).await.unwrap();
    assert!(state.inner.try_lock().is_ok(), "password work retained the global registry lock");
    assert_eq!(state.verification_slots.available_permits(), MAX_PASSWORD_VERIFICATIONS - 1);
    drop(preparation);
    assert_eq!(state.verification_slots.available_permits(), MAX_PASSWORD_VERIFICATIONS);
}

#[tokio::test]
async fn security_join_rechecks_invite_rotation_after_verification() {
    let (state, session_id, _, host_token) = fixture().await;
    let body = JoinShareSessionRequest {
        display_name: "Second fixture".to_owned(), invite_secret: "fixture-invite".to_owned(),
        device_id: "second".to_owned(), passphrase: None,
    };
    let preparation = prepare_join_verification(&state, session_id, &body).await.unwrap();
    rotate_invite(State(state.clone()), Path(session_id.0), Json(RotateInviteRequest {
        host_token, invite_secret: "rotated-fixture".to_owned(), invite_expires_at: None,
    })).await.unwrap();
    let result = finish_join(&state, session_id, body, preparation.passphrase_hash, true).await;
    assert_eq!(result.err().unwrap().0, StatusCode::UNAUTHORIZED);
    assert_eq!(state.inner.lock().await.sessions[&session_id].guests.len(), 1);
}

#[tokio::test]
async fn security_session_creation_has_a_global_capacity_bound() {
    let state = BrokerState::new(BrokerConfig { require_loopback_session_creation: false });
    for _ in 0..MAX_SHARE_SESSIONS {
        create_share_session(None, State(state.clone()), Json(CreateShareSessionRequest {
            invite_secret: "fixture-invite".to_owned(), invite_expires_at: None,
            passphrase_hash: None, trusted_devices: Vec::new(),
        })).await.unwrap();
    }
    let result = create_share_session(None, State(state.clone()), Json(CreateShareSessionRequest {
        invite_secret: "fixture-invite".to_owned(), invite_expires_at: None,
        passphrase_hash: None, trusted_devices: Vec::new(),
    })).await;
    assert_eq!(result.err().unwrap().0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(state.inner.lock().await.sessions.len(), MAX_SHARE_SESSIONS);
}
