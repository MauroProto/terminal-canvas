use egui::{Align2, Color32, FontId, RichText, ScrollArea, Sense, Stroke, Ui};

use crate::sidebar::terminal_list::draw_terminal_list;
use crate::sidebar::workspace_list::draw_workspace_tree;
use crate::state::Workspace;
use crate::update::{UpdateState, UpdateStatus};

pub mod terminal_list;
pub mod workspace_list;

pub const SIDEBAR_BG: Color32 = Color32::from_rgb(23, 23, 23);
pub const SIDEBAR_BORDER: Color32 = Color32::from_rgb(38, 38, 38);
pub const INPUT_BG: Color32 = Color32::from_rgb(39, 39, 42);
pub const ACTIVE_TAB_BG: Color32 = Color32::from_rgb(63, 63, 70);
pub const TEXT_PRIMARY: Color32 = Color32::WHITE;
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(163, 163, 163);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(115, 115, 115);
pub const HOVER_BG: Color32 = Color32::from_rgba_premultiplied(39, 39, 42, 120);
pub const ITEM_BG: Color32 = Color32::from_rgb(39, 39, 42);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarTab {
    Workspaces,
    Terminals,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarResponse {
    SwitchWorkspace(usize),
    OpenFolder,
    DeleteWorkspace(usize),
    FocusPanel(uuid::Uuid),
    SpawnTerminal(usize),
    RenamePanel(uuid::Uuid),
    ClosePanel(uuid::Uuid),
}

pub struct Sidebar {
    pub active_tab: SidebarTab,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self {
            active_tab: SidebarTab::Workspaces,
        }
    }
}

impl Sidebar {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        brand_texture: Option<&egui::TextureHandle>,
        workspaces: &[Workspace],
        active_ws: usize,
        update_state: &UpdateState,
    ) -> Vec<SidebarResponse> {
        let mut responses = Vec::new();

        ui.visuals_mut().widgets.noninteractive.bg_fill = SIDEBAR_BG;
        ui.painter().rect_filled(ui.max_rect(), 0.0, SIDEBAR_BG);

        ui.horizontal(|ui| {
            if let Some(texture) = brand_texture {
                ui.add(
                    egui::Image::new(texture)
                        .max_size(egui::vec2(16.0, 16.0))
                        .tint(TEXT_SECONDARY),
                );
            }
            ui.label(
                RichText::new("My Terminal")
                    .color(TEXT_PRIMARY)
                    .size(13.0)
                    .strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match &update_state.status {
                    UpdateStatus::Available => {
                        ui.label(
                            RichText::new("Update")
                                .color(Color32::from_rgb(90, 180, 90))
                                .size(11.0),
                        );
                    }
                    UpdateStatus::Downloading => {
                        ui.label(
                            RichText::new("Downloading...")
                                .color(Color32::from_rgb(200, 160, 60))
                                .size(11.0),
                        );
                    }
                    UpdateStatus::Ready => {
                        ui.label(
                            RichText::new("Ready")
                                .color(Color32::from_rgb(90, 180, 90))
                                .size(11.0),
                        );
                    }
                    _ => {
                        ui.label(
                            RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                                .color(TEXT_MUTED)
                                .size(11.0),
                        );
                    }
                }
            });
        });

        ui.add_space(8.0);
        self.show_tabs(ui);
        ui.add_space(8.0);

        ScrollArea::vertical().show(ui, |ui| match self.active_tab {
            SidebarTab::Workspaces => {
                responses.extend(draw_workspace_tree(ui, workspaces, active_ws));
            }
            SidebarTab::Terminals => {
                if let Some(ws) = workspaces.get(active_ws) {
                    responses.extend(draw_terminal_list(ui, ws));
                }
            }
        });

        ui.add_space(10.0);
        ui.painter().text(
            ui.max_rect().center_bottom() - egui::vec2(0.0, 8.0),
            Align2::CENTER_BOTTOM,
            "Ctrl+Shift+O folder · Ctrl+Shift+T terminal · Ctrl+B sidebar",
            FontId::proportional(9.5),
            TEXT_MUTED,
        );

        responses
    }

    fn show_tabs(&mut self, ui: &mut Ui) {
        let total = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(total, 34.0), Sense::hover());
        ui.painter().rect_filled(rect, 8.0, INPUT_BG);

        let half = rect.width() * 0.5;
        let workspaces =
            egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + half, rect.bottom()));
        let terminals =
            egui::Rect::from_min_max(egui::pos2(rect.left() + half, rect.top()), rect.max);
        let active_rect = match self.active_tab {
            SidebarTab::Workspaces => workspaces.shrink(2.0),
            SidebarTab::Terminals => terminals.shrink(2.0),
        };
        ui.painter().rect_filled(active_rect, 6.0, ACTIVE_TAB_BG);
        ui.painter().line_segment(
            [
                egui::pos2(rect.left() + half, rect.top() + 6.0),
                egui::pos2(rect.left() + half, rect.bottom() - 6.0),
            ],
            Stroke::new(1.0, SIDEBAR_BORDER),
        );

        if ui
            .interact(
                workspaces,
                ui.id().with("sidebar-tab-workspaces"),
                Sense::click(),
            )
            .clicked()
        {
            self.active_tab = SidebarTab::Workspaces;
        }
        if ui
            .interact(
                terminals,
                ui.id().with("sidebar-tab-terminals"),
                Sense::click(),
            )
            .clicked()
        {
            self.active_tab = SidebarTab::Terminals;
        }

        ui.painter().text(
            workspaces.center(),
            Align2::CENTER_CENTER,
            "Workspaces",
            FontId::proportional(11.5),
            TEXT_PRIMARY,
        );
        ui.painter().text(
            terminals.center(),
            Align2::CENTER_CENTER,
            "Terminals",
            FontId::proportional(11.5),
            TEXT_PRIMARY,
        );
    }
}
