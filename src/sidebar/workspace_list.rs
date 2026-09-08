use std::borrow::Cow;

use egui::{Align2, FontId, Sense, Ui};

use crate::sidebar::{SidebarResponse, SIDEBAR_BORDER, TEXT_MUTED, TEXT_PRIMARY};
use crate::state::Workspace;
use crate::theme::colors::{DIM, FOCUS, RAISED, TEXT};

const ROW_PAD_X: f32 = 14.0;
const ITEM_HEIGHT: f32 = 28.0;
const HEADER_HEIGHT_WITH_PATH: f32 = 46.0;
const HEADER_HEIGHT_PLAIN: f32 = 34.0;
const HEADER_ACTION_SIZE: f32 = 24.0;

pub fn draw_workspace_tree(
    ui: &mut Ui,
    workspaces: &[Workspace],
    active_ws: usize,
) -> Vec<SidebarResponse> {
    let mut responses = Vec::new();

    for (index, workspace) in workspaces.iter().enumerate() {
        ui.add_space(2.0);
        let has_path = workspace.folder_path_label().is_some();
        let header_height = if has_path {
            HEADER_HEIGHT_WITH_PATH
        } else {
            HEADER_HEIGHT_PLAIN
        };
        let (header_rect, header_response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), header_height),
            Sense::click(),
        );

        let active = index == active_ws;
        header_response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                ui.is_enabled(),
                active,
                format!("Workspace {}", workspace.name),
            )
        });
        if active {
            ui.painter().rect_filled(header_rect, 6.0, FOCUS);
        } else if header_response.hovered() {
            ui.painter().rect_filled(header_rect, 6.0, RAISED);
        }

        if active {
            let bar_rect = egui::Rect::from_min_max(
                egui::pos2(header_rect.left() + 4.0, header_rect.top() + 8.0),
                egui::pos2(header_rect.left() + 6.0, header_rect.bottom() - 8.0),
            );
            ui.painter().rect_filled(bar_rect, 1.0, TEXT_PRIMARY);
        }

        let label_x = header_rect.left() + ROW_PAD_X;
        if let Some(path) = workspace.folder_path_label() {
            ui.painter().text(
                egui::pos2(label_x, header_rect.top() + 9.0),
                Align2::LEFT_TOP,
                truncate(&workspace.name, 20),
                FontId::proportional(13.0),
                TEXT_PRIMARY,
            );
            ui.painter().text(
                egui::pos2(label_x, header_rect.bottom() - 9.0),
                Align2::LEFT_BOTTOM,
                truncate_middle(path, 32),
                FontId::proportional(11.0),
                TEXT_MUTED,
            );
        } else {
            ui.painter().text(
                egui::pos2(label_x, header_rect.center().y),
                Align2::LEFT_CENTER,
                truncate(&workspace.name, 22),
                FontId::proportional(13.0),
                TEXT_PRIMARY,
            );
        }

        let can_close = workspace.cwd().is_some() || workspaces.len() > 1;
        let close_rect = egui::Rect::from_center_size(
            egui::pos2(header_rect.right() - 16.0, header_rect.center().y),
            egui::vec2(HEADER_ACTION_SIZE, HEADER_ACTION_SIZE),
        );
        let close_response = can_close.then(|| {
            let response = ui.put(close_rect, egui::Button::new("×").frame(false));
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    ui.is_enabled(),
                    "Cerrar proyecto",
                )
            });
            response.on_hover_text("Cerrar proyecto y sus terminales")
        });
        let add_rect = egui::Rect::from_center_size(
            egui::pos2(
                header_rect.right() - if can_close { 46.0 } else { 16.0 },
                header_rect.center().y,
            ),
            egui::vec2(HEADER_ACTION_SIZE, HEADER_ACTION_SIZE),
        );
        let add_response = ui
            .put(
                add_rect,
                egui::Button::new(egui::RichText::new("+").size(15.0)).frame(false),
            )
            .on_hover_text("Nuevo terminal en este workspace");
        add_response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                format!("Nuevo terminal en {}", workspace.name),
            )
        });

        let close_hovered = close_response
            .as_ref()
            .map(egui::Response::hovered)
            .unwrap_or(false);
        if header_response.clicked() && !add_response.hovered() && !close_hovered {
            responses.push(SidebarResponse::SwitchWorkspace(index));
        }
        if add_response.clicked() {
            responses.push(SidebarResponse::SpawnTerminal(index));
        }
        if close_response.is_some_and(|response| response.clicked()) {
            responses.push(SidebarResponse::RequestCloseWorkspace(workspace.id));
        }

        for panel in &workspace.panels {
            let (item_rect, item_response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), ITEM_HEIGHT),
                Sense::click(),
            );
            item_response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    ui.is_enabled(),
                    format!("Abrir terminal {}", panel.title()),
                )
            });
            if item_response.hovered() {
                ui.painter().rect_filled(item_rect, 4.0, RAISED);
            }
            let dot_center = egui::pos2(item_rect.left() + 18.0, item_rect.center().y);
            let dot_color = if panel.is_alive() { TEXT } else { DIM };
            ui.painter().circle_filled(dot_center, 2.5, dot_color);
            ui.painter().text(
                egui::pos2(item_rect.left() + 30.0, item_rect.center().y),
                Align2::LEFT_CENTER,
                truncate(panel.title(), 24),
                FontId::proportional(12.0),
                if panel.is_alive() {
                    TEXT_PRIMARY
                } else {
                    TEXT_MUTED
                },
            );
            if item_response.clicked() {
                responses.push(SidebarResponse::FocusPanel(panel.id()));
            }
            if item_response.secondary_clicked() {
                responses.push(SidebarResponse::RenamePanel(panel.id()));
            }
        }

        if index < workspaces.len().saturating_sub(1) {
            ui.add_space(6.0);
            let divider_y = ui.cursor().min.y;
            let inset = 12.0;
            ui.painter().line_segment(
                [
                    egui::pos2(ui.min_rect().left() + inset, divider_y),
                    egui::pos2(ui.min_rect().right() - inset, divider_y),
                ],
                egui::Stroke::new(1.0_f32, SIDEBAR_BORDER),
            );
            ui.add_space(2.0);
        } else {
            ui.add_space(6.0);
        }
    }

    ui.add_space(6.0);
    let (new_rect, new_response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), Sense::click());
    new_response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "Abrir carpeta")
    });
    if new_response.hovered() {
        ui.painter().rect_filled(new_rect, 4.0, RAISED);
    }
    let new_color = if new_response.hovered() {
        TEXT_PRIMARY
    } else {
        TEXT_MUTED
    };
    ui.painter().text(
        egui::pos2(new_rect.left() + ROW_PAD_X, new_rect.center().y),
        Align2::LEFT_CENTER,
        "+ Open folder",
        FontId::proportional(12.0),
        new_color,
    );
    if new_response.clicked() {
        responses.push(SidebarResponse::OpenFolder);
    }

    responses
}

fn truncate(text: &str, max_chars: usize) -> Cow<'_, str> {
    let count = text.chars().count();
    if count <= max_chars {
        Cow::Borrowed(text)
    } else {
        let mut s: String = text.chars().take(max_chars).collect();
        s.push_str("...");
        Cow::Owned(s)
    }
}

fn truncate_middle(text: &str, max_chars: usize) -> Cow<'_, str> {
    let count = text.chars().count();
    if count <= max_chars || max_chars <= 3 {
        return Cow::Borrowed(text);
    }

    let left_count = (max_chars - 3) / 2;
    let right_count = max_chars - 3 - left_count;
    let mut result = String::with_capacity(max_chars * 4);
    result.extend(text.chars().take(left_count));
    result.push_str("...");
    result.extend(text.chars().skip(count - right_count));
    Cow::Owned(result)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use egui_kittest::kittest::Queryable as _;

    use super::draw_workspace_tree;
    use crate::sidebar::SidebarResponse;
    use crate::state::Workspace;

    #[test]
    fn a_project_has_a_visible_close_button_that_only_requests_confirmation() {
        let workspace = Workspace::from_folder(std::path::PathBuf::from("/tmp/project"));
        let workspace_id = workspace.id;
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_from_ui = Arc::clone(&captured);
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            captured_from_ui.lock().unwrap().extend(draw_workspace_tree(
                ui,
                std::slice::from_ref(&workspace),
                0,
            ));
        });

        harness.get_by_label("Cerrar proyecto").click();
        harness.run();

        assert!(captured
            .lock()
            .unwrap()
            .iter()
            .any(|response| { response == &SidebarResponse::RequestCloseWorkspace(workspace_id) }));
    }
}
