//! Onboarding: empty states y overlay de primeros pasos (Ship-it 7.2, T2).
//!
//! El overlay se muestra una sola vez: al cerrarlo se persiste `dismissed` en
//! `config.toml`, para que no vuelva en cada arranque.

use egui::{Align2, RichText};

use super::TerminalApp;
use crate::theme::colors as palette;

/// Qué hay que mostrar cuando el canvas está vacío.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EmptyState {
    /// No hay carpeta de workspace: lo primero es abrir una.
    NoFolder,
    /// Hay carpeta pero ningún panel.
    NoPanels,
    /// Hay paneles: no se muestra nada.
    None,
}

/// Decide el empty state a partir del estado del workspace. Puro y testeable.
pub(super) fn empty_state(has_folder: bool, panel_count: usize) -> EmptyState {
    // Un terminal abierto en HOME (sin carpeta de proyecto) ya es contenido
    // válido del canvas. Los mensajes vacíos sólo corresponden cuando no hay
    // ningún panel que puedan tapar o contradecir.
    if panel_count > 0 {
        EmptyState::None
    } else if has_folder {
        EmptyState::NoPanels
    } else {
        EmptyState::NoFolder
    }
}

/// Los tres pasos del overlay de bienvenida.
pub(super) const ONBOARDING_STEPS: [(&str, &str); 3] = [
    (
        "1. Abrí una carpeta",
        "El workspace sigue a la carpeta: los agentes y el diff trabajan sobre ese repo.",
    ),
    (
        "2. Abrí un terminal",
        "Ctrl+Shift+T para un shell, Ctrl+Shift+A para lanzar un agente en su propio worktree.",
    ),
    (
        "3. Revisá los cambios",
        "Ctrl+Shift+D abre el diff: anotá líneas y mandale las notas al agente.",
    ),
];

impl TerminalApp {
    /// Empty state del canvas y overlay de primeros pasos.
    pub(super) fn show_onboarding(&mut self, ctx: &egui::Context) {
        self.show_empty_state(ctx);
        self.show_onboarding_overlay(ctx);
    }

    fn show_empty_state(&mut self, ctx: &egui::Context) {
        let state = empty_state(self.ws().cwd.is_some(), self.ws().panels.len());
        if state == EmptyState::None {
            return;
        }
        let mut open_folder = false;
        let mut spawn_terminal = false;
        egui::Area::new(egui::Id::new("empty-state"))
            .order(egui::Order::Middle)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| match state {
                    EmptyState::NoFolder => {
                        ui.label(
                            RichText::new("Todavía no hay carpeta abierta")
                                .size(15.0)
                                .color(palette::TEXT_STRONG),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("El workspace sigue a la carpeta que elijas.")
                                .size(12.0)
                                .color(palette::DIM),
                        );
                        ui.add_space(10.0);
                        if ui.button("Abrir carpeta").clicked() {
                            open_folder = true;
                        }
                    }
                    EmptyState::NoPanels => {
                        ui.label(
                            RichText::new("Sin terminales abiertos")
                                .size(15.0)
                                .color(palette::TEXT_STRONG),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Ctrl+Shift+T abre un terminal nuevo.")
                                .size(12.0)
                                .color(palette::DIM),
                        );
                        ui.add_space(10.0);
                        if ui.button("Nuevo terminal").clicked() {
                            spawn_terminal = true;
                        }
                    }
                    EmptyState::None => {}
                });
            });
        if open_folder {
            self.pick_workspace_folder(ctx);
        }
        if spawn_terminal {
            self.ws_mut().spawn_terminal(ctx);
            self.reconcile_orchestration();
        }
    }

    fn show_onboarding_overlay(&mut self, ctx: &egui::Context) {
        if self.onboarding_dismissed {
            return;
        }
        let mut dismiss = false;
        egui::Area::new(egui::Id::new("onboarding-overlay"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -60.0))
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(palette::INK)
                    .stroke(egui::Stroke::new(1.0, palette::LINE))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_max_width(440.0);
                        ui.label(
                            RichText::new("Primeros pasos")
                                .size(13.0)
                                .color(palette::TEXT_STRONG)
                                .strong(),
                        );
                        ui.add_space(8.0);
                        for (title, body) in ONBOARDING_STEPS {
                            ui.label(RichText::new(title).size(12.0).color(palette::TEXT));
                            ui.label(RichText::new(body).size(11.0).color(palette::DIM));
                            ui.add_space(6.0);
                        }
                        ui.add_space(2.0);
                        if ui.button("Entendido").clicked() {
                            dismiss = true;
                        }
                    });
            });
        if dismiss {
            self.dismiss_onboarding();
        }
    }

    /// Cierra el overlay y lo persiste, para no repetirlo en cada arranque.
    fn dismiss_onboarding(&mut self) {
        self.onboarding_dismissed = true;
        let mut config = crate::config::runtime_config();
        config.onboarding_dismissed = true;
        crate::config::update_runtime_config(config.clone());
        if let Err(err) = crate::config::save(&config) {
            log::warn!("no se pudo persistir el onboarding: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{empty_state, EmptyState, ONBOARDING_STEPS};

    #[test]
    fn without_a_folder_or_panels_the_first_step_is_opening_one() {
        assert_eq!(empty_state(false, 0), EmptyState::NoFolder);
    }

    #[test]
    fn a_live_terminal_hides_the_empty_state_even_without_a_folder() {
        // Un shell abierto en HOME es un estado válido: no debe quedar tapado
        // por una invitación a abrir una carpeta.
        assert_eq!(empty_state(false, 1), EmptyState::None);
    }

    #[test]
    fn with_a_folder_and_no_panels_it_hints_the_shortcut() {
        assert_eq!(empty_state(true, 0), EmptyState::NoPanels);
    }

    #[test]
    fn with_panels_nothing_is_shown() {
        assert_eq!(empty_state(true, 1), EmptyState::None);
    }

    #[test]
    fn the_overlay_has_exactly_three_steps_with_content() {
        assert_eq!(ONBOARDING_STEPS.len(), 3);
        for (title, body) in ONBOARDING_STEPS {
            assert!(!title.trim().is_empty());
            assert!(!body.trim().is_empty());
        }
    }
}
