//! Broadcast: manda el mismo comando/prompt a varios terminales de una vez.
//!
//! Es la pieza que hace usable el workflow multi-agente: en vez de repetir
//! "git pull" o la misma instrucción en ocho paneles, se escribe una vez y se
//! elige a quién va. La selección de destinos es lógica pura y está testeada;
//! la UI sólo la maneja.

use egui::{vec2, Align2, RichText, ScrollArea};
use uuid::Uuid;

use crate::theme::colors as palette;

use super::TerminalApp;

/// Un destino posible del broadcast, tal como se lista en el diálogo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BroadcastTarget {
    pub(super) panel_id: Uuid,
    pub(super) title: String,
    /// Un panel sin terminal vivo no puede recibir nada: se lista en gris y
    /// no se puede tildar.
    pub(super) alive: bool,
    pub(super) selected: bool,
}

pub(super) struct BroadcastState {
    pub(super) text: String,
    pub(super) targets: Vec<BroadcastTarget>,
}

impl BroadcastState {
    /// Panels vivos y tildados. Los muertos se filtran incluso si quedaron
    /// tildados de antes (el panel pudo morir con el diálogo abierto).
    fn selected_ids(&self) -> Vec<Uuid> {
        self.targets
            .iter()
            .filter(|target| target.alive && target.selected)
            .map(|target| target.panel_id)
            .collect()
    }

    fn selected_count(&self) -> usize {
        self.selected_ids().len()
    }

    fn alive_count(&self) -> usize {
        self.targets.iter().filter(|target| target.alive).count()
    }

    /// ¿Se puede enviar? Hace falta texto y al menos un destino vivo tildado.
    fn can_send(&self) -> bool {
        !self.text.trim().is_empty() && self.selected_count() > 0
    }

    fn set_all_alive(&mut self, selected: bool) {
        for target in self.targets.iter_mut().filter(|target| target.alive) {
            target.selected = selected;
        }
    }
}

impl TerminalApp {
    pub(super) fn open_broadcast(&mut self) {
        let targets: Vec<BroadcastTarget> = self
            .ws()
            .panels
            .iter()
            .map(|panel| BroadcastTarget {
                panel_id: panel.id(),
                title: panel.title().to_owned(),
                alive: panel.is_alive(),
                // Por defecto todos los vivos: el caso común es "a todos".
                selected: panel.is_alive(),
            })
            .collect();

        if targets.iter().all(|target| !target.alive) {
            self.toast_error("No hay terminales vivos a los que enviar");
            return;
        }
        self.broadcast = Some(BroadcastState {
            text: String::new(),
            targets,
        });
    }

    pub(super) fn show_broadcast(&mut self, ctx: &egui::Context) {
        if self.broadcast.is_none() {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.broadcast = None;
            return;
        }

        let mut send = false;
        let mut cancel = false;
        let screen = ctx.screen_rect();

        egui::Area::new(egui::Id::new("broadcast-backdrop"))
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

        egui::Area::new(egui::Id::new("broadcast-dialog"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(palette::SURFACE)
                    .stroke(egui::Stroke::new(1.0, palette::LINE))
                    .rounding(12.0)
                    .inner_margin(18.0)
                    .show(ui, |ui| {
                        ui.set_width(520.0);
                        let Some(state) = self.broadcast.as_mut() else {
                            return;
                        };

                        ui.label(
                            RichText::new("Broadcast a terminales")
                                .size(15.0)
                                .color(palette::TEXT_STRONG),
                        );
                        ui.label(
                            RichText::new(
                                "El texto se envía como prompt (con Enter) a cada terminal tildado.",
                            )
                            .size(11.0)
                            .color(palette::DIM),
                        );
                        ui.add_space(10.0);

                        let field = ui.add(
                            egui::TextEdit::multiline(&mut state.text)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .hint_text("git pull --rebase"),
                        );
                        field.request_focus();
                        ui.add_space(10.0);

                        let (alive, selected) = (state.alive_count(), state.selected_count());
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Destinos ({selected}/{alive})"))
                                    .size(12.0)
                                    .color(palette::TEXT),
                            );
                            if ui.small_button("Todos").clicked() {
                                state.set_all_alive(true);
                            }
                            if ui.small_button("Ninguno").clicked() {
                                state.set_all_alive(false);
                            }
                        });
                        ui.add_space(4.0);

                        ScrollArea::vertical()
                            .id_salt("broadcast-targets")
                            .max_height(200.0)
                            .show(ui, |ui| {
                                for target in state.targets.iter_mut() {
                                    if target.alive {
                                        ui.checkbox(&mut target.selected, &target.title);
                                    } else {
                                        // Sin terminal vivo: visible pero no
                                        // seleccionable, para que se entienda
                                        // por qué no recibe nada.
                                        ui.add_enabled_ui(false, |ui| {
                                            let mut off = false;
                                            ui.checkbox(
                                                &mut off,
                                                format!("{} (sin terminal)", target.title),
                                            );
                                        });
                                    }
                                }
                            });

                        ui.add_space(14.0);
                        let can_send = state.can_send();
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(can_send, egui::Button::new("Enviar"))
                                .clicked()
                            {
                                send = true;
                            }
                            if ui.button("Cancelar").clicked() {
                                cancel = true;
                            }
                            if !can_send {
                                ui.label(
                                    RichText::new("Escribí un comando y elegí al menos un destino")
                                        .size(10.0)
                                        .color(palette::DIM),
                                );
                            }
                        });
                    });
            });

        // Ctrl+Enter envía sin tener que ir al botón.
        if ctx.input(|input| input.modifiers.command && input.key_pressed(egui::Key::Enter)) {
            send = true;
        }

        if cancel {
            self.broadcast = None;
            return;
        }
        if send {
            self.dispatch_broadcast();
        }
    }

    fn dispatch_broadcast(&mut self) {
        let Some(state) = self.broadcast.as_ref() else {
            return;
        };
        if !state.can_send() {
            return;
        }
        let text = state.text.trim().to_owned();
        let ids = state.selected_ids();
        self.broadcast = None;

        let mut sent = 0usize;
        let mut failed = 0usize;
        for panel_id in ids {
            if self.ws_mut().send_prompt_to_panel(panel_id, &text) {
                sent += 1;
            } else {
                failed += 1;
            }
        }
        if sent > 0 {
            self.toast_success(format!("Enviado a {sent} terminal(es)"));
        }
        if failed > 0 {
            self.toast_error(format!("{failed} panel(es) ya no existían"));
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{BroadcastState, BroadcastTarget};

    fn target(title: &str, alive: bool, selected: bool) -> BroadcastTarget {
        BroadcastTarget {
            panel_id: Uuid::new_v4(),
            title: title.to_owned(),
            alive,
            selected,
        }
    }

    fn state(text: &str, targets: Vec<BroadcastTarget>) -> BroadcastState {
        BroadcastState {
            text: text.to_owned(),
            targets,
        }
    }

    #[test]
    fn only_alive_and_selected_panels_receive_the_broadcast() {
        let state = state(
            "ls",
            vec![
                target("alive+on", true, true),
                target("alive+off", true, false),
                target("dead+on", false, true),
                target("dead+off", false, false),
            ],
        );
        let ids = state.selected_ids();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], state.targets[0].panel_id);
    }

    #[test]
    fn dead_panels_do_not_count_even_if_they_stayed_ticked() {
        // Un panel puede morir con el diálogo abierto; su tilde vieja no debe
        // inflar la cuenta ni habilitar el envío.
        let state = state("ls", vec![target("dead", false, true)]);
        assert_eq!(state.selected_count(), 0);
        assert!(!state.can_send());
    }

    #[test]
    fn empty_or_blank_text_cannot_be_sent() {
        let targets = vec![target("alive", true, true)];
        assert!(!state("", targets.clone()).can_send());
        assert!(!state("   \n\t ", targets.clone()).can_send());
        assert!(state("ls", targets).can_send());
    }

    #[test]
    fn sending_requires_at_least_one_target() {
        assert!(!state("ls", vec![target("alive", true, false)]).can_send());
        assert!(!state("ls", Vec::new()).can_send());
    }

    #[test]
    fn select_all_skips_dead_panels() {
        let mut state = state(
            "ls",
            vec![target("alive", true, false), target("dead", false, false)],
        );
        state.set_all_alive(true);
        assert!(state.targets[0].selected);
        assert!(
            !state.targets[1].selected,
            "a dead panel must never be selected"
        );
        assert_eq!(state.selected_count(), 1);
    }

    #[test]
    fn select_none_clears_every_tick() {
        let mut state = state("ls", vec![target("a", true, true), target("b", true, true)]);
        state.set_all_alive(false);
        assert_eq!(state.selected_count(), 0);
        assert!(!state.can_send());
    }

    #[test]
    fn alive_count_ignores_dead_panels() {
        let state = state(
            "ls",
            vec![
                target("a", true, true),
                target("b", true, false),
                target("c", false, false),
            ],
        );
        assert_eq!(state.alive_count(), 2);
        assert_eq!(state.selected_count(), 1);
    }
}
