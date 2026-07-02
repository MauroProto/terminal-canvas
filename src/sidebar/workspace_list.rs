use std::borrow::Cow;

use egui::{Align2, FontId, Sense, Ui};

use crate::sidebar::{SidebarResponse, SIDEBAR_BORDER, TEXT_MUTED, TEXT_PRIMARY, TEXT_SECONDARY};
use crate::state::Workspace;
use crate::theme::colors::{DIM, FOCUS, RAISED, TEXT};

const ROW_PAD_X: f32 = 14.0;
const ITEM_HEIGHT: f32 = 26.0;
const HEADER_HEIGHT_WITH_PATH: f32 = 44.0;
const HEADER_HEIGHT_PLAIN: f32 = 30.0;

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
                FontId::proportional(12.0),
                TEXT_PRIMARY,
            );
            ui.painter().text(
                egui::pos2(label_x, header_rect.bottom() - 9.0),
                Align2::LEFT_BOTTOM,
                truncate_middle(path, 32),
                FontId::proportional(9.5),
                TEXT_MUTED,
            );
        } else {
            ui.painter().text(
                egui::pos2(label_x, header_rect.center().y),
                Align2::LEFT_CENTER,
                truncate(&workspace.name, 22),
                FontId::proportional(12.0),
                TEXT_PRIMARY,
            );
        }

        let add_rect = egui::Rect::from_center_size(
            egui::pos2(header_rect.right() - 14.0, header_rect.center().y),
            egui::vec2(18.0, 18.0),
        );
        let add_response = ui.interact(add_rect, ui.id().with(("ws-add", index)), Sense::click());
        let add_color = if add_response.hovered() {
            TEXT_PRIMARY
        } else {
            TEXT_SECONDARY
        };
        ui.painter().text(
            add_rect.center(),
            Align2::CENTER_CENTER,
            "+",
            FontId::proportional(15.0),
            add_color,
        );

        if header_response.clicked() && !add_response.hovered() {
            responses.push(SidebarResponse::SwitchWorkspace(index));
        }
        if header_response.secondary_clicked() && workspaces.len() > 1 {
            responses.push(SidebarResponse::DeleteWorkspace(index));
        }
        if add_response.clicked() {
            responses.push(SidebarResponse::SpawnTerminal(index));
        }

        for panel in &workspace.panels {
            let (item_rect, item_response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), ITEM_HEIGHT),
                Sense::click(),
            );
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
                FontId::proportional(11.0),
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
                egui::Stroke::new(1.0, SIDEBAR_BORDER),
            );
            ui.add_space(2.0);
        } else {
            ui.add_space(6.0);
        }
    }

    ui.add_space(6.0);
    let (new_rect, new_response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ITEM_HEIGHT),
        Sense::click(),
    );
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
        FontId::proportional(11.0),
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
