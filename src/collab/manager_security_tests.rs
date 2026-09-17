use super::*;
use crate::collab::{PanelShareScope, SharedPanelSnapshot};

fn host_fixture(panel_count: usize) -> (CollabManager, GuestId, SharedWorkspaceSnapshot) {
    let guest_id = GuestId(Uuid::new_v4());
    let snapshot = SharedWorkspaceSnapshot {
        workspace_id: Uuid::new_v4(),
        workspace_name: "Fixture".to_owned(),
        generated_at: Utc::now(),
        guests: Vec::new(),
        terminal_controls: Vec::new(),
        panels: (0..panel_count)
            .map(|_| SharedPanelSnapshot {
                panel_id: Uuid::new_v4(),
                title: "Fixture terminal".to_owned(),
                position: [0.0, 0.0],
                size: [800.0, 600.0],
                color: [0, 0, 0],
                z_index: 0,
                focused: false,
                minimized: false,
                alive: true,
                preview_label: String::new(),
                share_scope: PanelShareScope::Controllable,
                visible_text: "fixture".to_owned(),
                history_text: String::new(),
                controller: None,
                controller_name: None,
                queue_len: 0,
            })
            .collect(),
    };
    let mut manager = CollabManager::new();
    manager.mode = CollabMode::Host;
    manager.session_state = CollabSessionState::Live;
    manager.host = Some(HostSessionContext {
        session_id: ShareSessionId(Uuid::new_v4()),
        workspace_id: snapshot.workspace_id,
        host_token: "fixture-token".to_owned(),
        session_secret: random_secret(),
        previous_session_secret: None,
        invite_secret: "fixture-invite".to_owned(),
        invite_expires_at: None,
        requires_passphrase: false,
        tls_cert_pem: String::new(),
        invite_code: String::new(),
        guests: HashMap::from([(
            guest_id,
            GuestPresence {
                id: guest_id,
                display_name: "Fixture guest".to_owned(),
                joined_at: Utc::now(),
                connection_state: GuestConnectionState::Connected,
            },
        )]),
        pending_joins: Vec::new(),
        pending_control_requests: Vec::new(),
        terminal_controls: HashMap::new(),
        last_snapshot: Some(snapshot.clone()),
        last_snapshot_sent_at: None,
        rekey_recovery_until: None,
        last_rekey_recovery_at: None,
        next_message_seq: 1,
    });
    (manager, guest_id, snapshot)
}

fn request(terminal_id: Uuid, guest_id: GuestId) -> ControlRequest {
    ControlRequest {
        terminal_id,
        guest_id,
        display_name: "Fixture guest".to_owned(),
        requested_at: Utc::now(),
    }
}

#[test]
fn security_unknown_control_targets_never_poison_host_snapshots() {
    let (mut manager, guest, snapshot) = host_fixture(1);
    // Force the exact previously problematic threshold, without allocating big frames.
    for _ in 0..=MAX_SHARED_CONTROLS {
        manager.note_control_request(request(Uuid::new_v4(), guest));
    }
    let host = manager.host.as_ref().unwrap();
    assert!(host.terminal_controls.is_empty());
    assert!(host.pending_control_requests.is_empty());
    manager.publish_snapshot(snapshot);
    assert!(manager.last_error.is_none());
    assert!(manager
        .host
        .as_ref()
        .unwrap()
        .last_snapshot_sent_at
        .is_some());
}

#[test]
fn security_private_dead_pending_and_unknown_targets_cannot_gain_control() {
    let (mut manager, guest, mut snapshot) = host_fixture(1);
    let panel = snapshot.panels[0].panel_id;
    for scope in [
        PanelShareScope::Private,
        PanelShareScope::VisibleOnly,
        PanelShareScope::VisibleAndHistory,
    ] {
        snapshot.panels[0].share_scope = scope;
        manager.host.as_mut().unwrap().last_snapshot = Some(snapshot.clone());
        manager.note_control_request(request(panel, guest));
        manager.grant_control(panel, guest);
        assert!(manager.pending_control_requests().is_empty());
        assert_eq!(manager.controller_for(panel), None);
    }
    snapshot.panels[0].share_scope = PanelShareScope::Controllable;
    snapshot.panels[0].alive = false;
    manager.host.as_mut().unwrap().last_snapshot = Some(snapshot.clone());
    manager.note_control_request(request(panel, guest));
    assert!(manager.pending_control_requests().is_empty());
    snapshot.panels[0].alive = true;
    let host = manager.host.as_mut().unwrap();
    host.last_snapshot = Some(snapshot);
    host.guests.get_mut(&guest).unwrap().connection_state = GuestConnectionState::Pending;
    manager.note_control_request(request(panel, guest));
    assert!(manager.pending_control_requests().is_empty());
}

#[test]
fn security_control_requests_are_bounded_and_cleanup_recovers_capacity() {
    let (mut manager, guest, snapshot) = host_fixture(MAX_PENDING_CONTROLS_PER_GUEST + 1);
    for panel in &snapshot.panels {
        manager.note_control_request(request(panel.panel_id, guest));
    }
    let host = manager.host.as_ref().unwrap();
    assert_eq!(
        host.pending_control_requests.len(),
        MAX_PENDING_CONTROLS_PER_GUEST
    );
    assert_eq!(host.terminal_controls.len(), MAX_PENDING_CONTROLS_PER_GUEST);
    let panel = snapshot.panels[0].panel_id;
    manager.grant_control(panel, guest);
    assert_eq!(manager.controller_for(panel), Some(guest));
    manager.release_controls_for_guest(guest);
    let host = manager.host.as_ref().unwrap();
    assert!(host.pending_control_requests.is_empty());
    assert!(host.terminal_controls.is_empty());
    manager.note_control_request(request(panel, guest));
    assert_eq!(manager.pending_control_requests().len(), 1);
    let mut private = snapshot;
    private.panels[0].share_scope = PanelShareScope::Private;
    manager.publish_snapshot(private);
    assert!(manager.pending_control_requests().is_empty());
    assert_eq!(manager.controller_for(panel), None);
    assert!(manager.last_error.is_none());
}
