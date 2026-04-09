use super::*;

impl TerminalApp {
    pub(super) fn show_rename_dialog(&mut self, ctx: &egui::Context) {
        let Some(panel_id) = self.renaming_panel else {
            return;
        };
        Area::new(Id::new("rename-dialog"))
            .order(Order::Debug)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(Color32::from_rgb(24, 24, 28))
                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(55, 55, 65)))
                    .rounding(10.0)
                    .inner_margin(egui::Margin::same(12.0))
                    .show(ui, |ui: &mut egui::Ui| {
                        ui.label("Rename terminal");
                        let response = ui.add_sized(
                            vec2(280.0, 28.0),
                            egui::TextEdit::singleline(&mut self.rename_buf),
                        );
                        let confirm = response.lost_focus()
                            && ui.input(|i: &egui::InputState| i.key_pressed(Key::Enter));
                        ui.horizontal(|ui: &mut egui::Ui| {
                            if ui.button("Cancel").clicked() {
                                self.renaming_panel = None;
                            }
                            if ui.button("Save").clicked() || confirm {
                                let title = self.rename_buf.clone();
                                self.ws_mut().rename_panel(panel_id, title);
                                self.renaming_panel = None;
                            }
                        });
                    });
            });
    }

    pub(super) fn show_launch_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.launch_agent.clone() else {
            return;
        };
        let mut cancel = false;
        let mut submit = false;
        Area::new(Id::new("launch-agent-dialog"))
            .order(Order::Debug)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(Color32::from_rgb(24, 24, 28))
                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(55, 55, 65)))
                    .rounding(12.0)
                    .inner_margin(egui::Margin::same(12.0))
                    .show(ui, |ui: &mut egui::Ui| {
                        ui.set_min_width(360.0);
                        ui.label("Launch agent");
                        egui::ComboBox::from_id_salt("launch-agent-provider")
                            .selected_text(draft.provider.label())
                            .show_ui(ui, |ui| {
                                for provider in crate::orchestration::launch_presets() {
                                    ui.selectable_value(
                                        &mut draft.provider,
                                        provider,
                                        provider.label(),
                                    );
                                }
                            });
                        ui.add_space(6.0);
                        ui.label("Task");
                        ui.add_sized(
                            vec2(336.0, 28.0),
                            egui::TextEdit::singleline(&mut draft.task_title)
                                .hint_text("Short task title"),
                        );
                        ui.add_space(6.0);
                        ui.label("Brief");
                        ui.add_sized(
                            vec2(336.0, 58.0),
                            egui::TextEdit::multiline(&mut draft.brief)
                                .hint_text("What should this agent do?"),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label("Repo mode");
                            egui::ComboBox::from_id_salt("launch-agent-worktree")
                                .selected_text(match draft.worktree_mode {
                                    WorktreeMode::Auto => "Worktree per agent",
                                    WorktreeMode::SharedRepo => "Shared repo",
                                })
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
                        });
                        if let Some(error) = &draft.error {
                            ui.add_space(6.0);
                            ui.colored_label(Color32::from_rgb(239, 68, 68), error);
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                cancel = true;
                            }
                            if ui.button("Launch").clicked() {
                                submit = true;
                            }
                        });
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

        let mut close = false;
        Area::new(Id::new("share-workspace-dialog"))
            .order(Order::Debug)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(Color32::from_rgb(24, 24, 28))
                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(58, 58, 66)))
                    .rounding(14.0)
                    .inner_margin(egui::Margin::same(14.0))
                    .show(ui, |ui| {
                        ui.set_min_width(460.0);
                        ui.heading("Share Workspace");
                        ui.add_space(6.0);
                        match self.collab.mode() {
                            CollabMode::Inactive => {
                                ui.label(
                                    egui::RichText::new(
                                        "Trusted Live comparte el workspace actual directo desde esta máquina del host.",
                                    )
                                    .color(Color32::from_rgb(170, 170, 176)),
                                );
                                ui.add_space(8.0);
                                ui.label("Reachable URL");
                                ui.add_sized(
                                    vec2(420.0, 28.0),
                                    egui::TextEdit::singleline(
                                        &mut self.share_workspace_draft.broker_url,
                                    ),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Usá la IP o dominio al que se van a conectar los invitados. Para acceso desde otra red, esa URL tiene que apuntar a tu máquina y puerto. El modo directo ahora usa HTTPS/WSS con certificado pinneado en el invite.",
                                    )
                                    .size(12.0)
                                    .color(Color32::from_rgb(152, 152, 160)),
                                );
                                ui.add_space(8.0);
                                ui.label("Session passphrase (optional)");
                                ui.add_sized(
                                    vec2(420.0, 28.0),
                                    egui::TextEdit::singleline(
                                        &mut self.share_workspace_draft.session_passphrase,
                                    )
                                    .password(true),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Si la ponés, no viaja en el invite code. La compartís por separado y se pide al entrar.",
                                    )
                                    .size(12.0)
                                    .color(Color32::from_rgb(152, 152, 160)),
                                );
                                ui.add_space(8.0);
                                ui.checkbox(
                                    &mut self.share_workspace_draft.acknowledge_trusted_live,
                                    "Entiendo que Trusted Live da acceso a terminales reales del host.",
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "No es sandbox. Un invitado aprobado puede ejecutar comandos reales en tu máquina dentro de esa terminal.",
                                    )
                                    .color(Color32::from_rgb(196, 162, 88)),
                                );
                                if let Some(error) = &self.share_workspace_draft.error {
                                    ui.add_space(6.0);
                                    ui.colored_label(Color32::from_rgb(239, 68, 68), error);
                                }
                                ui.add_space(10.0);
                                ui.horizontal(|ui| {
                                    if ui.button("Cancel").clicked() {
                                        close = true;
                                    }
                                    if ui.button("Start sharing").clicked() {
                                        self.start_share_workspace();
                                    }
                                });
                            }
                            CollabMode::Host => {
                                let workspace_name = self
                                    .shared_workspace()
                                    .map(|workspace| workspace.name.as_str())
                                    .unwrap_or("Workspace");
                                let (state_label, state_color) =
                                    collab_state_badge(self.collab.session_state());
                                ui.label(format!("Workspace: {workspace_name}"));
                                ui.colored_label(state_color, state_label);
                                ui.add_space(8.0);
                                if let Some(expires_at) = self.collab.invite_expires_at() {
                                    ui.horizontal(|ui| {
                                        ui.label(format!(
                                            "Expires: {} UTC",
                                            expires_at.format("%Y-%m-%d %H:%M")
                                        ));
                                        if ui.button("Rotate invite").clicked() {
                                            if let Err(err) = self.collab.rotate_invite() {
                                                self.share_workspace_draft.error =
                                                    Some(err.to_string());
                                            } else {
                                                self.share_workspace_draft.error = None;
                                            }
                                        }
                                    });
                                    ui.add_space(8.0);
                                }
                                if let Some(invite) = self.collab.invite_code().map(str::to_owned) {
                                    ui.label("Invite code");
                                    let mut invite_text = invite.clone();
                                    ui.add_sized(
                                        vec2(420.0, 56.0),
                                        egui::TextEdit::multiline(&mut invite_text)
                                            .interactive(false),
                                    );
                                    if ui.button("Copy invite").clicked() {
                                        ctx.copy_text(invite);
                                    }
                                }
                                if !self.share_workspace_draft.session_passphrase.trim().is_empty() {
                                    ui.label(
                                        egui::RichText::new(
                                            "Esta sesión también requiere la passphrase que configuraste.",
                                        )
                                        .size(12.0)
                                        .color(Color32::from_rgb(152, 152, 160)),
                                    );
                                }
                                if let Some(error) = self.collab.last_error() {
                                    ui.add_space(6.0);
                                    ui.colored_label(Color32::from_rgb(239, 68, 68), error);
                                }

                                let pending_joins = self.collab.pending_joins().to_vec();
                                if !pending_joins.is_empty() {
                                    ui.add_space(10.0);
                                    ui.label("Pending joins");
                                    for request in pending_joins {
                                        ui.horizontal(|ui| {
                                            ui.label(request.display_name.clone());
                                            if ui.button("Approve").clicked() {
                                                let _ = self.collab.approve_join(request.guest_id);
                                            }
                                            if ui.button("Trust device").clicked() {
                                                self.remember_trusted_device(
                                                    &request.device_id,
                                                    &request.display_name,
                                                );
                                                let _ = self.collab.approve_join(request.guest_id);
                                            }
                                            if ui.button("Deny").clicked() {
                                                let _ = self.collab.deny_join(request.guest_id);
                                            }
                                        });
                                    }
                                }

                                let pending_controls = self.collab.pending_control_requests().to_vec();
                                if !pending_controls.is_empty() {
                                    ui.add_space(10.0);
                                    ui.label("Control requests");
                                    for request in pending_controls {
                                        let panel_title = self
                                            .shared_workspace()
                                            .and_then(|workspace| workspace.panel(request.terminal_id))
                                            .map(|panel| panel.title().to_owned())
                                            .unwrap_or_else(|| "Terminal".to_owned());
                                        ui.horizontal(|ui| {
                                            ui.label(format!("{} -> {}", request.display_name, panel_title));
                                            if ui.button("Grant").clicked() {
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
                                    ui.add_space(10.0);
                                    ui.label("Guests");
                                    for guest in guests {
                                        ui.label(format!(
                                            "{} · {:?}",
                                            guest.display_name, guest.connection_state
                                        ));
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
                                        ui.add_space(10.0);
                                        ui.label("Live terminals");
                                        for (panel_id, title, guest_id) in controlled {
                                            let guest_name = self
                                                .collab
                                                .guests()
                                                .into_iter()
                                                .find(|guest| guest.id == guest_id)
                                                .map(|guest| guest.display_name)
                                                .unwrap_or_else(|| "Guest".to_owned());
                                            ui.horizontal(|ui| {
                                                ui.label(format!("{title} · {guest_name}"));
                                                if ui.button("Revoke").clicked() {
                                                    self.collab.revoke_control(
                                                        panel_id,
                                                        "Revoked by host",
                                                    );
                                                }
                                            });
                                        }
                                    }
                                }

                                ui.add_space(10.0);
                                ui.horizontal(|ui| {
                                    if ui.button("Close").clicked() {
                                        close = true;
                                    }
                                    if ui.button("Stop sharing").clicked() {
                                        self.collab.stop_session();
                                        close = true;
                                    }
                                });
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

        let mut close = false;
        Area::new(Id::new("join-shared-session-dialog"))
            .order(Order::Debug)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(Color32::from_rgb(24, 24, 28))
                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(58, 58, 66)))
                    .rounding(14.0)
                    .inner_margin(egui::Margin::same(14.0))
                    .show(ui, |ui| {
                        ui.set_min_width(440.0);
                        match self.collab.mode() {
                            CollabMode::Guest => {
                                ui.heading("Shared Session");
                                let (state_label, state_color) =
                                    collab_state_badge(self.collab.session_state());
                                ui.colored_label(state_color, state_label);
                                if let Some(snapshot) = &self.collab.guest_view().snapshot {
                                    ui.label(format!("Workspace: {}", snapshot.workspace_name));
                                    ui.add_space(8.0);
                                    ui.label("Participants");
                                    for guest in &snapshot.guests {
                                        ui.label(format!(
                                            "{} · {:?}",
                                            guest.display_name, guest.connection_state
                                        ));
                                    }
                                    let my_guest_id = self.collab.guest_view().my_guest_id;
                                    let controlled_panels = snapshot
                                        .panels
                                        .iter()
                                        .filter(|panel| panel.controller == my_guest_id)
                                        .map(|panel| (panel.panel_id, panel.title.clone()))
                                        .collect::<Vec<_>>();
                                    if !controlled_panels.is_empty() {
                                        ui.add_space(8.0);
                                        ui.label("Your terminals");
                                        for (panel_id, title) in controlled_panels {
                                            ui.horizontal(|ui| {
                                                ui.label(title);
                                                if ui.button("Release").clicked() {
                                                    self.collab.release_control(panel_id);
                                                }
                                            });
                                        }
                                    }
                                }
                                if let Some(error) = self.collab.last_error() {
                                    ui.add_space(6.0);
                                    ui.colored_label(Color32::from_rgb(239, 68, 68), error);
                                }
                                ui.add_space(10.0);
                                ui.horizontal(|ui| {
                                    if ui.button("Close").clicked() {
                                        close = true;
                                    }
                                    if ui.button("Leave session").clicked() {
                                        self.collab.stop_session();
                                        close = true;
                                    }
                                });
                            }
                            _ => {
                                ui.heading("Join Shared Session");
                                ui.label(
                                    egui::RichText::new(
                                        "Entrás al workspace compartido desde la misma app. Si el host te aprueba, ves el canvas en vivo y podés pedir control de una terminal.",
                                    )
                                    .color(Color32::from_rgb(170, 170, 176)),
                                );
                                ui.add_space(8.0);
                                ui.label("Display name");
                                ui.add_sized(
                                    vec2(400.0, 28.0),
                                    egui::TextEdit::singleline(
                                        &mut self.join_session_draft.display_name,
                                    ),
                                );
                                ui.add_space(8.0);
                                ui.label("Invite code");
                                ui.add_sized(
                                    vec2(400.0, 92.0),
                                    egui::TextEdit::multiline(
                                        &mut self.join_session_draft.invite_code,
                                    ),
                                );
                                ui.add_space(8.0);
                                ui.label("Session passphrase (if required)");
                                ui.add_sized(
                                    vec2(400.0, 28.0),
                                    egui::TextEdit::singleline(
                                        &mut self.join_session_draft.session_passphrase,
                                    )
                                    .password(true),
                                );
                                if let Some(error) = &self.join_session_draft.error {
                                    ui.add_space(6.0);
                                    ui.colored_label(Color32::from_rgb(239, 68, 68), error);
                                }
                                ui.add_space(10.0);
                                ui.horizontal(|ui| {
                                    if ui.button("Cancel").clicked() {
                                        close = true;
                                    }
                                    if ui.button("Join").clicked() {
                                        self.submit_join_session();
                                    }
                                });
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
        CollabSessionState::NotSharing => ("Not sharing", Color32::from_rgb(154, 154, 162)),
        CollabSessionState::Starting => ("Starting", Color32::from_rgb(245, 158, 11)),
        CollabSessionState::Live => ("Sharing live", Color32::from_rgb(76, 201, 124)),
        CollabSessionState::Disconnected => ("Connection issue", Color32::from_rgb(239, 68, 68)),
        CollabSessionState::Ended => ("Ended", Color32::from_rgb(120, 120, 128)),
    }
}
