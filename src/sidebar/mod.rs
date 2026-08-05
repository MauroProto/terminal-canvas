use egui::{Align2, FontId, RichText, ScrollArea, Sense, Stroke, Ui};

use crate::collab::{CollabMode, CollabSessionState};
use crate::sidebar::workspace_list::draw_workspace_tree;
use crate::state::Workspace;
use crate::theme::colors::{DIM, FOCUS, INK, LINE, RAISED, SURFACE, TEXT, TEXT_STRONG};
use crate::update::UpdateState;

pub mod file_tree;
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
    Files,
    Online,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    ReviewPanelChanges(uuid::Uuid),
    OpenSettings,
    OpenBroadcast,
    ExportScrollback,
    /// Abrir este archivo en el visor interno (desde el explorador).
    OpenFileInViewer(std::path::PathBuf),
}

/// Acciones del pie del sidebar. Están acá (y no sólo en la paleta de comandos
/// y los atajos) porque si no hay un botón visible, la función no existe para
/// quien no se aprendió el atajo.
///
/// Los iconos se limitan a glifos que las fuentes por defecto de egui sí
/// traen: con símbolos exóticos (U+21C9, U+2B33) salía el cuadrito vacío.
fn footer_actions() -> [(&'static str, &'static str, SidebarResponse); 3] {
    [
        ("⚙", "Settings", SidebarResponse::OpenSettings),
        ("»", "Broadcast", SidebarResponse::OpenBroadcast),
        ("↓", "Export output", SidebarResponse::ExportScrollback),
    ]
}
const FOOTER_ACTION_COUNT: usize = 3;

/// Sesión de agente que pide atención (esperando aprobación, input o fallida);
/// el sidebar la lista con acción de foco directo.
#[derive(Debug, Clone)]
pub struct AttentionItem {
    pub panel_id: uuid::Uuid,
    pub label: String,
    pub provider: &'static str,
    pub status: &'static str,
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
        attention: &[AttentionItem],
        file_tree: &mut file_tree::FileTreeState,
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

        // Reservamos el alto del pie para que la lista no lo tape.
        let footer_height = FOOTER_ACTION_COUNT as f32 * FOOTER_ROW_HEIGHT + 16.0;
        let scroll_height = (ui.available_height() - footer_height).max(0.0);

        ui.scope(|ui| {
            // Ocultamos el thumb del scrollbar (la "barrita blanca") manteniendo
            // el track. El usuario sigue pudiendo scrollear con rueda/trackpad.
            let visuals = ui.visuals_mut();
            visuals.widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
            visuals.widgets.hovered.bg_fill = egui::Color32::TRANSPARENT;
            visuals.widgets.active.bg_fill = egui::Color32::TRANSPARENT;
            ScrollArea::vertical()
                .max_height(scroll_height)
                .show(ui, |ui| match self.active_tab {
                    SidebarTab::Workspaces => {
                        if !attention.is_empty() {
                            responses.extend(draw_attention_section(ui, attention));
                        }
                        responses.extend(draw_workspace_tree(ui, workspaces, active_ws));
                    }
                    SidebarTab::Files => {
                        responses.extend(file_tree::draw_file_tree(ui, file_tree));
                    }
                    SidebarTab::Online => {
                        responses.extend(self.show_online_panel(ui, collab_mode, collab_state));
                    }
                });
        });

        // El pie queda pegado al fondo del sidebar, no debajo de la lista.
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            responses.extend(draw_sidebar_footer(ui));
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
            (SidebarTab::Files, "Files", "sidebar-tab-files"),
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

/// Lista de sesiones que piden atención: un click lleva el foco al panel.
fn draw_attention_section(ui: &mut Ui, attention: &[AttentionItem]) -> Vec<SidebarResponse> {
    let mut responses = Vec::new();
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.label(
            RichText::new(format!("Atención ({})", attention.len()))
                .size(11.0)
                .color(TEXT_PRIMARY),
        );
    });
    ui.add_space(2.0);
    for item in attention.iter().take(8) {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), Sense::click());
        if response.hovered() {
            ui.painter().rect_filled(rect.shrink(1.0), 4.0, ITEM_BG);
        }
        let dot_color = if item.status == "Failed" {
            egui::Color32::from_rgb(224, 108, 108)
        } else {
            egui::Color32::from_rgb(222, 178, 92)
        };
        ui.painter().circle_filled(
            egui::pos2(rect.left() + 18.0, rect.center().y),
            3.0,
            dot_color,
        );
        let label = if item.label.trim().is_empty() {
            format!("{} · {}", item.provider, item.status)
        } else {
            format!(
                "{} · {}",
                truncate_sidebar_label(&item.label, 18),
                item.status
            )
        };
        ui.painter().text(
            egui::pos2(rect.left() + 30.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(11.0),
            if response.hovered() {
                TEXT_PRIMARY
            } else {
                TEXT
            },
        );
        // Botón "diff": abre el code review de esa sesión.
        let diff_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 52.0, rect.center().y - 9.0),
            egui::vec2(44.0, 18.0),
        );
        let diff_response = ui.interact(
            diff_rect,
            ui.id().with(("attention-diff", item.panel_id)),
            Sense::click(),
        );
        let diff_color = if diff_response.hovered() {
            TEXT_PRIMARY
        } else {
            TEXT_MUTED
        };
        ui.painter()
            .rect_stroke(diff_rect, 4.0, Stroke::new(1.0, diff_color));
        ui.painter().text(
            diff_rect.center(),
            Align2::CENTER_CENTER,
            "diff",
            FontId::monospace(9.5),
            diff_color,
        );
        if diff_response.clicked() {
            responses.push(SidebarResponse::ReviewPanelChanges(item.panel_id));
        } else if response.clicked() {
            responses.push(SidebarResponse::FocusPanel(item.panel_id));
        }
    }
    ui.add_space(8.0);
    responses
}

fn truncate_sidebar_label(label: &str, max_chars: usize) -> String {
    let count = label.chars().count();
    if count <= max_chars {
        label.to_owned()
    } else {
        format!(
            "{}…",
            label
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    }
}

const FOOTER_ROW_HEIGHT: f32 = 26.0;

/// Fila de acciones al pie del sidebar. `bottom_up` hace que se dibujen de
/// abajo hacia arriba, así que iteramos al revés para que queden en el orden
/// declarado en `FOOTER_ACTIONS`.
fn draw_sidebar_footer(ui: &mut Ui) -> Vec<SidebarResponse> {
    let mut responses = Vec::new();
    ui.add_space(8.0);
    let width = ui.available_width();
    for (icon, label, response) in footer_actions().into_iter().rev() {
        if footer_button(ui, icon, label, width) {
            responses.push(response);
        }
    }
    responses
}

fn footer_button(ui: &mut Ui, icon: &str, label: &str, width: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width.max(60.0), FOOTER_ROW_HEIGHT),
        Sense::click(),
    );
    if response.hovered() {
        ui.painter()
            .rect_filled(rect.shrink2(egui::vec2(4.0, 2.0)), 5.0, RAISED);
    }
    let color = if response.hovered() {
        TEXT_PRIMARY
    } else {
        TEXT_MUTED
    };
    ui.painter().text(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        icon,
        FontId::proportional(12.5),
        color,
    );
    ui.painter().text(
        egui::pos2(rect.left() + 28.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(11.5),
        color,
    );
    response.clicked()
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
