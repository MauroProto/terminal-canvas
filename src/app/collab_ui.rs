//! Glue de colaboración de la app: drafts de compartir/unirse (con sus flujos
//! asíncronos), publicación de snapshots del host, input remoto de guests y
//! el banner de estado de sesión.

use chrono::Utc;
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Stroke};

use crate::canvas::scene::handle_canvas_input;
use crate::canvas::viewport::Viewport;
use crate::collab::auth::normalize_optional_passphrase;
use crate::collab::{
    bind_addr_for_share_url, draw_remote_workspace, CollabEvent, CollabMode, CollabSessionState,
    HostShareOptions, PanelShareScope, RemotePanelAction, SerializableKey, SerializableModifiers,
    SharedWorkspaceSnapshot, TerminalInputEvent, TrustedDevice,
};
use crate::state::Workspace;
use crate::theme::colors as palette;

use super::desktop::panel_zoom_gesture_active;
use super::TerminalApp;

#[derive(Clone)]
pub(super) struct ShareWorkspaceDraft {
    pub(super) broker_url: String,
    pub(super) session_passphrase: String,
    pub(super) acknowledge_trusted_live: bool,
    pub(super) error: Option<String>,
}

#[derive(Clone)]
pub(super) struct JoinSessionDraft {
    pub(super) invite_code: String,
    pub(super) display_name: String,
    pub(super) session_passphrase: String,
    pub(super) error: Option<String>,
    // El join corre en el worker HTTP de collab; mientras está en vuelo el
    // diálogo queda abierto mostrando progreso.
    pub(super) submitting: bool,
}

impl TerminalApp {
    pub(super) fn trusted_devices_snapshot(&self) -> Vec<TrustedDevice> {
        let mut devices = self.trusted_devices.values().cloned().collect::<Vec<_>>();
        devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        devices
    }

    pub(super) fn remember_trusted_device(&mut self, device_id: &str, display_name: &str) {
        if device_id.trim().is_empty() {
            return;
        }
        let now = Utc::now();
        let entry = self
            .trusted_devices
            .entry(device_id.to_owned())
            .or_insert_with(|| TrustedDevice {
                device_id: device_id.to_owned(),
                last_display_name: display_name.to_owned(),
                approved_at: now,
                last_seen_at: now,
            });
        entry.last_display_name = display_name.to_owned();
        entry.last_seen_at = now;
    }
    pub(super) fn open_share_workspace_dialog(&mut self) {
        self.share_workspace_open = true;
        self.share_workspace_draft.error = None;
        self.share_workspace_draft.broker_url = self.collab.broker_url().to_owned();
    }

    pub(super) fn open_join_session_dialog(&mut self) {
        self.join_session_open = true;
        self.join_session_draft.error = None;
    }
    pub(super) fn start_share_workspace(&mut self) {
        if !self.share_workspace_draft.acknowledge_trusted_live {
            self.share_workspace_draft.error = Some(
                "Tenés que confirmar que Trusted Live usa terminales reales del host.".to_owned(),
            );
            return;
        }
        let session_passphrase =
            normalize_optional_passphrase(&self.share_workspace_draft.session_passphrase);
        let reachable_url = self.share_workspace_draft.broker_url.trim().to_owned();
        let bind_addr = match bind_addr_for_share_url(&reachable_url) {
            Ok(bind_addr) => bind_addr,
            Err(err) => {
                self.share_workspace_draft.error = Some(err.to_string());
                return;
            }
        };
        self.collab.set_broker_url(reachable_url.clone());
        match self.collab.start_host_session(
            self.ws().id,
            HostShareOptions {
                bind_addr,
                reachable_url,
            },
            session_passphrase,
            self.trusted_devices_snapshot(),
        ) {
            Ok(()) => {
                self.share_workspace_draft.error = None;
                self.share_workspace_open = true;
            }
            Err(err) => {
                self.share_workspace_draft.error = Some(err.to_string());
            }
        }
    }

    pub(super) fn submit_join_session(&mut self) {
        let invite_code = self.join_session_draft.invite_code.trim().to_owned();
        let display_name = self.join_session_draft.display_name.trim().to_owned();
        let session_passphrase =
            normalize_optional_passphrase(&self.join_session_draft.session_passphrase);
        if invite_code.is_empty() || display_name.is_empty() {
            self.join_session_draft.error =
                Some("Pegá un invite code y un nombre visible.".to_owned());
            return;
        }
        match self.collab.join_session(
            &invite_code,
            display_name,
            session_passphrase,
            self.local_device_id.clone(),
        ) {
            Ok(()) => {
                // El POST sigue en el worker HTTP; el diálogo queda abierto
                // hasta que poll_join_session_result vea el desenlace.
                self.join_session_draft.error = None;
                self.join_session_draft.submitting = true;
            }
            Err(err) => {
                self.join_session_draft.error = Some(err.to_string());
            }
        }
    }

    pub(super) fn poll_join_session_result(&mut self) {
        if !self.join_session_draft.submitting {
            return;
        }
        if self.collab.mode() == CollabMode::Guest {
            self.join_session_draft.submitting = false;
            self.join_session_draft.error = None;
            self.join_session_open = false;
            self.viewport = Viewport::default();
        } else if !self.collab.join_in_flight() {
            self.join_session_draft.submitting = false;
            self.join_session_draft.error = self
                .collab
                .last_error()
                .map(str::to_owned)
                .or_else(|| Some("No se pudo unir a la sesión.".to_owned()));
        }
    }
    pub(super) fn shared_workspace(&self) -> Option<&Workspace> {
        let workspace_id = self.collab.shared_workspace_id()?;
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
    }

    pub(super) fn build_shared_workspace_snapshot(&self) -> Option<SharedWorkspaceSnapshot> {
        let workspace = self.shared_workspace()?;
        Some(SharedWorkspaceSnapshot {
            workspace_id: workspace.id,
            workspace_name: workspace.name.clone(),
            generated_at: Utc::now(),
            guests: Vec::new(),
            terminal_controls: Vec::new(),
            panels: workspace.shared_panel_snapshots(),
        })
    }

    pub(super) fn publish_collab_snapshot(&mut self) {
        if !matches!(self.collab.mode(), CollabMode::Host) {
            return;
        }
        if let Some(snapshot) = self.build_shared_workspace_snapshot() {
            self.collab.publish_snapshot(snapshot);
        }
    }

    pub(super) fn handle_collab_events(&mut self) {
        let shared_workspace_id = self.collab.shared_workspace_id();
        for event in self.collab.drain_events() {
            match event {
                CollabEvent::RemoteInput { guest_id, input } => {
                    if self.collab.controller_for(input.terminal_id) != Some(guest_id) {
                        continue;
                    }
                    let Some(workspace_id) = shared_workspace_id else {
                        continue;
                    };
                    if let Some(index) = self.workspace_index_by_id(workspace_id) {
                        let allows_control = self.workspaces[index]
                            .panels
                            .iter()
                            .find(|panel| panel.id() == input.terminal_id)
                            .map(|panel| panel.share_scope().allows_control())
                            .unwrap_or(false);
                        if !allows_control {
                            self.collab.revoke_control(
                                input.terminal_id,
                                "Panel is no longer controllable",
                            );
                            continue;
                        }
                        self.workspaces[index].apply_remote_input(input.terminal_id, &input.events);
                    }
                }
            }
        }
    }
    pub(super) fn set_focused_panel_share_scope(&mut self, scope: PanelShareScope) {
        let panel_id = if let Some(panel) = self.ws_mut().focused_panel_mut() {
            let panel_id = panel.id();
            panel.set_share_scope(scope);
            Some(panel_id)
        } else {
            None
        };
        if let Some(panel_id) = panel_id {
            if matches!(self.collab.mode(), CollabMode::Host)
                && !scope.allows_control()
                && self.collab.controller_for(panel_id).is_some()
            {
                self.collab
                    .revoke_control(panel_id, "Panel is no longer controllable");
            }
        }
    }
}

impl TerminalApp {
    /// Canvas en modo guest: dibuja el workspace remoto y rutea scroll/teclado
    /// hacia el host según quién controle cada terminal.
    pub(super) fn show_guest_canvas(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        canvas_rect: Rect,
    ) {
        let guest_snapshot = self.collab.guest_view().snapshot.clone();
        let guest_focused_panel = self.collab.guest_view().focused_panel;
        let guest_id = self.collab.guest_view().my_guest_id;
        let scroll_offsets = self.collab.guest_view().scroll_offsets.clone();
        let canvas_input = handle_canvas_input(ui, &mut self.viewport, canvas_rect, false, false);

        if let Some(snapshot) = guest_snapshot {
            if let Some(action) = draw_remote_workspace(
                ui,
                &snapshot,
                &self.viewport,
                canvas_rect,
                self.collab.session_state(),
                guest_focused_panel,
                guest_id,
                &scroll_offsets,
            ) {
                match action {
                    RemotePanelAction::Focus(panel_id) => self.collab.focus_remote_panel(panel_id),
                    RemotePanelAction::RequestControl(panel_id) => {
                        self.collab.request_control(panel_id)
                    }
                }
            }

            if !matches!(self.collab.session_state(), CollabSessionState::Live) {
                draw_guest_session_banner(ui, canvas_rect, self.collab.session_state());
            }

            let (smooth_scroll_delta, zoom_delta, modifiers) =
                ctx.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers));
            if smooth_scroll_delta.y != 0.0
                && !panel_zoom_gesture_active(smooth_scroll_delta, zoom_delta, modifiers)
            {
                if let Some(panel_id) = guest_focused_panel {
                    let controlled_by_me = snapshot
                        .panels
                        .iter()
                        .find(|panel| panel.panel_id == panel_id)
                        .and_then(|panel| panel.controller)
                        == guest_id;
                    if controlled_by_me {
                        self.collab.send_guest_input(
                            panel_id,
                            vec![TerminalInputEvent::Scroll {
                                delta: smooth_scroll_delta.y,
                            }],
                        );
                    } else {
                        let delta_lines = (smooth_scroll_delta.y.signum()
                            * (smooth_scroll_delta.y.abs() / 24.0).max(1.0))
                            as i32;
                        self.collab.scroll_remote_panel(panel_id, delta_lines);
                    }
                }
            }

            if let Some(panel_id) = guest_focused_panel {
                let controlled_by_me = snapshot
                    .panels
                    .iter()
                    .find(|panel| panel.panel_id == panel_id)
                    .and_then(|panel| panel.controller)
                    == guest_id;
                if controlled_by_me
                    && !self.command_palette.open
                    && !self.join_session_open
                    && !self.share_workspace_open
                {
                    let events = collect_guest_terminal_input(ctx);
                    if !events.is_empty() {
                        self.collab.send_guest_input(panel_id, events);
                    }
                }
            }
        } else {
            ui.painter().text(
                canvas_rect.center(),
                Align2::CENTER_CENTER,
                "Waiting for shared workspace…",
                FontId::proportional(20.0),
                palette::TEXT,
            );
        }

        if canvas_input.navigating {
            ui.ctx().request_repaint();
        }
    }
}

pub(super) fn host_terminal_input_pending(ctx: &egui::Context) -> bool {
    ctx.input(|input| {
        input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Text(_)
                    | egui::Event::Paste(_)
                    | egui::Event::Key { pressed: true, .. }
            )
        })
    })
}

pub(super) fn collect_guest_terminal_input(ctx: &egui::Context) -> Vec<TerminalInputEvent> {
    ctx.input(|input| {
        input
            .events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Text(text) if !input.modifiers.ctrl && !input.modifiers.command => {
                    Some(TerminalInputEvent::Text(text.clone()))
                }
                egui::Event::Paste(text) => Some(TerminalInputEvent::Paste(text.clone())),
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => SerializableKey::from_egui(*key).map(|key| TerminalInputEvent::Key {
                    key,
                    modifiers: SerializableModifiers {
                        ctrl: modifiers.ctrl,
                        alt: modifiers.alt,
                        shift: modifiers.shift,
                        command: modifiers.command,
                    },
                }),
                _ => None,
            })
            .collect()
    })
}

pub(super) fn default_guest_display_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "Guest".to_owned())
}

pub(super) fn draw_guest_session_banner(
    ui: &egui::Ui,
    canvas_rect: Rect,
    session_state: CollabSessionState,
) {
    let (title, body) = match session_state {
        CollabSessionState::Starting => (
            "Connecting to shared workspace",
            "Esperando la aprobación del host o la conexión inicial.",
        ),
        CollabSessionState::Disconnected => (
            "Host temporarily unavailable",
            "La sesión sigue abierta, pero el host perdió la conexión. Vamos a reintentar.",
        ),
        CollabSessionState::Ended => (
            "Shared session ended",
            "El host cerró la sesión o dejó de estar disponible.",
        ),
        _ => return,
    };

    let banner_rect = Rect::from_min_size(
        pos2(canvas_rect.center().x - 190.0, canvas_rect.top() + 20.0),
        vec2(380.0, 62.0),
    );
    let painter = ui.painter();
    painter.rect_filled(
        banner_rect,
        14.0,
        Color32::from_rgba_premultiplied(10, 10, 10, 232),
    );
    painter.rect_stroke(banner_rect, 14.0, Stroke::new(1.0, palette::LINE));
    painter.text(
        banner_rect.left_top() + vec2(14.0, 12.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(16.0),
        palette::TEXT_STRONG,
    );
    painter.text(
        banner_rect.left_top() + vec2(14.0, 34.0),
        Align2::LEFT_TOP,
        body,
        FontId::proportional(12.0),
        palette::TEXT,
    );
}
