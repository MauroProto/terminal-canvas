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
    pub(super) sessions: Vec<AgentSessionEntry>,
    pub(super) selected: usize,
}

impl TerminalApp {
    pub(super) fn open_resume_picker(&mut self) {
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

        let sessions = crate::orchestration::list_claude_sessions(&cwd);
        if sessions.is_empty() {
            self.toast_error(format!(
                "No hay conversaciones guardadas para {}",
                cwd.display()
            ));
            return;
        }
        self.resume_picker = Some(ResumeState {
            cwd,
            sessions,
            selected: 0,
        });
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
        let screen = ctx.screen_rect();

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
                egui::Frame::none()
                    .fill(palette::SURFACE)
                    .stroke(egui::Stroke::new(1.0, palette::LINE))
                    .rounding(12.0)
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
        // `--resume <id>` entra a esa conversación puntual, a diferencia de
        // `--continue`, que toma la más reciente.
        let command = format!("claude --resume {}", entry.id);
        let title = entry.title.clone();

        let panel_id = self.ws().focused_panel().map(|panel| panel.id());
        let Some(panel_id) = panel_id else {
            self.toast_error("No hay terminal enfocado donde retomarla");
            return;
        };
        if self.ws_mut().send_prompt_to_panel(panel_id, &command) {
            self.toast_success(format!("Retomando: {title}"));
        } else {
            self.toast_error("No se pudo escribir en ese terminal");
        }
    }
}
