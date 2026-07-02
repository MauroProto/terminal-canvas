use super::*;
use egui::{pos2, Color32, FontId, Response};

const DIALOG_INPUT_BG: Color32 = palette::SURFACE;

fn dialog_frame() -> egui::Frame {
    egui::Frame::default()
        .fill(palette::INK)
        .stroke(egui::Stroke::new(1.0, palette::LINE))
        .rounding(8.0)
        .inner_margin(egui::Margin::same(22.0))
}

fn dialog_backdrop(ctx: &egui::Context) {
    Area::new(Id::new("dialog-backdrop"))
        .order(Order::Middle)
        .fixed_pos(pos2(0.0, 0.0))
        .show(ctx, |ui| {
            let rect = ctx.screen_rect();
            ui.painter()
                .rect_filled(rect, 0.0, Color32::from_rgba_premultiplied(0, 0, 0, 170));
        });
}

fn dialog_title(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(15.0)
            .color(palette::TEXT_STRONG),
    );
}

fn dialog_subtitle(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
    ui.label(egui::RichText::new(text).size(11.5).color(palette::DIM));
}

fn dialog_field_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(14.0);
    ui.label(egui::RichText::new(text).size(11.0).color(palette::TEXT));
    ui.add_space(4.0);
}

fn dialog_help_text(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
    ui.label(egui::RichText::new(text).size(10.5).color(palette::DIM));
}

fn dialog_input_visuals(ui: &mut egui::Ui) {
    let visuals = ui.visuals_mut();
    visuals.extreme_bg_color = DIALOG_INPUT_BG;
    visuals.widgets.inactive.bg_fill = DIALOG_INPUT_BG;
    visuals.widgets.hovered.bg_fill = DIALOG_INPUT_BG;
    visuals.widgets.active.bg_fill = DIALOG_INPUT_BG;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, palette::LINE);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, palette::DIM);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, palette::TEXT);
    visuals.widgets.inactive.fg_stroke.color = palette::TEXT;
    visuals.widgets.hovered.fg_stroke.color = palette::TEXT_STRONG;
    visuals.widgets.active.fg_stroke.color = palette::TEXT_STRONG;
    visuals.selection.bg_fill = palette::FOCUS;
    visuals.selection.stroke = egui::Stroke::new(1.0, palette::TEXT_STRONG);
}

fn dialog_singleline(ui: &mut egui::Ui, value: &mut String, password: bool) -> Response {
    dialog_input_visuals(ui);
    let mut edit = egui::TextEdit::singleline(value)
        .margin(egui::Margin::symmetric(10.0, 6.0))
        .text_color(palette::TEXT_STRONG)
        .desired_width(f32::INFINITY);
    if password {
        edit = edit.password(true);
    }
    ui.add_sized(egui::vec2(ui.available_width(), 30.0), edit)
}

fn dialog_multiline(ui: &mut egui::Ui, value: &mut String, height: f32) -> Response {
    dialog_input_visuals(ui);
    let edit = egui::TextEdit::multiline(value)
        .margin(egui::Margin::symmetric(10.0, 6.0))
        .text_color(palette::TEXT_STRONG)
        .desired_width(f32::INFINITY);
    ui.add_sized(egui::vec2(ui.available_width(), height), edit)
}

fn dialog_link(ui: &mut egui::Ui, label: &str, primary: bool) -> bool {
    let font = FontId::proportional(12.0);
    let text_w = ui.fonts(|f| {
        f.layout_no_wrap(label.to_owned(), font.clone(), palette::TEXT)
            .size()
            .x
    });
    let pad_x = 14.0;
    let height = 30.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(text_w + pad_x * 2.0, height),
        egui::Sense::click(),
    );
    let hovered = response.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 4.0, palette::RAISED);
    }
    let color = if primary {
        palette::TEXT_STRONG
    } else if hovered {
        palette::TEXT
    } else {
        palette::DIM
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        color,
    );
    if primary {
        let underline_y = rect.center().y + 8.0;
        let half = text_w * 0.5;
        ui.painter().line_segment(
            [
                pos2(rect.center().x - half, underline_y),
                pos2(rect.center().x + half, underline_y),
            ],
            egui::Stroke::new(1.0, palette::TEXT_STRONG),
        );
    }
    response.clicked()
}

fn dialog_button_row(ui: &mut egui::Ui, secondary: &str, primary: &str) -> (bool, bool) {
    ui.add_space(20.0);
    let mut sec = false;
    let mut pri = false;
    ui.horizontal(|ui| {
        ui.style_mut().spacing.item_spacing.x = 4.0;
        if dialog_link(ui, secondary, false) {
            sec = true;
        }
        if dialog_link(ui, primary, true) {
            pri = true;
        }
    });
    (sec, pri)
}

fn dialog_inline_link(ui: &mut egui::Ui, label: &str) -> bool {
    let font = FontId::proportional(11.0);
    let text_w = ui.fonts(|f| {
        f.layout_no_wrap(label.to_owned(), font.clone(), palette::TEXT)
            .size()
            .x
    });
    let pad_x = 10.0;
    let height = 24.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(text_w + pad_x * 2.0, height),
        egui::Sense::click(),
    );
    let hovered = response.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 4.0, palette::RAISED);
    }
    let color = if hovered {
        palette::TEXT_STRONG
    } else {
        palette::TEXT
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        color,
    );
    response.clicked()
}

impl TerminalApp {
    pub(super) fn show_rename_dialog(&mut self, ctx: &egui::Context) {
        let Some(panel_id) = self.renaming_panel else {
            return;
        };
        dialog_backdrop(ctx);
        let mut close = false;
        let mut save = false;
        Area::new(Id::new("rename-dialog"))
            .order(Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                dialog_frame().show(ui, |ui| {
                    ui.set_min_width(360.0);
                    dialog_title(ui, "Rename terminal");
                    dialog_field_label(ui, "Title");
                    let response = dialog_singleline(ui, &mut self.rename_buf, false);
                    if response.lost_focus()
                        && ui.input(|i: &egui::InputState| i.key_pressed(Key::Enter))
                    {
                        save = true;
                    }
                    let (cancel, confirm) = dialog_button_row(ui, "Cancel", "Save");
                    if cancel {
                        close = true;
                    }
                    if confirm {
                        save = true;
                    }
                });
            });
        if save {
            let title = self.rename_buf.clone();
            self.ws_mut().rename_panel(panel_id, title);
            self.renaming_panel = None;
        } else if close {
            self.renaming_panel = None;
        }
    }

    pub(super) fn show_launch_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.launch_agent.clone() else {
            return;
        };
        dialog_backdrop(ctx);
        let mut cancel = false;
        let mut submit = false;
        Area::new(Id::new("launch-agent-dialog"))
            .order(Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                dialog_frame().show(ui, |ui| {
                    ui.set_min_width(420.0);
                    dialog_title(ui, "Launch agent");
                    dialog_field_label(ui, "Provider");
                    egui::ComboBox::from_id_salt("launch-agent-provider")
                        .selected_text(draft.provider.label())
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for provider in crate::orchestration::launch_presets() {
                                ui.selectable_value(
                                    &mut draft.provider,
                                    provider,
                                    provider.label(),
                                );
                            }
                        });
                    dialog_field_label(ui, "Task");
                    dialog_singleline(ui, &mut draft.task_title, false);
                    dialog_field_label(ui, "Brief");
                    dialog_multiline(ui, &mut draft.brief, 80.0);
                    dialog_field_label(ui, "Repo mode");
                    egui::ComboBox::from_id_salt("launch-agent-worktree")
                        .selected_text(match draft.worktree_mode {
                            WorktreeMode::Auto => "Worktree per agent",
                            WorktreeMode::SharedRepo => "Shared repo",
                        })
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut draft.worktree_mode,
                                WorktreeMode::Auto,
                                "Worktree per agent",
                            );
                            ui.selectable_value(
                                &mut draft.worktree_mode,
                                WorktreeMode::SharedRepo,
                                "Shared repo",
                            );
                        });
                    if let Some(error) = &draft.error {
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(error)
                                .size(11.0)
                                .color(palette::TEXT_STRONG),
                        );
                    }
                    if draft.pending_session.is_some() {
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new("Creando worktree…")
                                .size(11.0)
                                .color(palette::DIM),
                        );
                    }
                    let (sec, pri) = dialog_button_row(ui, "Cancel", "Launch");
                    if sec {
                        cancel = true;
                    }
                    if pri && draft.pending_session.is_none() {
                        submit = true;
                    }
                });
            });
        if cancel {
            self.launch_agent = None;
        } else {
            self.launch_agent = Some(draft);
            if submit {
                self.submit_launch_agent(ctx);
            }
        }
    }

    pub(super) fn show_share_workspace_dialog(&mut self, ctx: &egui::Context) {
        if !self.share_workspace_open {
            return;
        }
        dialog_backdrop(ctx);
        let mut close = false;
        Area::new(Id::new("share-workspace-dialog"))
            .order(Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                dialog_frame().show(ui, |ui| {
                    ui.set_min_width(480.0);
                    dialog_title(ui, "Share workspace");
                    match self.collab.mode() {
                        CollabMode::Inactive => {
                            dialog_subtitle(
                                ui,
                                "Trusted Live comparte el workspace actual directo desde esta máquina del host.",
                            );
                            dialog_field_label(ui, "Reachable URL");
                            dialog_singleline(
                                ui,
                                &mut self.share_workspace_draft.broker_url,
                                false,
                            );
                            dialog_help_text(
                                ui,
                                "Usá la IP o dominio al que se van a conectar los invitados. Para acceso desde otra red, esa URL tiene que apuntar a tu máquina y puerto. El modo directo usa HTTPS/WSS con certificado pinneado en el invite.",
                            );
                            dialog_field_label(ui, "Session passphrase (optional)");
                            dialog_singleline(
                                ui,
                                &mut self.share_workspace_draft.session_passphrase,
                                true,
                            );
                            dialog_help_text(
                                ui,
                                "Si la ponés, no viaja en el invite code. La compartís por separado y se pide al entrar.",
                            );
                            ui.add_space(14.0);
                            ui.checkbox(
                                &mut self.share_workspace_draft.acknowledge_trusted_live,
                                "Entiendo que Trusted Live da acceso a terminales reales del host.",
                            );
                            dialog_help_text(
                                ui,
                                "No es sandbox. Un invitado aprobado puede ejecutar comandos reales en tu máquina dentro de esa terminal.",
                            );
                            if let Some(error) = &self.share_workspace_draft.error {
                                ui.add_space(10.0);
                                ui.label(
                                    egui::RichText::new(error)
                                        .size(11.0)
                                        .color(palette::TEXT_STRONG),
                                );
                            }
                            let (sec, pri) = dialog_button_row(ui, "Cancel", "Start sharing");
                            if sec {
                                close = true;
                            }
                            if pri {
                                self.start_share_workspace();
                            }
                        }
                        CollabMode::Host => {
                            let workspace_name = self
                                .shared_workspace()
                                .map(|workspace| workspace.name.as_str())
                                .unwrap_or("Workspace");
                            let (state_label, state_color) =
                                collab_state_badge(self.collab.session_state());
                            dialog_subtitle(ui, &format!("Workspace: {workspace_name}"));
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(state_label)
                                    .size(11.0)
                                    .color(state_color),
                            );
                            if let Some(expires_at) = self.collab.invite_expires_at() {
                                dialog_field_label(
                                    ui,
                                    &format!(
                                        "Invite expires: {} UTC",
                                        expires_at.format("%Y-%m-%d %H:%M")
                                    ),
                                );
                                if dialog_inline_link(ui, "Rotate invite") {
                                    // Corre en el worker HTTP; un fallo se
                                    // muestra vía collab.last_error().
                                    self.collab.rotate_invite();
                                    self.share_workspace_draft.error = None;
                                }
                            }
                            if let Some(invite) = self.collab.invite_code().map(str::to_owned) {
                                dialog_field_label(ui, "Invite code");
                                let mut invite_text = invite.clone();
                                dialog_input_visuals(ui);
                                ui.add_sized(
                                    egui::vec2(ui.available_width(), 60.0),
                                    egui::TextEdit::multiline(&mut invite_text)
                                        .interactive(false)
                                        .text_color(palette::TEXT_STRONG)
                                        .margin(egui::Margin::symmetric(10.0, 6.0)),
                                );
                                ui.add_space(6.0);
                                if dialog_inline_link(ui, "Copy invite") {
                                    ctx.copy_text(invite);
                                }
                            }
                            if !self
                                .share_workspace_draft
                                .session_passphrase
                                .trim()
                                .is_empty()
                            {
                                dialog_help_text(
                                    ui,
                                    "Esta sesión también requiere la passphrase que configuraste.",
                                );
                            }
                            if let Some(error) = self.collab.last_error() {
                                ui.add_space(10.0);
                                ui.label(
                                    egui::RichText::new(error)
                                        .size(11.0)
                                        .color(palette::TEXT_STRONG),
                                );
                            }

                            let pending_joins = self.collab.pending_joins().to_vec();
                            if !pending_joins.is_empty() {
                                dialog_field_label(ui, "Pending joins");
                                for request in pending_joins {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(&request.display_name)
                                                .size(11.5)
                                                .color(palette::TEXT_STRONG),
                                        );
                                        ui.add_space(8.0);
                                        if dialog_inline_link(ui, "Approve") {
                                            self.collab.approve_join(request.guest_id);
                                        }
                                        if dialog_inline_link(ui, "Trust device") {
                                            self.remember_trusted_device(
                                                &request.device_id,
                                                &request.display_name,
                                            );
                                            self.collab.approve_join(request.guest_id);
                                        }
                                        if dialog_inline_link(ui, "Deny") {
                                            self.collab.deny_join(request.guest_id);
                                        }
                                    });
                                }
                            }

                            let pending_controls = self.collab.pending_control_requests().to_vec();
                            if !pending_controls.is_empty() {
                                dialog_field_label(ui, "Control requests");
                                for request in pending_controls {
                                    let panel_info = self
                                        .shared_workspace()
                                        .and_then(|workspace| {
                                            workspace.panel(request.terminal_id)
                                        })
                                        .map(|panel| {
                                            (panel.title().to_owned(), panel.share_scope())
                                        });
                                    let (panel_title, share_scope) = panel_info.unwrap_or_else(
                                        || ("Terminal".to_owned(), PanelShareScope::Private),
                                    );
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} → {}",
                                                request.display_name, panel_title
                                            ))
                                            .size(11.5)
                                            .color(palette::TEXT),
                                        );
                                        if !share_scope.allows_control() {
                                            ui.label(
                                                egui::RichText::new("not controllable")
                                                    .size(11.0)
                                                    .color(palette::DIM),
                                            );
                                        } else if dialog_inline_link(ui, "Grant") {
                                            self.collab.grant_control(
                                                request.terminal_id,
                                                request.guest_id,
                                            );
                                        }
                                    });
                                }
                            }

                            let guests = self.collab.guests();
                            if !guests.is_empty() {
                                dialog_field_label(ui, "Guests");
                                for guest in guests {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} · {:?}",
                                            guest.display_name, guest.connection_state
                                        ))
                                        .size(11.0)
                                        .color(palette::TEXT),
                                    );
                                }
                            }

                            if let Some(workspace) = self.shared_workspace() {
                                let controlled = workspace
                                    .panels
                                    .iter()
                                    .filter_map(|panel| {
                                        let guest_id = self.collab.controller_for(panel.id())?;
                                        Some((panel.id(), panel.title().to_owned(), guest_id))
                                    })
                                    .collect::<Vec<_>>();
                                if !controlled.is_empty() {
                                    dialog_field_label(ui, "Live terminals");
                                    for (panel_id, title, guest_id) in controlled {
                                        let guest_name = self
                                            .collab
                                            .guests()
                                            .into_iter()
                                            .find(|guest| guest.id == guest_id)
                                            .map(|guest| guest.display_name)
                                            .unwrap_or_else(|| "Guest".to_owned());
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{title} · {guest_name}"
                                                ))
                                                .size(11.5)
                                                .color(palette::TEXT),
                                            );
                                            if dialog_inline_link(ui, "Revoke") {
                                                self.collab
                                                    .revoke_control(panel_id, "Revoked by host");
                                            }
                                        });
                                    }
                                }
                            }

                            let (sec, pri) = dialog_button_row(ui, "Close", "Stop sharing");
                            if sec {
                                close = true;
                            }
                            if pri {
                                self.collab.stop_session();
                                close = true;
                            }
                        }
                        CollabMode::Guest => {}
                    }
                });
            });

        if close {
            self.share_workspace_open = false;
        }
    }

    pub(super) fn show_join_session_dialog(&mut self, ctx: &egui::Context) {
        if !self.join_session_open {
            return;
        }
        dialog_backdrop(ctx);
        let mut close = false;
        Area::new(Id::new("join-shared-session-dialog"))
            .order(Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                dialog_frame().show(ui, |ui| {
                    ui.set_min_width(460.0);
                    match self.collab.mode() {
                        CollabMode::Guest => {
                            dialog_title(ui, "Shared session");
                            let (state_label, state_color) =
                                collab_state_badge(self.collab.session_state());
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(state_label)
                                    .size(11.0)
                                    .color(state_color),
                            );
                            if let Some(snapshot) = &self.collab.guest_view().snapshot {
                                dialog_field_label(
                                    ui,
                                    &format!("Workspace: {}", snapshot.workspace_name),
                                );
                                ui.add_space(2.0);
                                ui.label(
                                    egui::RichText::new("Participants")
                                        .size(11.0)
                                        .color(palette::DIM),
                                );
                                for guest in &snapshot.guests {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} · {:?}",
                                            guest.display_name, guest.connection_state
                                        ))
                                        .size(11.0)
                                        .color(palette::TEXT),
                                    );
                                }
                                let my_guest_id = self.collab.guest_view().my_guest_id;
                                let controlled_panels = snapshot
                                    .panels
                                    .iter()
                                    .filter(|panel| panel.controller == my_guest_id)
                                    .map(|panel| (panel.panel_id, panel.title.clone()))
                                    .collect::<Vec<_>>();
                                if !controlled_panels.is_empty() {
                                    dialog_field_label(ui, "Your terminals");
                                    for (panel_id, title) in controlled_panels {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(title)
                                                    .size(11.5)
                                                    .color(palette::TEXT),
                                            );
                                            if dialog_inline_link(ui, "Release") {
                                                self.collab.release_control(panel_id);
                                            }
                                        });
                                    }
                                }
                            }
                            if let Some(error) = self.collab.last_error() {
                                ui.add_space(10.0);
                                ui.label(
                                    egui::RichText::new(error)
                                        .size(11.0)
                                        .color(palette::TEXT_STRONG),
                                );
                            }
                            let (sec, pri) = dialog_button_row(ui, "Close", "Leave session");
                            if sec {
                                close = true;
                            }
                            if pri {
                                self.collab.stop_session();
                                close = true;
                            }
                        }
                        _ => {
                            dialog_title(ui, "Join shared session");
                            dialog_subtitle(
                                ui,
                                "Entrás al workspace compartido desde la misma app. Si el host te aprueba, ves el desktop remoto con sus paneles y podés pedir control de una terminal.",
                            );
                            dialog_field_label(ui, "Display name");
                            dialog_singleline(
                                ui,
                                &mut self.join_session_draft.display_name,
                                false,
                            );
                            dialog_field_label(ui, "Invite code");
                            dialog_multiline(ui, &mut self.join_session_draft.invite_code, 90.0);
                            dialog_field_label(ui, "Session passphrase (if required)");
                            dialog_singleline(
                                ui,
                                &mut self.join_session_draft.session_passphrase,
                                true,
                            );
                            if let Some(error) = &self.join_session_draft.error {
                                ui.add_space(10.0);
                                ui.label(
                                    egui::RichText::new(error)
                                        .size(11.0)
                                        .color(palette::TEXT_STRONG),
                                );
                            }
                            if self.join_session_draft.submitting {
                                ui.add_space(10.0);
                                ui.label(
                                    egui::RichText::new("Conectando…")
                                        .size(11.0)
                                        .color(palette::DIM),
                                );
                            }
                            let (sec, pri) = dialog_button_row(ui, "Cancel", "Join");
                            if sec {
                                close = true;
                            }
                            if pri && !self.join_session_draft.submitting {
                                self.submit_join_session();
                            }
                        }
                    }
                });
            });

        if close {
            self.join_session_open = false;
        }
    }
}

pub(super) fn collab_state_badge(state: CollabSessionState) -> (&'static str, Color32) {
    match state {
        CollabSessionState::NotSharing => ("Not sharing", palette::DIM),
        CollabSessionState::Starting => ("Starting", palette::TEXT),
        CollabSessionState::Live => ("Sharing live", palette::TEXT_STRONG),
        CollabSessionState::Disconnected => ("Connection issue", palette::TEXT_STRONG),
        CollabSessionState::Ended => ("Ended", palette::DIM),
    }
}
