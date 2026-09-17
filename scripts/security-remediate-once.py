"""One-time deterministic remediation of the audited source. No target code is executed."""
from pathlib import Path
import subprocess

EXPECTED = {
    'src/collab/broker.rs': '5c90f001692ce2c1df922c119b1491691518b767',
    'src/collab/manager.rs': '927b180f25258ec0b350ac211647f775ea9833de',
    'src/collab/auth.rs': '8b0a0ea575b62c07f8820c1e6cd480c3705f22e9',
    'src/collab/tls.rs': 'ef93e5c30be9b0c186a4a090cbd64d9d575fe84d',
    'src/app.rs': '9c44e55e3f0e235f37b5317eac7310d521e6d809',
    'src/app/orchestration_ui.rs': 'e12489b7a9b3e30038be9854d112550102f3f068',
    'src/config.rs': 'b375aff23e5959da7532a003f415649c1e7bda36',
    'src/state/durable_write.rs': '19ebba261bad310ba3fbbd392c54ddc27311e02a',
    'src/terminal/input.rs': '78e4f68502fccb3ab5d4a13550f7a77f61796f05',
}
for path, expected in EXPECTED.items():
    actual = subprocess.check_output(['git', 'hash-object', path], text=True).strip()
    if actual != expected:
        raise SystemExit(f'Unexpected source revision: {path}')

def edit(path, before, after, count=1):
    p = Path(path)
    text = p.read_text()
    found = text.count(before)
    if found != count:
        raise SystemExit(f'Expected {count} anchors in {path}, got {found}: {before[:80]!r}')
    p.write_text(text.replace(before, after))

def section(path, start, end, replacement):
    p = Path(path)
    text = p.read_text()
    if text.count(start) != 1 or text.count(end) != 1:
        raise SystemExit(f'Ambiguous section in {path}: {start}')
    a, b = text.index(start), text.index(end)
    if a >= b:
        raise SystemExit(f'Reversed section in {path}')
    p.write_text(text[:a] + replacement + '\n\n' + text[b:])

def append(path, contents):
    p = Path(path)
    p.write_text(p.read_text() + '\n' + contents + '\n')

# C1: Approval is a separate, persistent authorization decision.
broker = 'src/collab/broker.rs'
edit(broker, 'use tokio::sync::{mpsc, Mutex};', 'use tokio::sync::{mpsc, Mutex, Semaphore};')
edit(broker, '    inner: Arc<Mutex<Sessions>>,', '    inner: Arc<Mutex<Sessions>>,\n    password_work: Arc<Semaphore>,')
edit(broker, '            inner: Arc::new(Mutex::new(Sessions::default())),', '            inner: Arc::new(Mutex::new(Sessions::default())),\n            password_work: Arc::new(Semaphore::new(2)),')
edit(broker, 'struct GuestRecord {\n', '''#[derive(Clone, Copy, PartialEq, Eq)]
enum GuestAuthorization { Pending, Approved, Denied }

struct GuestRecord {
    authorization: GuestAuthorization,
''')
edit(broker, '        GuestRecord {\n            token: guest_token.clone(),', '''        GuestRecord {
            authorization: if auto_approved { GuestAuthorization::Approved } else { GuestAuthorization::Pending },
            token: guest_token.clone(),''')
edit(broker, '                    GuestRecord {\n                        token: "guest-token".to_owned(),', '                    GuestRecord {\n                        authorization: GuestAuthorization::Approved,\n                        token: "guest-token".to_owned(),')
edit(broker, '    guest.connection_state = GuestConnectionState::Approved;', '''    guest.authorization = GuestAuthorization::Approved;
    guest.connection_state = if guest.tx.is_some() { GuestConnectionState::Connected } else { GuestConnectionState::Approved };''')
edit(broker, '    guest.connection_state = GuestConnectionState::Denied;', '    guest.authorization = GuestAuthorization::Denied;\n    guest.connection_state = GuestConnectionState::Denied;')
edit(broker, '''                let (guest_id, _) = session
                    .guests
                    .iter()
                    .find(|(_, guest)| constant_time_str_eq(&guest.token, &query.token))''', '''                let (guest_id, _) = session
                    .guests
                    .iter()
                    .find(|(_, guest)| guest.authorization != GuestAuthorization::Denied
                        && constant_time_str_eq(&guest.token, &query.token))''')
edit(broker, '''                guest.tx = Some(tx.clone());
                guest.connection_id = Some(connection_id);
                guest.last_seen = Instant::now();
                guest.disconnected_at = None;
                if matches!(
                    guest.connection_state,
                    GuestConnectionState::Approved | GuestConnectionState::Disconnected
                ) {
                    guest.connection_state = GuestConnectionState::Connected;
                }''', '''                // Denial may race with stream authentication; re-check at upgrade.
                if guest.authorization == GuestAuthorization::Denied { return; }
                if let Some(previous) = guest.tx.replace(tx.clone()) {
                    let _ = previous.try_send(Message::Close(None));
                }
                guest.connection_id = Some(connection_id);
                guest.last_seen = Instant::now();
                guest.disconnected_at = None;
                guest.mark_connected();''')
edit(broker, '                session.host_tx = Some(tx.clone());', '''                if let Some(previous) = session.host_tx.replace(tx.clone()) {
                    let _ = previous.try_send(Message::Close(None));
                }''')
edit(broker, '''                    if !matches!(guest.connection_state, GuestConnectionState::Denied) {
                        guest.connection_state = GuestConnectionState::Disconnected;
                    }''', '                    guest.mark_disconnected();', count=2)
edit(broker, '''                    if matches!(
                        guest.connection_state,
                        GuestConnectionState::Approved | GuestConnectionState::Connected
                    ) {''', '                    if guest.can_relay() {')
edit(broker, '''                if matches!(
                    guest.connection_state,
                    GuestConnectionState::Approved | GuestConnectionState::Connected
                ) {''', '                if guest.can_relay() {')
edit(broker, '''            if !matches!(
                guest.connection_state,
                GuestConnectionState::Approved | GuestConnectionState::Connected
            ) {''', '            if !guest.can_relay() {')
edit(broker, 'relay_payload(&state, session_id, auth, Message::Binary(payload)).await;', 'relay_payload(&state, session_id, auth, connection_id, Message::Binary(payload)).await;')
edit(broker, '''async fn relay_payload(
    state: &BrokerState,
    session_id: ShareSessionId,
    auth: StreamAuth,
    payload: Message,''', '''async fn relay_payload(
    state: &BrokerState,
    session_id: ShareSessionId,
    auth: StreamAuth,
    connection_id: Uuid,
    payload: Message,''')
edit(broker, '''        StreamAuth::Host => {
            for guest in session.guests.values() {''', '''        StreamAuth::Host => {
            if session.host_connection_id != Some(connection_id) { return; }
            for guest in session.guests.values() {''')
edit(broker, '''            if !guest.can_relay() {
                return;
            }''', '''            if !guest.can_relay() || guest.connection_id != Some(connection_id) { return; }''')
append(broker, '''
impl GuestRecord {
    fn mark_connected(&mut self) {
        self.connection_state = match self.authorization {
            GuestAuthorization::Pending => GuestConnectionState::Pending,
            GuestAuthorization::Approved => GuestConnectionState::Connected,
            GuestAuthorization::Denied => GuestConnectionState::Denied,
        };
    }
    fn mark_disconnected(&mut self) {
        self.connection_state = match self.authorization {
            GuestAuthorization::Pending => GuestConnectionState::Pending,
            GuestAuthorization::Approved => GuestConnectionState::Disconnected,
            GuestAuthorization::Denied => GuestConnectionState::Denied,
        };
    }
    fn can_relay(&self) -> bool {
        self.authorization == GuestAuthorization::Approved
            && matches!(self.connection_state, GuestConnectionState::Approved | GuestConnectionState::Connected)
    }
}
''')

# C6: Accept only the bounded PHC policy produced by this application.
auth = 'src/collab/auth.rs'
edit(auth, '''    let parsed = PasswordHash::new(hash)
        .map_err(|err| anyhow::anyhow!("failed to parse passphrase hash: {err}"))?;
    Ok(argon2id()?''', '''    validate_passphrase_hash(hash)?;
    let parsed = PasswordHash::new(hash)
        .map_err(|err| anyhow::anyhow!("failed to parse passphrase hash: {err}"))?;
    Ok(argon2id()?''')
append(auth, '''
/// Validate before invoking Argon2: PHC parameters override verifier defaults.
pub fn validate_passphrase_hash(hash: &str) -> anyhow::Result<()> {
    anyhow::ensure!(hash.len() <= 256, "Passphrase hash too large");
    let parsed = PasswordHash::new(hash).map_err(|_| anyhow::anyhow!("Invalid passphrase hash"))?;
    let params = Params::try_from(&parsed).map_err(|_| anyhow::anyhow!("Invalid Argon2 parameters"))?;
    anyhow::ensure!(parsed.algorithm.as_str() == "argon2id" && parsed.version == Some(19)
        && params.m_cost() == ARGON2_MEMORY_KIB && params.t_cost() == ARGON2_ITERATIONS
        && params.p_cost() == ARGON2_PARALLELISM && parsed.params.iter().count() == 3,
        "Unsupported passphrase hash policy");
    let mut salt_bytes = [0u8; 64];
    let salt = parsed.salt.ok_or_else(|| anyhow::anyhow!("Missing salt"))?;
    let decoded = salt.decode_b64(&mut salt_bytes).map_err(|_| anyhow::anyhow!("Invalid salt"))?;
    anyhow::ensure!(decoded.len() == 16 && parsed.hash.is_some_and(|output| output.len() == 32),
        "Invalid passphrase hash size");
    Ok(())
}
#[cfg(test)]
mod security_policy_tests {
    use super::*;
    #[test]
    fn only_the_generated_bounded_hash_policy_is_accepted() {
        let hash = hash_passphrase("dummy-password").unwrap();
        validate_passphrase_hash(&hash).unwrap();
        for altered in [hash.replace("m=19456", "m=19457"), hash.replace("t=2", "t=3"),
            hash.replace("p=1", "p=2"), hash.replace("argon2id", "argon2i"), hash.replace("v=19", "v=16")]
        {
            assert!(validate_passphrase_hash(&altered).is_err());
            assert!(verify_passphrase(&altered, "dummy-password").is_err());
        }
        for invalid in ["", "not-a-hash", "$argon2id$v=19$m=19456,t=2,p=1"] {
            assert!(validate_passphrase_hash(invalid).is_err());
        }
    }
}
''')
edit(broker, '    let session_id = ShareSessionId(Uuid::new_v4());\n    let host_token = random_token();', '''    if body.invite_secret.is_empty() || body.invite_secret.len() > MAX_INVITE_SECRET_LEN
        || body.trusted_devices.len() > 128
        || body.trusted_devices.iter().any(|device| device.device_id.is_empty()
            || device.device_id.len() > MAX_DEVICE_ID_LEN || device.last_display_name.len() > MAX_DISPLAY_NAME_LEN)
    {
        return Err((StatusCode::BAD_REQUEST, "Invalid session configuration".to_owned()));
    }
    if let Some(hash) = &body.passphrase_hash {
        super::auth::validate_passphrase_hash(hash)
            .map_err(|_| (StatusCode::BAD_REQUEST, "Unsupported passphrase hash policy".to_owned()))?;
    }
    let session_id = ShareSessionId(Uuid::new_v4());
    let host_token = random_token();''')
edit(broker, '''    state
        .inner
        .lock()
        .await
        .sessions
        .insert(session_id, session);''', '''    let mut guard = state.inner.lock().await;
    if guard.sessions.len() >= 64 {
        return Err((StatusCode::TOO_MANY_REQUESTS, "Session capacity reached".to_owned()));
    }
    guard.sessions.insert(session_id, session);''')
p = Path(broker)
s = p.read_text()
a = s.index('    let session_id = ShareSessionId(session_id);\n    let mut guard = state.inner.lock().await;', s.index('async fn join_share_session('))
b = s.index('    let guest_id = GuestId(Uuid::new_v4());', a)
s = s[:a] + '''    let session_id = ShareSessionId(session_id);
    let passphrase_hash = {
        let guard = state.inner.lock().await;
        let session = guard.sessions.get(&session_id)
            .ok_or((StatusCode::NOT_FOUND, "Session not found".to_owned()))?;
        check_join_access(session, &body, Instant::now())?;
        session.passphrase_hash.clone()
    };
    // The permit remains held by the work even if its HTTP request is cancelled.
    // No sessions mutex is held while computing or waiting for password work.
    let verified = if let Some(hash) = passphrase_hash.clone() {
        if let Some(passphrase) = body.passphrase.clone() {
            let permit = state.password_work.clone().try_acquire_owned()
                .map_err(|_| (StatusCode::TOO_MANY_REQUESTS, "Password verification busy".to_owned()))?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                verify_passphrase(&hash, &passphrase).unwrap_or(false)
            }).await.map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "Password verification unavailable".to_owned()))?
        } else { false }
    } else { true };
    let mut guard = state.inner.lock().await;
    let session = guard.sessions.get_mut(&session_id)
        .ok_or((StatusCode::NOT_FOUND, "Session not found".to_owned()))?;
    let now = Instant::now();
    // Re-check rotation, expiry, lockout and capacity after the unlocked calculation.
    check_join_access(session, &body, now)?;
    if session.passphrase_hash != passphrase_hash {
        return Err((StatusCode::UNAUTHORIZED, "Session credentials changed".to_owned()));
    }
    if !verified {
        register_failed_join_attempt(session, now);
        return Err((StatusCode::UNAUTHORIZED, "Invalid or missing session passphrase".to_owned()));
    }
    session.failed_join_attempts = 0;
    session.join_locked_until = None;

''' + s[b:]
p.write_text(s)
append(broker, '''
fn check_join_access(session: &SessionRecord, body: &JoinShareSessionRequest, now: Instant)
    -> Result<(), (StatusCode, String)>
{
    if session.join_locked_until.is_some_and(|until| until > now) {
        return Err((StatusCode::TOO_MANY_REQUESTS, "Too many failed attempts".to_owned()));
    }
    if session.invite_expires_at.is_some_and(|expires| expires <= Utc::now()) {
        return Err((StatusCode::GONE, "Invite expired".to_owned()));
    }
    if !constant_time_str_eq(&session.invite_secret, &body.invite_secret) {
        return Err((StatusCode::UNAUTHORIZED, "Invalid invite secret".to_owned()));
    }
    if active_guest_count(session) >= 3 {
        return Err((StatusCode::BAD_REQUEST, "Participant limit reached".to_owned()));
    }
    Ok(())
}
''')

# C2: Authorize the resource before retaining or mutating control requests.
manager = 'src/collab/manager.rs'
edit(manager, '''    pub fn note_control_request(&mut self, request: ControlRequest) {
        let Some(host) = &mut self.host else {
            return;
        };''', '''    pub fn note_control_request(&mut self, request: ControlRequest) {
        let Some(host) = &mut self.host else { return; };
        if !control_target_allowed(host, request.terminal_id, request.guest_id)
            || host.guests.get(&request.guest_id).is_none_or(|guest| guest.display_name != request.display_name)
            || host.pending_control_requests.len() >= MAX_SHARED_CONTROLS
            || (!host.terminal_controls.contains_key(&request.terminal_id) && host.terminal_controls.len() >= MAX_SHARED_CONTROLS)
            || host.terminal_controls.get(&request.terminal_id).is_some_and(|control|
                control.controller == Some(request.guest_id) || control.queue.len() >= MAX_CONTROL_QUEUE)
        { return; }''')
edit(manager, '''    pub fn grant_control(&mut self, terminal_id: Uuid, guest_id: GuestId) {
        let Some(host) = &mut self.host else {
            return;
        };''', '''    pub fn grant_control(&mut self, terminal_id: Uuid, guest_id: GuestId) {
        let Some(host) = &mut self.host else { return; };
        if !control_target_allowed(host, terminal_id, guest_id) { return; }''')
edit(manager, '        snapshot.guests = host.guests.values().cloned().collect();', '''        // Prune before output validation so stale controls cannot poison snapshots.
        let controllable: HashSet<_> = snapshot.panels.iter()
            .filter(|panel| panel.share_scope.allows_control()).map(|panel| panel.panel_id).collect();
        host.pending_control_requests.retain(|request| controllable.contains(&request.terminal_id));
        host.terminal_controls.retain(|id, _| controllable.contains(id));
        snapshot.guests = host.guests.values().cloned().collect();''')
edit(manager, '''            host.pending_control_requests
                .retain(|request| request.guest_id != guest_id);''', '''            host.pending_control_requests.retain(|request| request.guest_id != guest_id);
            for control in host.terminal_controls.values_mut() {
                control.queue.retain(|queued| *queued != guest_id);
            }
            host.terminal_controls.retain(|_, control| control.controller.is_some() || !control.queue.is_empty());''')
append(manager, '''
fn control_target_allowed(host: &HostSessionContext, terminal_id: Uuid, guest_id: GuestId) -> bool {
    host.guests.get(&guest_id).is_some_and(|guest|
        matches!(guest.connection_state, GuestConnectionState::Approved | GuestConnectionState::Connected))
        && host.last_snapshot.as_ref().is_some_and(|snapshot|
            snapshot.workspace_id == host.workspace_id && snapshot.panels.iter().any(|panel|
                panel.panel_id == terminal_id && panel.share_scope.allows_control()))
}
#[cfg(test)]
mod security_control_tests {
    use super::*;
    use crate::collab::models::{PanelShareScope, SharedPanelSnapshot};
    fn fixture() -> (CollabManager, Uuid, GuestId) {
        let mut manager = CollabManager::new();
        let terminal_id = Uuid::new_v4();
        let guest_id = GuestId(Uuid::new_v4());
        let workspace_id = Uuid::new_v4();
        let snapshot = SharedWorkspaceSnapshot {
            workspace_id, workspace_name: "Dummy".to_owned(), generated_at: Utc::now(),
            guests: Vec::new(), terminal_controls: Vec::new(),
            panels: vec![SharedPanelSnapshot {
                panel_id: terminal_id, title: "Dummy".to_owned(), position: [0.0, 0.0], size: [800.0, 600.0],
                color: [0, 0, 0], z_index: 0, focused: false, minimized: false, alive: true,
                preview_label: String::new(), share_scope: PanelShareScope::Controllable,
                visible_text: String::new(), history_text: String::new(), controller: None,
                controller_name: None, queue_len: 0,
            }],
        };
        manager.mode = CollabMode::Host;
        manager.session_state = CollabSessionState::Live;
        manager.host = Some(HostSessionContext {
            session_id: ShareSessionId(Uuid::new_v4()), workspace_id,
            host_token: "dummy".to_owned(), session_secret: random_secret(), previous_session_secret: None,
            invite_secret: "dummy".to_owned(), invite_expires_at: None, requires_passphrase: false,
            tls_cert_pem: String::new(), invite_code: String::new(),
            guests: HashMap::from([(guest_id, GuestPresence {
                id: guest_id, display_name: "Dummy".to_owned(), joined_at: Utc::now(),
                connection_state: GuestConnectionState::Connected,
            })]),
            pending_joins: Vec::new(), pending_control_requests: Vec::new(), terminal_controls: HashMap::new(),
            last_snapshot: Some(snapshot), last_snapshot_sent_at: None,
            rekey_recovery_until: None, last_rekey_recovery_at: None, next_message_seq: 1,
        });
        (manager, terminal_id, guest_id)
    }
    fn request(terminal_id: Uuid, guest_id: GuestId) -> ControlRequest {
        ControlRequest { terminal_id, guest_id, display_name: "Dummy".to_owned(), requested_at: Utc::now() }
    }
    #[test]
    fn nonexistent_targets_do_not_accumulate_or_poison_snapshots() {
        let (mut manager, terminal_id, guest_id) = fixture();
        for _ in 0..MAX_SHARED_CONTROLS + 1 {
            manager.note_control_request(request(Uuid::new_v4(), guest_id));
        }
        assert!(manager.pending_control_requests().is_empty());
        assert!(manager.host.as_ref().unwrap().terminal_controls.is_empty());
        manager.note_control_request(request(terminal_id, guest_id));
        manager.note_control_request(request(terminal_id, guest_id));
        assert_eq!(manager.pending_control_requests().len(), 1);
        assert_eq!(manager.host.as_ref().unwrap().terminal_controls.len(), 1);
        let snapshot = manager.host.as_ref().unwrap().last_snapshot.clone().unwrap();
        manager.publish_snapshot(snapshot);
        assert!(manager.last_error().is_none());
    }
    #[test]
    fn private_read_only_and_unknown_guests_cannot_request_or_gain_control() {
        let (mut manager, terminal_id, guest_id) = fixture();
        for scope in [PanelShareScope::Private, PanelShareScope::VisibleOnly, PanelShareScope::VisibleAndHistory] {
            manager.host.as_mut().unwrap().last_snapshot.as_mut().unwrap().panels[0].share_scope = scope;
            manager.note_control_request(request(terminal_id, guest_id));
            manager.grant_control(terminal_id, guest_id);
            assert!(manager.pending_control_requests().is_empty());
            assert_eq!(manager.controller_for(terminal_id), None);
        }
        manager.host.as_mut().unwrap().last_snapshot.as_mut().unwrap().panels[0].share_scope = PanelShareScope::Controllable;
        manager.note_control_request(request(terminal_id, GuestId(Uuid::new_v4())));
        assert!(manager.pending_control_requests().is_empty());
    }
    #[test]
    fn changing_scope_prunes_control_state_before_publication() {
        let (mut manager, terminal_id, guest_id) = fixture();
        manager.note_control_request(request(terminal_id, guest_id));
        manager.grant_control(terminal_id, guest_id);
        assert_eq!(manager.controller_for(terminal_id), Some(guest_id));
        let mut snapshot = manager.host.as_ref().unwrap().last_snapshot.clone().unwrap();
        snapshot.panels[0].share_scope = PanelShareScope::Private;
        manager.publish_snapshot(snapshot);
        assert_eq!(manager.controller_for(terminal_id), None);
        assert!(manager.pending_control_requests().is_empty());
    }
}
''')

# C3: External captures are reviewed and copied, never submitted to a terminal.
ui = 'src/app/orchestration_ui.rs'
section(ui, '    /// Drena las capturas de Design Mode (P3.18, T3)', '    pub(super) fn maybe_refresh_orchestration', '''    /// Review captures without sending bytes or Enter to any terminal.
    pub(super) fn poll_design_captures(&mut self, ctx: &egui::Context) {
        let id = egui::Id::new("terminalcanvas.pending-design-captures");
        let mut pending = ctx.data_mut(|data|
            data.get_temp::<Vec<crate::orchestration::DesignCapture>>(id).unwrap_or_default());
        if let Some(server) = self.hook_server.as_ref() {
            for capture in server.poll_design() {
                if pending.len() < 16 { pending.push(capture); }
                else { discard_design_screenshot(&capture); }
            }
        }
        if let Some(capture) = pending.first() {
            match review_design_capture(ctx, capture) {
                CaptureReviewAction::Keep => {}
                CaptureReviewAction::Copy => {
                    ctx.copy_text(crate::terminal::input::sanitize_agent_prompt(
                        &crate::orchestration::format_design_capture(capture)));
                    pending.remove(0);
                    self.toast_success("Texto copiado. Revisalo y pegalo en el agente elegido.");
                }
                CaptureReviewAction::Discard => {
                    discard_design_screenshot(capture);
                    pending.remove(0);
                }
            }
        }
        ctx.data_mut(|data| data.insert_temp(id, pending));
    }''')
edit('src/app.rs', 'self.poll_design_captures();', 'self.poll_design_captures(ctx);')
append(ui, '''
#[derive(Debug, PartialEq, Eq)]
enum CaptureReviewAction { Keep, Copy, Discard }
fn review_design_capture(ctx: &egui::Context, capture: &crate::orchestration::DesignCapture) -> CaptureReviewAction {
    let mut action = CaptureReviewAction::Keep;
    egui::Window::new("Revisar captura web").id(egui::Id::new("terminalcanvas.design-review"))
        .collapsible(false).default_width(560.0).show(ctx, |ui| {
            ui.label("Contenido externo no confiable. No se enviará ni ejecutará en ningún terminal.");
            let mut preview = crate::terminal::input::sanitize_agent_prompt(
                &crate::orchestration::format_design_capture(capture));
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut preview).desired_width(f32::INFINITY)
                    .interactive(false).code_editor());
            });
            ui.horizontal(|ui| {
                if ui.button("Copiar texto").clicked() { action = CaptureReviewAction::Copy; }
                if ui.button("Descartar").clicked() { action = CaptureReviewAction::Discard; }
            });
        });
    action
}
fn discard_design_screenshot(capture: &crate::orchestration::DesignCapture) {
    if let Some(path) = &capture.screenshot_path { let _ = std::fs::remove_file(path); }
}
#[cfg(test)]
mod capture_security_tests {
    use super::*;
    #[test]
    fn review_never_copies_or_submits_without_an_explicit_button_action() {
        let ctx = egui::Context::default();
        let capture = crate::orchestration::DesignCapture {
            selector: "button".to_owned(), html: "dummy untrusted text".to_owned(),
            css: String::new(), rect: String::new(), screenshot_path: None,
        };
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            assert_eq!(review_design_capture(ctx, &capture), CaptureReviewAction::Keep);
        });
        assert!(output.platform_output.commands.is_empty());
    }
}
''')
edit('src/terminal/input.rs', '''    text.replace('\\x1b', "<ESC>")''', '''    text.chars().map(|ch| {
        if ch == '\\x1b' { "<ESC>".to_owned() }
        else if ch.is_control() && !matches!(ch, '\\n' | '\\t') { " ".to_owned() }
        else { ch.to_string() }
    }).collect()''')
append('src/terminal/input.rs', '''
#[cfg(test)]
mod prompt_security_tests {
    #[test]
    fn prompt_sanitization_drops_terminal_controls_but_keeps_text() {
        let text = super::sanitize_agent_prompt("hola\\x03\\r\\x00\\x1b\\n\\tfin");
        assert_eq!(text, "hola   <ESC>\\n\\tfin");
    }
}
''')

# C4: Exclusive pinned trust, no redirects for state-changing collaboration requests.
tls = 'src/collab/tls.rs'
edit(tls, '        .https_only(true);', '        .https_only(true)\n        .redirect(reqwest::redirect::Policy::none());')
edit(tls, '        builder = builder.add_root_certificate(cert);', '        builder = builder.tls_built_in_root_certs(false).add_root_certificate(cert);')
edit(tls, '    let mut roots = RootCertStore::empty();', '    anyhow::ensure!(certs.len() == 1, "Expected one pinned certificate");\n    let mut roots = RootCertStore::empty();')
append(tls, '''
#[cfg(test)]
mod security_tls_tests {
    use super::*;
    use crate::collab::server::EmbeddedCollabServer;
    #[test]
    fn pinned_http_accepts_its_host_and_rejects_a_different_certificate() {
        let material = generate_tls_material(vec!["127.0.0.1".to_owned()]).unwrap();
        let other = generate_tls_material(vec!["127.0.0.1".to_owned()]).unwrap();
        let mut server = EmbeddedCollabServer::start("127.0.0.1:0".parse().unwrap(),
            material.cert_pem.clone(), material.key_pem).unwrap();
        let response = http_client(Some(&material.cert_pem)).unwrap().get(server.local_api_url()).send();
        assert!(response.is_ok(), "the pinned host must be reachable: {response:?}");
        assert!(http_client(Some(&other.cert_pem)).unwrap().get(server.local_api_url()).send().is_err());
        assert!(http_client(None).unwrap().get(server.local_api_url()).send().is_err());
        server.stop().unwrap();
    }
    #[test]
    fn malformed_pins_fail_closed_for_http_and_websocket() {
        assert!(http_client(Some("invalid certificate")).is_err());
        assert!(websocket_connector(Some("invalid certificate")).is_err());
    }
}
''')

# C5: Unique temporary files private from creation; private atomic backups.
durable = 'src/state/durable_write.rs'
section(durable, 'fn write_atomic_changed(path: &Path, bytes: &[u8])', 'fn content_matches(path: &Path, bytes: &[u8])', '''fn write_atomic_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let tmp = sibling_with_suffix(path, &format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        sync_directory(path);
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(&tmp); }
    result
}''')
edit(durable, '    if let Err(err) = std::fs::copy(path, &newest) {', '    if let Err(err) = std::fs::read(path).and_then(|bytes| write_atomic_changed(&newest, &bytes)) {')
append(durable, '''
/// Migrate existing Unix configuration and backups even for content-equal saves.
/// Windows inherits the per-user profile ACL; mode bits are not an ACL substitute.
pub fn protect_private_files(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)] {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        // SAFETY: geteuid has no pointer arguments and only returns process identity.
        let uid = unsafe { libc::geteuid() };
        let parent = path.parent().ok_or_else(|| std::io::Error::other("Private file has no parent"))?;
        let parent_metadata = match std::fs::symlink_metadata(parent) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() || parent_metadata.uid() != uid {
            return Err(std::io::Error::other("Private directory must be owned by the current user and not be a symlink"));
        }
        let directory = std::fs::OpenOptions::new().read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY).open(parent)?;
        let metadata = directory.metadata()?;
        if !metadata.is_dir() || metadata.uid() != uid { return Err(std::io::Error::other("Invalid private directory owner")); }
        directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        let mut paths = candidate_paths(path);
        paths.push(tmp_path(path));
        for candidate in paths {
            let file = match std::fs::OpenOptions::new().read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(&candidate) {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != uid {
                return Err(std::io::Error::other("Private config must be an owner-only regular file"));
            }
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(not(unix))] let _ = path;
    Ok(())
}
#[cfg(all(test, unix))]
mod private_file_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn config_migration_noop_replacement_and_backups_remain_private() {
        let root = std::env::temp_dir().join(format!("tc-private-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        let config = crate::config::AppConfig { linear_token: Some("dummy-private-value".to_owned()), ..Default::default() };
        crate::config::save_to_path(&config, &path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(backup_path(&path, 0), "old dummy").unwrap();
        std::fs::set_permissions(backup_path(&path, 0), std::fs::Permissions::from_mode(0o644)).unwrap();
        crate::config::save_to_path(&config, &path).unwrap();
        for candidate in [&path, &backup_path(&path, 0)] {
            assert_eq!(std::fs::metadata(candidate).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(std::fs::metadata(&root).unwrap().permissions().mode() & 0o777, 0o700);
        let mut changed = config.clone();
        changed.font_size += 1.0;
        crate::config::save_to_path(&changed, &path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(crate::config::load_from_path(&path).linear_token, config.linear_token);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn private_config_rejects_symlinks_and_hardlinks() {
        let root = std::env::temp_dir().join(format!("tc-links-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("other.txt");
        let path = root.join("config.toml");
        std::fs::write(&target, "untouched").unwrap();
        symlink(&target, &path).unwrap();
        assert!(protect_private_files(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&target, &path).unwrap();
        assert!(protect_private_files(&path).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "untouched");
        std::fs::remove_dir_all(root).unwrap();
    }
}
''')
edit('src/config.rs', '''pub fn load_from_path(path: &std::path::Path) -> AppConfig {
    let mut config = AppConfig::default();''', '''pub fn load_from_path(path: &std::path::Path) -> AppConfig {
    if let Err(err) = crate::state::durable_write::protect_private_files(path) {
        log::warn!("No se puede proteger la configuración privada: {err}");
        return AppConfig::default();
    }
    let mut config = AppConfig::default();''')
edit('src/config.rs', '''    let file = ConfigFile {
        terminal: TerminalSection {''', '''    crate::state::durable_write::protect_private_files(path)?;
    let file = ConfigFile {
        terminal: TerminalSection {''')

# In-process handler regressions use dummy identities and no deployed service.
append(broker, '''
#[cfg(test)]
mod security_regressions {
    use super::*;
    async fn session(hash: Option<String>) -> (BrokerState, CreateShareSessionResponse) {
        let state = BrokerState::new(BrokerConfig { require_loopback_session_creation: false });
        let created = create_share_session(None, State(state.clone()), Json(CreateShareSessionRequest {
            invite_secret: "dummy-invite".to_owned(), invite_expires_at: None,
            passphrase_hash: hash, trusted_devices: Vec::new(),
        })).await.unwrap().0;
        (state, created)
    }
    fn join_request(passphrase: Option<&str>) -> JoinShareSessionRequest {
        JoinShareSessionRequest { display_name: "Dummy".to_owned(), invite_secret: "dummy-invite".to_owned(),
            device_id: "dummy-device".to_owned(), passphrase: passphrase.map(str::to_owned) }
    }
    async fn attach_dummy(state: &BrokerState, id: ShareSessionId, guest_id: GuestId) -> (Uuid, mpsc::Receiver<Message>) {
        let connection_id = Uuid::new_v4();
        let (tx, rx) = mpsc::channel(8);
        let mut guard = state.inner.lock().await;
        let guest = guard.sessions.get_mut(&id).unwrap().guests.get_mut(&guest_id).unwrap();
        guest.tx = Some(tx);
        guest.connection_id = Some(connection_id);
        guest.disconnected_at = None;
        guest.mark_connected();
        (connection_id, rx)
    }
    #[tokio::test]
    async fn pending_reconnection_does_not_authorize_either_relay_direction() {
        let (state, created) = session(None).await;
        let join = join_share_session(State(state.clone()), Path(created.session_id.0), Json(join_request(None))).await.unwrap().0;
        let (old, _old_rx) = attach_dummy(&state, created.session_id, join.guest_id).await;
        handle_disconnect(&state, created.session_id, StreamAuth::Guest(join.guest_id), old).await;
        let (current, mut guest_rx) = attach_dummy(&state, created.session_id, join.guest_id).await;
        let (host_tx, mut host_rx) = mpsc::channel(8);
        let host_id = Uuid::new_v4();
        {
            let mut guard = state.inner.lock().await;
            let session = guard.sessions.get_mut(&created.session_id).unwrap();
            session.host_tx = Some(host_tx);
            session.host_connection_id = Some(host_id);
            assert!(!session.guests.get(&join.guest_id).unwrap().can_relay());
        }
        relay_payload(&state, created.session_id, StreamAuth::Host, host_id, Message::Binary(b"dummy".to_vec())).await;
        relay_payload(&state, created.session_id, StreamAuth::Guest(join.guest_id), current, Message::Binary(b"dummy".to_vec())).await;
        assert!(guest_rx.try_recv().is_err());
        assert!(host_rx.try_recv().is_err());
    }
    #[tokio::test]
    async fn approval_survives_reconnect_but_denial_does_not_become_approval() {
        let (state, created) = session(None).await;
        let join = join_share_session(State(state.clone()), Path(created.session_id.0), Json(join_request(None))).await.unwrap().0;
        approve_join(State(state.clone()), Path(created.session_id.0), Json(JoinDecisionRequest {
            host_token: created.host_token.clone(), guest_id: join.guest_id,
        })).await.unwrap();
        let (connection, _rx) = attach_dummy(&state, created.session_id, join.guest_id).await;
        handle_disconnect(&state, created.session_id, StreamAuth::Guest(join.guest_id), connection).await;
        let (_connection, _rx) = attach_dummy(&state, created.session_id, join.guest_id).await;
        { let guard = state.inner.lock().await; assert!(guard.sessions[&created.session_id].guests[&join.guest_id].can_relay()); }
        deny_join(State(state.clone()), Path(created.session_id.0), Json(JoinDecisionRequest {
            host_token: created.host_token, guest_id: join.guest_id,
        })).await.unwrap();
        let mut guard = state.inner.lock().await;
        let guest = guard.sessions.get_mut(&created.session_id).unwrap().guests.get_mut(&join.guest_id).unwrap();
        guest.mark_disconnected();
        guest.mark_connected();
        assert!(!guest.can_relay());
        assert_eq!(guest.connection_state, GuestConnectionState::Denied);
    }
    #[tokio::test]
    async fn heartbeat_timeout_preserves_pending_authorization() {
        let (state, created) = session(None).await;
        let join = join_share_session(State(state.clone()), Path(created.session_id.0), Json(join_request(None))).await.unwrap().0;
        let (_connection, _rx) = attach_dummy(&state, created.session_id, join.guest_id).await;
        { let mut guard = state.inner.lock().await; guard.sessions.get_mut(&created.session_id).unwrap().host_disconnected_at = None; }
        cleanup_expired_sessions(&state, Instant::now() + HEARTBEAT_TIMEOUT + Duration::from_secs(1)).await;
        let (_connection, _rx) = attach_dummy(&state, created.session_id, join.guest_id).await;
        let guard = state.inner.lock().await;
        let guest = &guard.sessions[&created.session_id].guests[&join.guest_id];
        assert_eq!(guest.connection_state, GuestConnectionState::Pending);
        assert!(!guest.can_relay());
    }
    #[tokio::test]
    async fn invalid_hash_parameters_are_rejected_before_session_creation() {
        let state = BrokerState::new(BrokerConfig { require_loopback_session_creation: false });
        let hash = super::super::auth::hash_passphrase("dummy-password").unwrap().replace("m=19456", "m=19457");
        let result = create_share_session(None, State(state.clone()), Json(CreateShareSessionRequest {
            invite_secret: "dummy".to_owned(), invite_expires_at: None, passphrase_hash: Some(hash), trusted_devices: Vec::new(),
        })).await;
        assert_eq!(result.err().unwrap().0, StatusCode::BAD_REQUEST);
        assert!(state.inner.lock().await.sessions.is_empty());
    }
    #[tokio::test]
    async fn password_work_is_bounded_without_locking_unrelated_sessions() {
        let hash = super::super::auth::hash_passphrase("dummy-password").unwrap();
        let (state, created) = session(Some(hash)).await;
        let permits = state.password_work.clone().try_acquire_many_owned(2).unwrap();
        let result = join_share_session(State(state.clone()), Path(created.session_id.0), Json(join_request(Some("dummy-password")))).await;
        assert_eq!(result.err().unwrap().0, StatusCode::TOO_MANY_REQUESTS);
        assert!(state.inner.try_lock().is_ok());
        let other = create_share_session(None, State(state.clone()), Json(CreateShareSessionRequest {
            invite_secret: "other-dummy".to_owned(), invite_expires_at: None, passphrase_hash: None, trusted_devices: Vec::new(),
        })).await;
        assert!(other.is_ok());
        drop(permits);
        assert!(join_share_session(State(state), Path(created.session_id.0), Json(join_request(Some("dummy-password")))).await.is_ok());
    }
}
''')

# Repository hygiene. Hosted branch protection is a separate administration setting.
ci = '.github/workflows/ci.yml'
edit(ci, 'name: CI\n', 'name: CI\n\npermissions:\n  contents: read\n')
edit(ci, '        uses: actions/checkout@v4\n\n', '        uses: actions/checkout@v4\n        with:\n          persist-credentials: false\n\n', count=2)
edit(ci, '        uses: actions/checkout@v4\n      - name: Verify browser extension', '        uses: actions/checkout@v4\n        with:\n          persist-credentials: false\n      - name: Verify browser extension')
edit(ci, '          fetch-depth: 0', '          fetch-depth: 0\n          persist-credentials: false')
for file in [ci, '.github/workflows/release.yml']:
    p = Path(file)
    text = p.read_text().replace('uses: actions/checkout@v4', 'uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4')
    text = text.replace('uses: dtolnay/rust-toolchain@master', 'uses: dtolnay/rust-toolchain@02cb101ec7c40f2c49e1d9714d64511d8e1b74de # master')
    p.write_text(text)
append('.gitignore', '''
# Local secrets and generated diagnostic data
.env
.env.*
!.env.example
!.env.sample
*.p12
*.pfx
*.key
credentials.json
terminalcanvas-diagnostics-*.zip
''')
append('extension/README.md', '''
## Revisión antes de usar una captura

Las capturas se muestran en una ventana de revisión dentro de TerminalCanvas.
El botón **Copiar texto** copia el contenido para pegarlo en el agente elegido.
No se envía texto automáticamente a un terminal y no se pulsa Enter. Cerrar o
cambiar de agente no puede redirigir una captura pendiente a un shell.
Las capturas contienen datos de páginas externas; revisalos antes de usarlos.
''')
Path('SECURITY.md').write_text('''# Security

TerminalCanvas executes real local terminals. Remote terminal control is an
explicit grant of the host OS account's authority, not a sandbox.

Keep invitation codes private, use a separate session passphrase, grant control
only to trusted participants, and close sharing when it is no longer needed.
Browser captures are untrusted data and require review and explicit copying;
they are never automatically submitted to a terminal.

Configuration and backups are created with private Unix permissions. On Windows,
store them in the standard per-user profile with a private inherited ACL.
Do not post config files, invitation codes, logs or diagnostic archives publicly
without reviewing their contents.

## Reporting

Use GitHub private vulnerability reporting when the repository exposes that
option. Otherwise contact the maintainer privately before publishing sensitive
reproduction details. This file does not assert that private reporting is enabled.

## Maintainer controls

Protect the default branch, require the CI checks, block force pushes and deletion,
and protect release tags. These are GitHub administration settings, not controls
that can be enabled by committing this file. Keep dependency advisory scans enabled.
''')
Path('.github/workflows/security-remediation.yml').unlink()
Path('scripts/security-remediate-once.py').unlink()
print('Applied C1-C6 source changes and security regression tests.')
