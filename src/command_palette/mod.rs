use egui::{pos2, vec2, Align2, Area, Color32, FontId, Frame, Id, Key, Order, RichText, TextEdit};

use crate::command_palette::commands::{Command, CommandEntry, COMMANDS};
use crate::command_palette::fuzzy::fuzzy_score;
use crate::theme::colors as palette;

pub mod commands;
pub mod fuzzy;
pub mod rank;

#[derive(Default)]
pub struct CommandPalette {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    request_focus: bool,
    pub desktop_mode: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    #[test]
    fn keyboard_selection_scrolls_beyond_the_first_ten_commands() {
        let mut palette = CommandPalette {
            desktop_mode: true,
            ..Default::default()
        };
        palette.toggle();
        let expected = palette.filtered_entries()[15];
        let mut harness = Harness::builder()
            .with_size(vec2(700.0, 580.0))
            .build_state(
                |ctx, state: &mut (CommandPalette, Option<Command>)| {
                    if let Some(command) = state.0.show(ctx) {
                        state.1 = Some(command);
                    }
                },
                (palette, None),
            );
        for _ in 0..15 {
            harness.key_press(Key::ArrowDown);
            harness.run();
        }
        assert_eq!(harness.state().0.selected, 15);
        let selected = harness.get_by_label(expected.label).rect();
        assert!(
            selected.top() >= 80.0 && selected.bottom() <= 560.0,
            "selected row must be visible: {selected:?}"
        );
        harness.key_press(Key::Enter);
        harness.run();
        assert_eq!(harness.state().1, Some(expected.command));
        assert!(!harness.state().0.open);
    }

    #[test]
    fn opening_the_palette_focuses_search_and_updates_results_in_the_same_frame() {
        let mut palette = CommandPalette::default();
        palette.toggle();
        let mut harness = Harness::new_state(
            |ctx, palette: &mut CommandPalette| {
                palette.show(ctx);
            },
            palette,
        );
        harness
            .input_mut()
            .events
            .push(egui::Event::Text("Export Diagnostics".to_owned()));
        harness.run();
        assert_eq!(harness.state().query, "Export Diagnostics");
        assert_eq!(
            harness.state().filtered_entries()[0].command,
            Command::ExportDiagnostics
        );
        assert!(harness.get_by_label("Export Diagnostics").rect().bottom() < 500.0);
    }
}

impl CommandPalette {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.request_focus = self.open;
        if !self.open {
            self.query.clear();
            self.selected = 0;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected = 0;
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<Command> {
        if !self.open {
            return None;
        }

        let screen = ctx.available_rect();
        let width = (screen.width() - 40.0).min(500.0);

        let backdrop = ctx.layer_painter(egui::LayerId::new(Order::Debug, Id::new("cp-backdrop")));
        backdrop.rect_filled(screen, 0.0, Color32::from_rgba_premultiplied(0, 0, 0, 150));

        let area = Area::new(Id::new("command-palette"))
            .order(Order::Debug)
            .fixed_pos(pos2(screen.center().x - width * 0.5, screen.top() + 80.0));

        let mut command = None;
        area.show(ctx, |ui| {
            Frame::default()
                .fill(palette::SURFACE)
                .stroke(egui::Stroke::new(1.0, palette::LINE))
                .corner_radius(10.0)
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui: &mut egui::Ui| {
                    ui.set_min_width(width);
                    let search = ui.add_sized(
                        vec2(width - 20.0, 28.0),
                        TextEdit::singleline(&mut self.query)
                            .hint_text("Type a command...")
                            .font(FontId::monospace(14.0)),
                    );
                    let focus_requested = std::mem::take(&mut self.request_focus);
                    if focus_requested {
                        search.request_focus();
                    }
                    if search.changed() {
                        self.selected = 0;
                    }
                    let entries = self.filtered_entries();
                    self.selected = self.selected.min(entries.len().saturating_sub(1));
                    let scroll_selection = focus_requested
                        || search.changed()
                        || ui.input(|input| {
                            input.key_pressed(Key::ArrowDown) || input.key_pressed(Key::ArrowUp)
                        });
                    if search.lost_focus()
                        && ui.input(|i: &egui::InputState| i.key_pressed(Key::Escape))
                    {
                        self.close();
                    }

                    if ui.input(|i: &egui::InputState| i.key_pressed(Key::ArrowDown))
                        && !entries.is_empty()
                    {
                        self.selected = (self.selected + 1) % entries.len();
                    }
                    if ui.input(|i: &egui::InputState| i.key_pressed(Key::ArrowUp))
                        && !entries.is_empty()
                    {
                        self.selected = if self.selected == 0 {
                            entries.len() - 1
                        } else {
                            self.selected - 1
                        };
                    }
                    if ui.input(|i: &egui::InputState| i.key_pressed(Key::Enter)) {
                        command = entries.get(self.selected).map(|entry| entry.command);
                    }
                    if ui.input(|i: &egui::InputState| i.key_pressed(Key::Escape)) {
                        self.close();
                    }

                    ui.separator();

                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .id_salt("command-palette-results")
                        .show(ui, |ui| {
                            for (index, entry) in entries.iter().enumerate() {
                                let selected = index == self.selected;
                                let (rect, response) = ui.allocate_exact_size(
                                    vec2(width - 20.0, 32.0),
                                    egui::Sense::click(),
                                );
                                response.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::WidgetType::SelectableLabel,
                                        ui.is_enabled(),
                                        selected,
                                        entry.label,
                                    )
                                });
                                if selected && scroll_selection {
                                    response.scroll_to_me(Some(egui::Align::Center));
                                }
                                if selected {
                                    ui.painter()
                                        .rect_filled(rect.shrink(2.0), 6.0, palette::HOVER);
                                }
                                ui.painter().text(
                                    rect.left_center() + vec2(12.0, 0.0),
                                    Align2::LEFT_CENTER,
                                    format!("{} {}", if selected { "▸" } else { " " }, entry.label),
                                    FontId::proportional(13.0),
                                    Color32::WHITE,
                                );
                                ui.painter().text(
                                    rect.right_center() - vec2(12.0, 0.0),
                                    Align2::RIGHT_CENTER,
                                    entry.shortcut,
                                    FontId::monospace(10.5),
                                    palette::TEXT,
                                );
                                if response.clicked() {
                                    command = Some(entry.command);
                                }
                            }
                        });
                    if entries.is_empty() {
                        ui.label(RichText::new("No commands match").color(palette::TEXT));
                    }
                });
        });

        if command.is_some() {
            self.close();
        }
        command
    }

    pub fn filtered_entries(&self) -> Vec<&'static CommandEntry> {
        let mut entries: Vec<_> = COMMANDS
            .iter()
            .filter(|entry| !self.desktop_mode || entry.command.available_on_desktop())
            .filter_map(|entry| fuzzy_score(&self.query, entry.label).map(|score| (score, entry)))
            .collect();
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.cmp(b.1.label)));
        entries.into_iter().map(|(_, entry)| entry).collect()
    }
}
