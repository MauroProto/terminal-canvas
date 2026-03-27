use egui::{pos2, vec2, Align2, Area, Color32, FontId, Frame, Id, Key, Order, RichText, TextEdit};

use crate::command_palette::commands::{Command, CommandEntry, COMMANDS};
use crate::command_palette::fuzzy::fuzzy_score;

pub mod commands;
pub mod fuzzy;

#[derive(Default)]
pub struct CommandPalette {
    pub open: bool,
    pub query: String,
    pub selected: usize,
}

impl CommandPalette {
    pub fn toggle(&mut self) {
        self.open = !self.open;
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
        let entries = self.filtered_entries();
        self.selected = self.selected.min(entries.len().saturating_sub(1));

        let backdrop = ctx.layer_painter(egui::LayerId::new(Order::Debug, Id::new("cp-backdrop")));
        backdrop.rect_filled(screen, 0.0, Color32::from_rgba_premultiplied(0, 0, 0, 150));

        let area = Area::new(Id::new("command-palette"))
            .order(Order::Debug)
            .fixed_pos(pos2(screen.center().x - width * 0.5, screen.top() + 80.0));

        let mut command = None;
        area.show(ctx, |ui| {
            Frame::default()
                .fill(Color32::from_rgb(24, 24, 28))
                .stroke(egui::Stroke::new(1.0, Color32::from_rgb(55, 55, 65)))
                .rounding(10.0)
                .inner_margin(egui::Margin::same(10.0))
                .show(ui, |ui: &mut egui::Ui| {
                    ui.set_min_width(width);
                    let search = ui.add_sized(
                        vec2(width - 20.0, 28.0),
                        TextEdit::singleline(&mut self.query)
                            .hint_text("Type a command...")
                            .font(FontId::monospace(14.0)),
                    );
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

                    for (index, entry) in entries.iter().take(10).enumerate() {
                        let selected = index == self.selected;
                        let (rect, response) =
                            ui.allocate_exact_size(vec2(width - 20.0, 32.0), egui::Sense::click());
                        if selected {
                            ui.painter().rect_filled(
                                rect.shrink(2.0),
                                6.0,
                                Color32::from_rgb(45, 45, 62),
                            );
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
                            Color32::from_rgb(163, 163, 163),
                        );
                        if response.clicked() {
                            command = Some(entry.command);
                        }
                    }
                    if entries.is_empty() {
                        ui.label(
                            RichText::new("No commands match")
                                .color(Color32::from_rgb(163, 163, 163)),
                        );
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
            .filter_map(|entry| fuzzy_score(&self.query, entry.label).map(|score| (score, entry)))
            .collect();
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.cmp(b.1.label)));
        entries.into_iter().map(|(_, entry)| entry).collect()
    }
}
