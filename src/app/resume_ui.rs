//! Selector de conversaciones anteriores del agente (el equivalente a
//! `/resume`, pero desde la app).
//!
//! No duplicamos el historial: se lista el que el propio CLI ya escribió en
//! disco y se reanuda la elegida pasándole `--resume <id>`.

use egui::{vec2, Align2, RichText, ScrollArea};

use crate::orchestration::AgentSessionEntry;
use crate::theme::colors as palette;

use super::TerminalApp;

pub(super) struct ResumeState {
    /// Directorio cuyas conversaciones se están listando.
    pub(super) cwd: std::path::PathBuf,
    /// Comando original del panel, incluidos sus argumentos de modelo/config.
    pub(super) launch_command: String,
    pub(super) sessions: Vec<AgentSessionEntry>,
    pub(super) selected: usize,
    receiver: Option<std::sync::mpsc::Receiver<Vec<AgentSessionEntry>>>,
    load_error: Option<String>,
}

impl TerminalApp {
    pub(super) fn open_resume_picker(&mut self) {
        let focused_agent = self.ws().focused_panel().and_then(|panel| {
            let command = panel.agent_command()?.to_owned();
            let provider = crate::orchestration::AgentProvider::detect(&command)?;
            Some((
                provider,
                command,
                panel.agent_session_id().map(str::to_owned),
            ))
        });
        let claude_launch_command = focused_agent
            .as_ref()
            .filter(|(provider, _, _)| *provider == crate::orchestration::AgentProvider::ClaudeCode)
            .map(|(_, command, _)| command.clone())
            .unwrap_or_else(|| "claude".to_owned());

        // Los proveedores sin explorador de historial integrado igual pueden
        // retomar su conversación más reciente con el contrato oficial del
        // CLI. Se reemplaza el proceso actual para que el comando se ejecute
        // en un shell nuevo, nunca como texto dentro del prompt del agente.
        if let Some((provider, command, session_id)) = focused_agent {
            if provider != crate::orchestration::AgentProvider::ClaudeCode {
                let Some(resume) =
                    resume_plan_for_provider(provider, &command, session_id.as_deref())
                else {
                    self.toast_error(format!(
                        "{} no ofrece reanudación automática verificada",
                        provider.label()
                    ));
                    return;
                };
                self.replace_focused_agent(resume, command, provider.label().to_owned());
                return;
            }
        }

        // El cwd del panel enfocado es el que decide qué proyecto se lista:
        // los CLI indexan su historial por directorio de trabajo.
        let cwd = self
            .ws()
            .focused_panel()
            .and_then(|panel| panel.current_cwd())
            .map(std::path::PathBuf::from)
            .or_else(|| self.ws().cwd.clone());

        let Some(cwd) = cwd else {
            self.toast_error("No sé en qué carpeta buscar: abrí una carpeta primero");
            return;
        };

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let root = cwd.clone();
        let repaint = self.ctx.clone();
        let started = std::thread::Builder::new()
            .name("resume-history-reader".into())
            .spawn(move || {
                let _ = sender.send(crate::orchestration::list_claude_sessions(&root));
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            })
            .is_ok();
        self.resume_picker = Some(ResumeState {
            cwd,
            launch_command: claude_launch_command,
            sessions: Vec::new(),
            selected: 0,
            receiver: started.then_some(receiver),
            load_error: (!started).then(|| "No se pudo iniciar la lectura del historial".into()),
        });
    }

    pub(super) fn poll_resume_picker(&mut self, ctx: &egui::Context) {
        if let Some(state) = self.resume_picker.as_mut() {
            match state.receiver.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(sessions)) => {
                    state.sessions = sessions;
                    state.receiver = None;
                }
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    state.receiver = None;
                    state.load_error = Some("No se pudo leer el historial".into());
                }
                Some(Err(std::sync::mpsc::TryRecvError::Empty)) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50))
                }
                None => {}
            }
        }
    }

    pub(super) fn show_resume_picker(&mut self, ctx: &egui::Context) {
        if self.resume_picker.is_none() {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.resume_picker = None;
            return;
        }

        // Navegación con flechas antes de dibujar, para que la fila resaltada
        // sea la que el usuario acaba de elegir.
        let (up, down, enter) = ctx.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowUp),
                input.key_pressed(egui::Key::ArrowDown),
                input.key_pressed(egui::Key::Enter),
            )
        });
        if let Some(state) = self.resume_picker.as_mut() {
            let last = state.sessions.len().saturating_sub(1);
            if down {
                state.selected = (state.selected + 1).min(last);
            }
            if up {
                state.selected = state.selected.saturating_sub(1);
            }
        }

        let mut chosen: Option<usize> = if enter {
            self.resume_picker.as_ref().map(|state| state.selected)
        } else {
            None
        };
        let mut cancel = false;
        let screen = ctx.content_rect();

        egui::Area::new(egui::Id::new("resume-backdrop"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter().rect_filled(
                    screen,
                    0.0,
                    egui::Color32::from_rgba_premultiplied(0, 0, 0, 150),
                );
                ui.allocate_space(screen.size());
            });

        egui::Area::new(egui::Id::new("resume-dialog"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(palette::SURFACE)
                    .stroke(egui::Stroke::new(1.0_f32, palette::LINE))
                    .corner_radius(12.0)
                    .inner_margin(18.0)
                    .show(ui, |ui| {
                        ui.set_width(620.0);
                        let Some(state) = self.resume_picker.as_ref() else {
                            return;
                        };
                        ui.label(
                            RichText::new("Retomar conversación")
                                .size(15.0)
                                .color(palette::TEXT_STRONG),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} conversaciones en {}",
                                state.sessions.len(),
                                state.cwd.display()
                            ))
                            .size(10.5)
                            .color(palette::DIM),
                        );
                        ui.add_space(10.0);

                        if state.receiver.is_some() {
                            ui.spinner();
                            ui.label("Buscando conversaciones…");
                        } else if let Some(error) = &state.load_error {
                            ui.label(error);
                        } else if state.sessions.is_empty() {
                            ui.label("No hay conversaciones guardadas para esta carpeta");
                        }
                        let selected = state.selected;
                        ScrollArea::vertical()
                            .id_salt("resume-list")
                            .max_height(340.0)
                            .show(ui, |ui| {
                                for (index, entry) in state.sessions.iter().enumerate() {
                                    let active = index == selected;
                                    let (rect, response) = ui.allocate_exact_size(
                                        vec2(ui.available_width(), 30.0),
                                        egui::Sense::click(),
                                    );
                                    if active || response.hovered() {
                                        ui.painter().rect_filled(
                                            rect,
                                            5.0,
                                            if active {
                                                palette::FOCUS
                                            } else {
                                                palette::HOVER
                                            },
                                        );
                                    }
                                    ui.painter().text(
                                        egui::pos2(rect.left() + 10.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        &entry.title,
                                        egui::FontId::proportional(12.0),
                                        if active {
                                            palette::TEXT_STRONG
                                        } else {
                                            palette::TEXT
                                        },
                                    );
                                    if response.clicked() {
                                        chosen = Some(index);
                                    }
                                }
                            });

                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Retomar").clicked() {
                                chosen = Some(selected);
                            }
                            if ui.button("Cancelar").clicked() {
                                cancel = true;
                            }
                            ui.label(
                                RichText::new("↑↓ para elegir · Enter para retomar")
                                    .size(10.0)
                                    .color(palette::DIM),
                            );
                        });
                    });
            });

        if cancel {
            self.resume_picker = None;
            return;
        }
        if let Some(index) = chosen {
            self.resume_selected_session(index);
        }
    }

    fn resume_selected_session(&mut self, index: usize) {
        let Some(state) = self.resume_picker.take() else {
            return;
        };
        let Some(entry) = state.sessions.get(index) else {
            return;
        };
        let launch_command = state.launch_command.clone();
        // `--resume <id>` entra a esa conversación puntual, a diferencia de
        // `--continue`, que toma la más reciente. `resume_invocation`
        // sanitiza el id: un archivo de sesión malicioso no puede inyectar
        // flags.
        let Some(resume) = ResumePlan::exact(
            crate::orchestration::AgentProvider::ClaudeCode,
            &launch_command,
            &entry.id,
        ) else {
            self.toast_error("Esa conversación tiene un id inválido; no se puede retomar");
            return;
        };
        let title = entry.title.clone();

        let panel_id = self.ws().focused_panel().map(|panel| panel.id());
        let Some(panel_id) = panel_id else {
            self.toast_error("No hay terminal enfocado donde retomarla");
            return;
        };
        self.replace_agent_in_panel(panel_id, resume, launch_command, title);
    }

    fn replace_focused_agent(&mut self, resume: ResumePlan, launch_command: String, title: String) {
        let Some(panel_id) = self.ws().focused_panel().map(|panel| panel.id()) else {
            self.toast_error("No hay terminal enfocado donde retomarla");
            return;
        };
        self.replace_agent_in_panel(panel_id, resume, launch_command, title);
    }

    /// Un comando de resume pertenece al shell, no al prompt del agente que
    /// está corriendo. Cerrar y recrear la sesión de runtime en el mismo panel
    /// evita que `claude --resume ...` termine enviado como un mensaje.
    fn replace_agent_in_panel(
        &mut self,
        panel_id: uuid::Uuid,
        resume: ResumePlan,
        launch_command: String,
        title: String,
    ) {
        let cwd = self
            .ws()
            .panel(panel_id)
            .and_then(|panel| panel.current_cwd())
            .map(std::path::PathBuf::from)
            .or_else(|| self.ws().cwd.clone());
        let manager = self.ws().pty_manager();
        let workspace_id = self.ws().id;
        let Some(panel) = self.ws_mut().panel_mut(panel_id) else {
            self.toast_error("No se encontró el terminal donde retomarla");
            return;
        };
        let resumed =
            replace_and_remember_resume_session(panel, resume.exact_session_id, |panel| {
                panel.replace_focused_session(
                    manager,
                    cwd.as_deref(),
                    workspace_id,
                    resume.command,
                    launch_command,
                )
            });
        if resumed {
            self.toast_success(format!("Retomando: {title}"));
        } else {
            self.toast_error("No se pudo iniciar la conversación reanudada");
        }
    }
}

struct ResumePlan {
    command: String,
    exact_session_id: Option<String>,
}

impl ResumePlan {
    fn exact(
        provider: crate::orchestration::AgentProvider,
        command: &str,
        session_id: &str,
    ) -> Option<Self> {
        // Gemini's own latest selector is not a stable conversation identity.
        if provider == crate::orchestration::AgentProvider::GeminiCli && session_id == "latest" {
            return None;
        }
        let command = crate::orchestration::resume_invocation(provider, command, session_id)?;
        Some(Self {
            command,
            exact_session_id: Some(session_id.to_owned()),
        })
    }
}

fn resume_plan_for_provider(
    provider: crate::orchestration::AgentProvider,
    command: &str,
    session_id: Option<&str>,
) -> Option<ResumePlan> {
    session_id
        .and_then(|id| ResumePlan::exact(provider, command, id))
        .or_else(|| {
            crate::orchestration::supports_latest_resume(provider).then(|| ResumePlan {
                command: crate::orchestration::resume_command(provider, command),
                exact_session_id: None,
            })
        })
}

/// Replacement clears the old hook identity. Publish only the ID carried by
/// this successful exact resume, before any new hook or layout snapshot.
fn replace_and_remember_resume_session(
    panel: &mut crate::panel::CanvasPanel,
    exact_session_id: Option<String>,
    replace: impl FnOnce(&mut crate::panel::CanvasPanel) -> bool,
) -> bool {
    let leaf_id = panel.focused_leaf_id();
    if !replace(panel) {
        return false;
    }
    panel.set_agent_session_id_for_leaf(leaf_id, exact_session_id);
    true
}

#[cfg(test)]
fn resume_for_provider(
    provider: crate::orchestration::AgentProvider,
    command: &str,
    session_id: Option<&str>,
) -> Option<String> {
    resume_plan_for_provider(provider, command, session_id).map(|plan| plan.command)
}

#[cfg(test)]
mod tests {
    use super::resume_for_provider;
    use crate::orchestration::AgentProvider;

    #[test]
    fn non_claude_resume_prefers_the_exact_hook_session() {
        assert_eq!(
            resume_for_provider(AgentProvider::CodexCli, "codex --config x", Some("sess-42"))
                .as_deref(),
            Some("codex --config x resume sess-42")
        );
        assert_eq!(
            resume_for_provider(AgentProvider::GeminiCli, "gemini", Some("gem-7")).as_deref(),
            Some("gemini --resume gem-7")
        );
    }

    #[test]
    fn non_claude_resume_falls_back_to_latest_without_an_id() {
        assert_eq!(
            resume_for_provider(AgentProvider::OpenCode, "opencode", None).as_deref(),
            Some("opencode --continue")
        );
    }
}

#[cfg(test)]
mod resume_identity_tests {
    use std::sync::{Arc, Mutex};

    use egui::{pos2, vec2, Color32};
    use uuid::Uuid;

    use super::{replace_and_remember_resume_session, resume_plan_for_provider, ResumePlan};
    use crate::orchestration::AgentProvider;
    use crate::panel::CanvasPanel;
    use crate::runtime::PtyManager;
    use crate::terminal::panel::TerminalPanel;
    use crate::terminal::session_controller::session_spec;
    use crate::terminal::split_tree::{Axis, SplitNode};

    fn panel_with_old_identity() -> CanvasPanel {
        let mut panel = CanvasPanel::Terminal(TerminalPanel::new(
            pos2(10.0, 20.0),
            vec2(640.0, 420.0),
            Color32::WHITE,
            1,
        ));
        panel.set_agent_command(Some("claude --model sonnet".to_owned()));
        panel.set_agent_session_id(Some("old-conversation".to_owned()));
        panel
    }

    #[test]
    fn selected_resume_id_survives_save_and_cold_specs_without_hooks() {
        let mut saved = panel_with_old_identity().to_saved();
        let root =
            Uuid::parse_str(saved.root_leaf.as_deref().expect("root leaf")).expect("root UUID");
        let mut tree = SplitNode::leaf(root);
        let selected_leaf = tree.split(root, Axis::Horizontal).expect("split leaf");
        let launch_command = "claude --model sonnet";
        let workspace_id = Uuid::from_u128(42);
        saved.split_tree = Some(serde_json::to_value(&tree).expect("serialize tree"));
        saved.focused_leaf = Some(selected_leaf.to_string());
        saved
            .leaf_agent_commands
            .insert(selected_leaf.to_string(), launch_command.to_owned());
        saved
            .leaf_agent_session_ids
            .insert(selected_leaf.to_string(), "old-selected".to_owned());
        let manager = Arc::new(Mutex::new(PtyManager::new_for_tests()));
        let mut panel = CanvasPanel::Terminal(TerminalPanel::from_saved(
            saved,
            &egui::Context::default(),
            None,
            workspace_id,
            Arc::clone(&manager),
        ));
        let panel_id = panel.id();
        let resume = ResumePlan::exact(AgentProvider::ClaudeCode, launch_command, "chosen-B")
            .expect("valid selected conversation");
        let mut captured_spec = None;

        // Model only replacement's metadata effect; never attach or spawn a PTY.
        let resumed =
            replace_and_remember_resume_session(&mut panel, resume.exact_session_id, |panel| {
                assert_eq!(panel.agent_session_id(), Some("old-selected"));
                captured_spec = Some(session_spec(
                    "Selected conversation".to_owned(),
                    None,
                    Some(resume.command),
                    None,
                    Some(panel.id()),
                    Some(panel.focused_leaf_id()),
                    Some(workspace_id),
                ));
                panel.set_agent_command(Some(launch_command.to_owned()));
                panel.set_agent_session_id(None);
                assert_eq!(panel.agent_session_id(), None);
                true
            });
        assert!(resumed);
        let spec = captured_spec.expect("captured pure SessionSpec");
        assert_eq!(
            spec.startup_command.as_deref(),
            Some("claude --model sonnet --resume chosen-B")
        );
        assert_eq!(spec.panel_id, Some(panel_id));
        assert_eq!(spec.leaf_id, Some(selected_leaf));
        assert_eq!(spec.workspace_id, Some(workspace_id));
        assert_eq!(panel.focused_leaf_id(), selected_leaf);
        assert_eq!(panel.agent_session_id(), Some("chosen-B"));
        assert_eq!(panel.agent_command(), Some(launch_command));
        let saved = panel.to_saved();
        assert_eq!(saved.agent_session_id.as_deref(), Some("old-conversation"));
        assert_eq!(
            saved
                .leaf_agent_session_ids
                .get(&root.to_string())
                .map(String::as_str),
            Some("old-conversation")
        );
        assert_eq!(
            saved
                .leaf_agent_session_ids
                .get(&selected_leaf.to_string())
                .map(String::as_str),
            Some("chosen-B")
        );
        assert_eq!(
            saved
                .leaf_agent_commands
                .get(&selected_leaf.to_string())
                .map(String::as_str),
            Some(launch_command)
        );

        let encoded = serde_json::to_vec(&saved).expect("serialize panel");
        let saved = serde_json::from_slice(&encoded).expect("deserialize panel");
        let cold_manager = Arc::new(Mutex::new(PtyManager::new_for_tests()));
        let cold = TerminalPanel::from_saved(
            saved,
            &egui::Context::default(),
            None,
            workspace_id,
            Arc::clone(&cold_manager),
        );
        assert_eq!(cold.id, panel_id);
        assert_eq!(cold.focused_leaf_id(), selected_leaf);
        assert_eq!(cold.agent_session_id(), Some("chosen-B"));
        assert_eq!(cold.agent_command(), Some(launch_command));
        let runtime_ids = cold.all_runtime_session_ids();
        let guard = cold_manager.lock().expect("cold manager");
        assert_eq!(guard.attached_session_count(), 0);
        assert_eq!(guard.detached_session_count(), 2);
        assert_eq!(
            guard.startup_command_for_tests(runtime_ids[0]),
            Some("claude --model sonnet --resume old-conversation")
        );
        assert_eq!(
            guard.startup_command_for_tests(runtime_ids[1]),
            Some("claude --model sonnet --resume chosen-B")
        );
        drop(guard);
        let guard = manager.lock().expect("original detached manager");
        assert_eq!(guard.attached_session_count(), 0);
        assert_eq!(guard.detached_session_count(), 2);
    }

    #[test]
    fn hook_identity_is_kept_only_by_a_valid_exact_resume_plan() {
        for (provider, launch, expected) in [
            (
                AgentProvider::ClaudeCode,
                "claude --model sonnet",
                "claude --model sonnet --resume known-42",
            ),
            (
                AgentProvider::CodexCli,
                "codex --config x",
                "codex --config x resume known-42",
            ),
            (
                AgentProvider::GeminiCli,
                "gemini --model pro",
                "gemini --model pro --resume known-42",
            ),
            (
                AgentProvider::OpenCode,
                "opencode",
                "opencode --session known-42",
            ),
            (
                AgentProvider::Copilot,
                "copilot",
                "copilot --resume known-42",
            ),
        ] {
            let plan = resume_plan_for_provider(provider, launch, Some("known-42"))
                .expect("provider supports exact resume");
            assert_eq!(plan.command, expected);
            assert_eq!(plan.exact_session_id.as_deref(), Some("known-42"));
        }
        for invalid in ["", "-flag", "id with spaces", "id\nnext", "id;next"] {
            assert!(ResumePlan::exact(AgentProvider::ClaudeCode, "claude", invalid).is_none());
            let fallback =
                resume_plan_for_provider(AgentProvider::ClaudeCode, "claude", Some(invalid))
                    .expect("latest fallback");
            assert_eq!(fallback.command, "claude --continue");
            assert_eq!(fallback.exact_session_id, None);
        }
        assert!(
            resume_plan_for_provider(AgentProvider::Aider, "aider", Some("known-42")).is_none()
        );
        let gemini_latest =
            resume_plan_for_provider(AgentProvider::GeminiCli, "gemini", Some("latest"))
                .expect("Gemini latest fallback");
        assert_eq!(gemini_latest.command, "gemini --resume latest");
        assert_eq!(gemini_latest.exact_session_id, None);
        assert!(ResumePlan::exact(AgentProvider::GeminiCli, "gemini", "latest").is_none());
    }
}
