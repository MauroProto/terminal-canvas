//! Guest keyboard ownership exercised through real TLS/WebSocket transport.
//! Only the terminal grids are in memory; no provider account or OS PTY is used.

use std::time::{Duration, Instant};

use egui::{pos2, vec2, CentralPanel, RawInput, Rect};
use uuid::Uuid;

use super::TerminalApp;
use crate::collab::{
    CollabManager, CollabSessionState, HostShareOptions, PanelShareScope, SharedPanelSnapshot,
    SharedWorkspaceSnapshot, TerminalInputEvent,
};
use crate::state::{AppState, Workspace};

const DEADLINE: Duration = Duration::from_secs(10);

fn guest_app(ctx: &egui::Context) -> TerminalApp {
    let workspace = Workspace::new("Guest UI regression", None);
    let saved = AppState {
        schema_version: crate::state::persistence::APP_STATE_SCHEMA_VERSION,
        workspaces: vec![workspace.to_saved()],
        active_ws: 0,
        sidebar_visible: true,
        legacy_canvas_ui: Default::default(),
        local_device_id: Uuid::new_v4().to_string(),
        trusted_devices: Vec::new(),
        orchestration: Default::default(),
    };
    TerminalApp::build(ctx, None, Some(saved), None, false, false)
}

fn pump_until(
    host: &mut CollabManager,
    guest: &mut TerminalApp,
    done: impl Fn(&CollabManager, &TerminalApp) -> bool,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        assert!(
            host.drain_events().is_empty(),
            "setup must not emit terminal input"
        );
        assert!(guest.collab.drain_events().is_empty());
        host.publish_snapshot(test_snapshot());
        if done(host, guest) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Online setup timeout: host={:?}, guest={:?}, host_error={:?}, guest_error={:?}",
            host.session_state(),
            guest.collab.session_state(),
            host.last_error(),
            guest.collab.last_error()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Stable IDs keep snapshots comparable while the real transport connects.
fn test_snapshot() -> SharedWorkspaceSnapshot {
    SharedWorkspaceSnapshot {
        workspace_id: Uuid::from_u128(1),
        workspace_name: "Isolated Online fixture".into(),
        generated_at: chrono::Utc::now(),
        guests: Vec::new(),
        terminal_controls: Vec::new(),
        panels: vec![SharedPanelSnapshot {
            panel_id: Uuid::from_u128(2),
            title: "Remote test terminal".into(),
            position: [0.0, 0.0],
            size: [650.0, 350.0],
            color: [100, 100, 100],
            z_index: 1,
            focused: true,
            minimized: false,
            alive: true,
            preview_label: String::new(),
            share_scope: PanelShareScope::Controllable,
            visible_text: "READY".into(),
            history_text: "READY".into(),
            controller: None,
            controller_name: None,
            queue_len: 0,
        }],
    }
}

fn guest_frame(
    guest: &mut TerminalApp,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
    focused: bool,
) {
    ctx.run_ui(
        RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 600.0))),
            focused,
            events,
            ..Default::default()
        },
        |ui| {
            CentralPanel::default().show(ui, |ui| guest.show_guest_canvas(ui, ctx, ui.max_rect()));
        },
    )
    .drop_without_applying_deltas();
}

#[test]
fn online_guest_dialog_text_never_reaches_the_real_host_transport() {
    let ctx = egui::Context::default();
    let mut guest = guest_app(&ctx);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut host = CollabManager::new();
    host.start_host_session(
        Uuid::from_u128(1),
        HostShareOptions {
            bind_addr: address,
            reachable_url: format!("https://{address}"),
        },
        None,
        Vec::new(),
    )
    .unwrap();
    let invite = host.invite_code().unwrap().to_owned();
    guest
        .collab
        .join_session(
            &invite,
            "UI fixture guest".into(),
            None,
            guest.local_device_id.clone(),
        )
        .unwrap();
    pump_until(&mut host, &mut guest, |host, _| {
        !host.pending_joins().is_empty()
    });
    assert_eq!(host.pending_joins().len(), 1);
    let id = host.pending_joins()[0].guest_id;
    host.approve_join(id);
    pump_until(&mut host, &mut guest, |_, guest| {
        guest.collab.session_state() == CollabSessionState::Live
            && guest.collab.guest_view().snapshot.is_some()
    });
    guest.collab.focus_remote_panel(Uuid::from_u128(2));
    guest.collab.request_control(Uuid::from_u128(2));
    pump_until(&mut host, &mut guest, |host, _| {
        !host.pending_control_requests().is_empty()
    });
    host.grant_control(Uuid::from_u128(2), id);
    pump_until(&mut host, &mut guest, |_, guest| {
        guest.collab.guest_view().snapshot.as_ref().unwrap().panels[0].controller == Some(id)
    });

    // Each surface previously outside the guest's three-dialog allowlist.
    guest.settings_open = true;
    guest_frame(
        &mut guest,
        &ctx,
        vec![egui::Event::Text("settings-private".into())],
        true,
    );
    guest.settings_open = false;
    guest.renaming_panel = Some(Uuid::from_u128(3));
    guest_frame(
        &mut guest,
        &ctx,
        vec![egui::Event::Paste("rename-private".into())],
        true,
    );
    guest.renaming_panel = None;
    guest.search_open = true;
    guest_frame(
        &mut guest,
        &ctx,
        vec![egui::Event::Text("search-private".into())],
        true,
    );
    guest.search_open = false;
    guest.closing_workspace = Some(Uuid::from_u128(4));
    guest_frame(
        &mut guest,
        &ctx,
        vec![egui::Event::Text("confirmation-private".into())],
        true,
    );
    guest.closing_workspace = None;
    guest_frame(
        &mut guest,
        &ctx,
        vec![egui::Event::Text("unfocused-private".into())],
        false,
    );

    // A later visible terminal event is the ordered transport barrier: if any
    // private event was sent earlier, it must arrive before this same-socket event.
    guest_frame(
        &mut guest,
        &ctx,
        vec![egui::Event::Text("PUBLIC-TERMINAL-INPUT".into())],
        true,
    );
    let deadline = Instant::now() + DEADLINE;
    loop {
        guest.collab.drain_events();
        let events = host.drain_events();
        if !events.is_empty() {
            assert_eq!(
                events.len(),
                1,
                "dialog text leaked to the host: {events:?}"
            );
            let crate::collab::CollabEvent::RemoteInput { guest_id, input } = &events[0];
            assert_eq!(*guest_id, id);
            assert_eq!(
                input.events,
                vec![TerminalInputEvent::Text("PUBLIC-TERMINAL-INPUT".into())]
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "visible terminal input did not reach host"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    guest.collab.stop_session();
    host.stop_session();
}
