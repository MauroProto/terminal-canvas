//! Diálogo de configuración: edita `AppConfig` en vivo y lo persiste a
//! `config.toml`. Los cambios booleanos aplican al instante; el tamaño de
//! fuente se re-aplica en el momento; scrollback y shell rigen para los
//! terminales nuevos.

use egui::{vec2, Align2, Color32, RichText, ScrollArea};

use crate::config::{self, AppConfig};
use crate::terminal::metrics::{set_base_font_size, MAX_FONT_SIZE, MIN_FONT_SIZE};
use crate::theme::colors as palette;

use super::TerminalApp;

#[derive(Debug, Clone)]
pub(super) struct SettingsDraft {
    pub(super) font_size: f32,
    pub(super) scrollback_lines: usize,
    pub(super) allow_osc52: bool,
    pub(super) audio_bell: bool,
    pub(super) copy_on_select: bool,
    pub(super) agent_notifications: bool,
    pub(super) shell: String,
}

impl SettingsDraft {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            font_size: config.font_size,
            scrollback_lines: config.scrollback_lines,
            allow_osc52: config.allow_osc52,
            audio_bell: config.audio_bell,
            copy_on_select: config.copy_on_select,
            agent_notifications: config.agent_notifications,
            shell: config.shell.clone().unwrap_or_default(),
        }
    }

    fn to_config(&self) -> AppConfig {
        let shell = self.shell.trim().to_owned();
        AppConfig {
            font_size: self.font_size,
            scrollback_lines: self.scrollback_lines,
            allow_osc52: self.allow_osc52,
            audio_bell: self.audio_bell,
            copy_on_select: self.copy_on_select,
            agent_notifications: self.agent_notifications,
            shell: if shell.is_empty() { None } else { Some(shell) },
        }
    }
}

impl TerminalApp {
    pub(super) fn open_settings(&mut self) {
        let config = config::runtime_config();
        self.settings_draft = Some(SettingsDraft::from_config(&config));
        self.settings_open = true;
    }

    pub(super) fn show_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open || self.settings_draft.is_none() {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.settings_open = false;
            self.settings_draft = None;
            return;
        }

        let mut save = false;
        let mut cancel = false;
        let screen = ctx.screen_rect();

        egui::Area::new(egui::Id::new("settings-backdrop"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter().rect_filled(
                    screen,
                    0.0,
                    Color32::from_rgba_premultiplied(0, 0, 0, 170),
                );
            });

        egui::Area::new(egui::Id::new("settings-dialog"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(palette::INK)
                    .stroke(egui::Stroke::new(1.0, palette::LINE))
                    .rounding(10.0)
                    .inner_margin(egui::Margin::same(20.0))
                    .show(ui, |ui| {
                        ui.set_min_width(440.0);
                        ui.label(
                            RichText::new("Configuración")
                                .size(16.0)
                                .color(palette::TEXT_STRONG)
                                .strong(),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Los cambios se guardan en config.toml")
                                .size(11.0)
                                .color(palette::DIM),
                        );
                        ui.add_space(12.0);
                        ui.separator();

                        ScrollArea::vertical()
                            .id_salt("settings-scroll")
                            .max_height(screen.height() * 0.6)
                            .show(ui, |ui| {
                                self.settings_body(ui);
                            });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Guardar").clicked() {
                                        save = true;
                                    }
                                    if ui.button("Cancelar").clicked() {
                                        cancel = true;
                                    }
                                },
                            );
                        });
                    });
            });

        if save {
            self.apply_settings_draft();
            self.settings_open = false;
            self.settings_draft = None;
        } else if cancel {
            self.settings_open = false;
            self.settings_draft = None;
        }
    }

    fn settings_body(&mut self, ui: &mut egui::Ui) {
        let Some(draft) = self.settings_draft.as_mut() else {
            return;
        };

        // Tamaño de fuente.
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Tamaño de fuente")
                    .size(12.0)
                    .color(palette::TEXT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::DragValue::new(&mut draft.font_size)
                        .range(MIN_FONT_SIZE..=MAX_FONT_SIZE)
                        .speed(0.5),
                );
            });
        });
        ui.add_space(10.0);

        // Scrollback.
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Líneas de scrollback")
                    .size(12.0)
                    .color(palette::TEXT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::DragValue::new(&mut draft.scrollback_lines)
                        .range(100..=1_000_000)
                        .speed(500),
                );
            });
        });
        ui.label(
            RichText::new("Aplica a terminales nuevos")
                .size(10.0)
                .color(palette::DIM),
        );
        ui.add_space(10.0);

        // Shell.
        ui.label(
            RichText::new("Shell personalizada")
                .size(12.0)
                .color(palette::TEXT),
        );
        ui.add_space(4.0);
        let edit = egui::TextEdit::singleline(&mut draft.shell)
            .hint_text("vacío = login shell del sistema")
            .text_color(palette::TEXT_STRONG)
            .margin(egui::Margin::symmetric(8.0, 5.0));
        ui.add_sized(vec2(ui.available_width() - 8.0, 26.0), edit);
        ui.label(
            RichText::new("Aplica a terminales nuevos")
                .size(10.0)
                .color(palette::DIM),
        );
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        // Toggles.
        checkbox_row(
            ui,
            &mut draft.allow_osc52,
            "Permitir OSC 52",
            "El terminal puede escribir el portapapeles",
        );
        checkbox_row(
            ui,
            &mut draft.audio_bell,
            "Bell sonoro",
            "Sonido al sonar la campana (además del flash)",
        );
        checkbox_row(
            ui,
            &mut draft.copy_on_select,
            "Copiar al seleccionar",
            "Copia automática al terminar una selección",
        );
        checkbox_row(
            ui,
            &mut draft.agent_notifications,
            "Notificaciones de agentes",
            "Aviso del sistema cuando un agente necesita atención",
        );
    }

    fn apply_settings_draft(&mut self) {
        let Some(draft) = self.settings_draft.as_ref() else {
            return;
        };
        let new_config = draft.to_config();
        // El tamaño de fuente se aplica en vivo.
        set_base_font_size(new_config.font_size);
        // Actualiza la config runtime (los booleanos se leen en vivo).
        config::update_runtime_config(new_config.clone());
        // Persistir a disco.
        match config::save(&new_config) {
            Ok(()) => self.toast_success("Configuración guardada en config.toml"),
            Err(err) => {
                log::warn!("No se pudo guardar config.toml: {err}");
                self.toast_error(format!("No se pudo guardar config.toml: {err}"));
            }
        }
        // Forzá un repintado para que el cambio de fuente se vea ya.
        if let Some(ctx) = self.ctx.clone() {
            ctx.request_repaint();
        }
    }
}

fn checkbox_row(ui: &mut egui::Ui, value: &mut bool, label: &str, hint: &str) {
    ui.horizontal(|ui| {
        ui.checkbox(value, "");
        ui.vertical(|ui| {
            ui.label(RichText::new(label).size(12.0).color(palette::TEXT));
            ui.label(RichText::new(hint).size(10.0).color(palette::DIM));
        });
    });
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use super::SettingsDraft;
    use crate::config::AppConfig;

    fn non_default_config() -> AppConfig {
        AppConfig {
            font_size: 18.5,
            scrollback_lines: 31_337,
            allow_osc52: true,
            audio_bell: true,
            copy_on_select: true,
            agent_notifications: false,
            shell: Some("/opt/homebrew/bin/fish".to_owned()),
        }
    }

    #[test]
    fn draft_round_trips_every_field() {
        let config = non_default_config();
        assert_ne!(config, AppConfig::default());
        let restored = SettingsDraft::from_config(&config).to_config();
        assert_eq!(restored, config);
    }

    #[test]
    fn blank_shell_becomes_none_so_we_fall_back_to_the_login_shell() {
        let mut draft = SettingsDraft::from_config(&AppConfig::default());
        draft.shell = "   \t ".to_owned();
        assert_eq!(draft.to_config().shell, None);
    }

    #[test]
    fn shell_is_trimmed_before_being_stored() {
        let mut draft = SettingsDraft::from_config(&AppConfig::default());
        draft.shell = "  /bin/zsh\n".to_owned();
        assert_eq!(draft.to_config().shell.as_deref(), Some("/bin/zsh"));
    }

    #[test]
    fn missing_shell_in_config_shows_as_empty_draft_field() {
        let config = AppConfig {
            shell: None,
            ..AppConfig::default()
        };
        assert!(SettingsDraft::from_config(&config).shell.is_empty());
    }
}
