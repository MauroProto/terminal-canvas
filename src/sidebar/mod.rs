use egui::{Align2, FontId, RichText, ScrollArea, Sense, Stroke, Ui};

use crate::collab::{CollabMode, CollabSessionState};
use crate::sidebar::workspace_list::draw_workspace_tree;
use crate::state::Workspace;
use crate::theme::colors::{DIM, FOCUS, INK, LINE, RAISED, SURFACE, TEXT, TEXT_STRONG};
use crate::update::UpdateState;

pub mod workspace_list;

pub const SIDEBAR_BG: egui::Color32 = INK;
pub const SIDEBAR_BORDER: egui::Color32 = LINE;
#[allow(dead_code)]
pub const INPUT_BG: egui::Color32 = SURFACE;
#[allow(dead_code)]
pub const ACTIVE_TAB_BG: egui::Color32 = FOCUS;
pub const TEXT_PRIMARY: egui::Color32 = TEXT_STRONG;
pub const TEXT_SECONDARY: egui::Color32 = TEXT;
pub const TEXT_MUTED: egui::Color32 = DIM;
#[allow(dead_code)]
pub const ITEM_BG: egui::Color32 = RAISED;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarTab {
    Workspaces,
    Online,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarResponse {
    SwitchWorkspace(usize),
    OpenFolder,
    DeleteWorkspace(usize),
    OpenShareWorkspace,
    OpenJoinSession,
    OpenCollabSession,
    StopCollabSession,
    FocusPanel(uuid::Uuid),
    SpawnTerminal(usize),
    RenamePanel(uuid::Uuid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SidebarCollabControls {
    state_label: &'static str,
    primary_label: &'static str,
    secondary_label: Option<&'static str>,
}

fn sidebar_collab_controls(mode: CollabMode, state: CollabSessionState) -> SidebarCollabControls {
    let state_label = match state {
        CollabSessionState::NotSharing => "Not sharing",
        CollabSessionState::Starting => "Connecting",
        CollabSessionState::Live => "Live",
        CollabSessionState::Disconnected => "Waiting",
        CollabSessionState::Ended => "Ended",
    };

    match mode {
        CollabMode::Inactive => SidebarCollabControls {
            state_label,
            primary_label: "Share",
            secondary_label: Some("Join"),
        },
        CollabMode::Host => SidebarCollabControls {
            state_label,
            primary_label: "Session",
            secondary_label: Some("Stop"),
        },
        CollabMode::Guest => SidebarCollabControls {
            state_label,
            primary_label: "Session",
            secondary_label: Some("Leave"),
        },
    }
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
        _brand_texture: Option<&egui::TextureHandle>,
        workspaces: &[Workspace],
        active_ws: usize,
        update_state: &UpdateState,
        collab_mode: CollabMode,
        collab_state: CollabSessionState,
    ) -> Vec<SidebarResponse> {
        let mut responses = Vec::new();

        ui.visuals_mut().widgets.noninteractive.bg_fill = SIDEBAR_BG;
        let area = ui.max_rect();
        ui.painter().rect_filled(area, 0.0, SIDEBAR_BG);
        let divider_color = egui::Color32::from_rgb(72, 72, 72);
        let divider_width = 1.5;
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(area.right() - divider_width, area.top()),
                egui::pos2(area.right(), area.bottom()),
            ),
            0.0,
            divider_color,
        );

        let _ = update_state;
        ui.add_space(8.0);
        responses.extend(self.show_tabs(ui));
        ui.add_space(8.0);

        ui.scope(|ui| {
            // Ocultamos el thumb del scrollbar (la "barrita blanca") manteniendo
            // el track. El usuario sigue pudiendo scrollear con rueda/trackpad.
            let visuals = ui.visuals_mut();
            visuals.widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
            visuals.widgets.hovered.bg_fill = egui::Color32::TRANSPARENT;
            visuals.widgets.active.bg_fill = egui::Color32::TRANSPARENT;
            ScrollArea::vertical().show(ui, |ui| match self.active_tab {
                SidebarTab::Workspaces => {
                    responses.extend(draw_workspace_tree(ui, workspaces, active_ws));
                }
                SidebarTab::Online => {
                    responses.extend(self.show_online_panel(ui, collab_mode, collab_state));
                }
            });
        });

        responses
    }

    fn show_tabs(&mut self, ui: &mut Ui) -> Vec<SidebarResponse> {
        let responses = Vec::new();
        let total = ui.available_width();
        let row_height = 28.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(total, row_height), Sense::hover());

        let label_pad_left = 12.0;
        let item_gap = 18.0;

        let entries = [
            (
                SidebarTab::Workspaces,
                "Workspaces",
                "sidebar-tab-workspaces",
            ),
            (SidebarTab::Online, "Online", "sidebar-tab-online"),
        ];

        let painter = ui.painter().clone();
        let font = FontId::proportional(11.5);

        // Pre-medir cada label para construir hit-rects ajustados al texto.
        let measure = |text: &str| -> f32 {
            ui.fonts(|fonts| {
                fonts
                    .layout_no_wrap(text.to_owned(), font.clone(), TEXT_PRIMARY)
                    .size()
                    .x
            })
        };

        let mut cursor_x = rect.left() + label_pad_left;
        for (tab, label, id_seed) in entries {
            let text_width = measure(label);
            let hit_rect = egui::Rect::from_min_size(
                egui::pos2(cursor_x - 6.0, rect.top()),
                egui::vec2(text_width + 12.0, row_height),
            );
            let response = ui.interact(hit_rect, ui.id().with(id_seed), Sense::click());
            if response.clicked() {
                self.active_tab = tab;
            }
            let active = self.active_tab == tab;
            let color = if active {
                TEXT_PRIMARY
            } else if response.hovered() {
                TEXT
            } else {
                TEXT_MUTED
            };
            let baseline = egui::pos2(cursor_x, rect.center().y);
            painter.text(baseline, Align2::LEFT_CENTER, label, font.clone(), color);
            if active {
                let underline_y = rect.bottom() - 4.0;
                painter.line_segment(
                    [
                        egui::pos2(cursor_x, underline_y),
                        egui::pos2(cursor_x + text_width, underline_y),
                    ],
                    Stroke::new(1.5, TEXT_PRIMARY),
                );
            }
            cursor_x += text_width + item_gap;
        }

        responses
    }

    fn show_online_panel(
        &mut self,
        ui: &mut Ui,
        collab_mode: CollabMode,
        collab_state: CollabSessionState,
    ) -> Vec<SidebarResponse> {
        let mut responses = Vec::new();
        let controls = sidebar_collab_controls(collab_mode, collab_state);
        let status_alive = matches!(collab_state, CollabSessionState::Live);
        let description = match collab_mode {
            CollabMode::Inactive => "Compartí este workspace o unite a una sesión existente.",
            CollabMode::Host => "Esta máquina está compartiendo el workspace actual.",
            CollabMode::Guest => "Estás dentro de una sesión remota como invitado.",
        };

        ui.add_space(4.0);
        ui.scope(|ui| {
            ui.style_mut().spacing.item_spacing.x = 6.0;
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                let dot_color = if status_alive {
                    TEXT_PRIMARY
                } else {
                    TEXT_MUTED
                };
                let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), Sense::hover());
                ui.painter()
                    .circle_filled(dot_rect.center(), 3.0, dot_color);
                ui.label(
                    RichText::new(controls.state_label)
                        .size(11.0)
                        .color(if status_alive { TEXT_PRIMARY } else { TEXT }),
                );
            });
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.style_mut().spacing.item_spacing.x = 0.0;
            let avail = (ui.available_width() - 14.0).max(120.0);
            let primary = render_text_link(ui, controls.primary_label, avail * 0.5, true);
            if primary {
                responses.push(match collab_mode {
                    CollabMode::Inactive => SidebarResponse::OpenShareWorkspace,
                    CollabMode::Host | CollabMode::Guest => SidebarResponse::OpenCollabSession,
                });
            }
            if let Some(label) = controls.secondary_label {
                let secondary = render_text_link(ui, label, avail * 0.5, false);
                if secondary {
                    responses.push(match collab_mode {
                        CollabMode::Inactive => SidebarResponse::OpenJoinSession,
                        CollabMode::Host | CollabMode::Guest => SidebarResponse::StopCollabSession,
                    });
                }
            }
        });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.label(RichText::new(description).size(10.5).color(TEXT_MUTED));
        });

        responses
    }
}

fn render_text_link(ui: &mut Ui, label: &str, slot_width: f32, primary: bool) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(slot_width.max(60.0), 26.0), Sense::click());
    let color = if primary {
        if response.hovered() {
            TEXT_PRIMARY
        } else {
            TEXT
        }
    } else if response.hovered() {
        TEXT
    } else {
        TEXT_MUTED
    };
    ui.painter().text(
        egui::pos2(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(11.5),
        color,
    );
    if response.hovered() {
        let underline_y = rect.bottom() - 6.0;
        let text_w = ui.fonts(|f| {
            f.layout_no_wrap(label.to_owned(), FontId::proportional(11.5), color)
                .size()
                .x
        });
        ui.painter().line_segment(
            [
                egui::pos2(rect.left(), underline_y),
                egui::pos2(rect.left() + text_w, underline_y),
            ],
            Stroke::new(1.0, color),
        );
    }
    response.clicked()
}

#[cfg(test)]
mod tests {
    use crate::collab::{CollabMode, CollabSessionState};

    use super::sidebar_collab_controls;

    #[test]
    fn inactive_sidebar_collab_controls_match_share_join_flow() {
        let controls =
            sidebar_collab_controls(CollabMode::Inactive, CollabSessionState::NotSharing);

        assert_eq!(controls.state_label, "Not sharing");
        assert_eq!(controls.primary_label, "Share");
        assert_eq!(controls.secondary_label, Some("Join"));
    }

    #[test]
    fn guest_sidebar_collab_controls_match_session_leave_flow() {
        let controls = sidebar_collab_controls(CollabMode::Guest, CollabSessionState::Live);

        assert_eq!(controls.state_label, "Live");
        assert_eq!(controls.primary_label, "Session");
        assert_eq!(controls.secondary_label, Some("Leave"));
    }
}
