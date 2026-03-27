use egui::{Align2, Color32, FontId, Sense, Ui};

use crate::sidebar::{
    SidebarResponse, ITEM_BG, SIDEBAR_BORDER, TEXT_MUTED, TEXT_PRIMARY, TEXT_SECONDARY,
};
use crate::state::Workspace;

pub fn draw_workspace_tree(
    ui: &mut Ui,
    workspaces: &[Workspace],
    active_ws: usize,
) -> Vec<SidebarResponse> {
    let mut responses = Vec::new();

    for (index, workspace) in workspaces.iter().enumerate() {
        ui.add_space(6.0);
        let has_path = workspace.folder_path_label().is_some();
        let header_height = if has_path { 40.0 } else { 28.0 };
        let (header_rect, header_response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), header_height),
            Sense::click(),
        );
        if index == active_ws {
            ui.painter().rect_filled(header_rect, 8.0, ITEM_BG);
        }

        if let Some(path) = workspace.folder_path_label() {
            ui.painter().text(
                header_rect.left_top() + egui::vec2(10.0, 8.0),
                Align2::LEFT_TOP,
                truncate(&workspace.name, 20),
                FontId::proportional(11.5),
                TEXT_PRIMARY,
            );
            ui.painter().text(
                header_rect.left_bottom() + egui::vec2(10.0, -8.0),
                Align2::LEFT_BOTTOM,
                truncate_middle(path, 30),
                FontId::proportional(10.0),
                TEXT_MUTED,
            );
        } else {
            ui.painter().text(
                header_rect.left_center() + egui::vec2(10.0, 0.0),
                Align2::LEFT_CENTER,
                truncate(&workspace.name, 20),
                FontId::proportional(11.5),
                TEXT_PRIMARY,
            );
        }
        ui.painter().text(
            header_rect.right_center() - egui::vec2(12.0, 0.0),
            Align2::RIGHT_CENTER,
            "+",
            FontId::proportional(16.0),
            TEXT_SECONDARY,
        );
        if header_response.clicked() {
            responses.push(SidebarResponse::SwitchWorkspace(index));
        }
        if header_response.secondary_clicked() && workspaces.len() > 1 {
            responses.push(SidebarResponse::DeleteWorkspace(index));
        }

        let add_rect = egui::Rect::from_center_size(
            header_rect.right_center() - egui::vec2(12.0, 0.0),
            egui::vec2(20.0, 20.0),
        );
        if ui
            .interact(add_rect, ui.id().with(("ws-add", index)), Sense::click())
            .clicked()
        {
            responses.push(SidebarResponse::SpawnTerminal(index));
        }

        for panel in &workspace.panels {
            let (item_rect, item_response) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), Sense::click());
            let dot = egui::Rect::from_center_size(
                item_rect.left_center() + egui::vec2(10.0, 0.0),
                egui::vec2(6.0, 6.0),
            );
            let color = if panel.is_alive() {
                panel.color()
            } else {
                Color32::from_rgb(90, 90, 90)
            };
            ui.painter().circle_filled(dot.center(), 3.0, color);
            ui.painter().text(
                item_rect.left_center() + egui::vec2(22.0, 0.0),
                Align2::LEFT_CENTER,
                truncate(panel.title(), 22),
                FontId::proportional(11.0),
                TEXT_SECONDARY,
            );
            if item_response.clicked() {
                responses.push(SidebarResponse::FocusPanel(panel.id()));
            }
            if item_response.secondary_clicked() {
                responses.push(SidebarResponse::RenamePanel(panel.id()));
            }
        }

        let divider_y = ui.cursor().min.y + 4.0;
        ui.painter().line_segment(
            [
                egui::pos2(ui.min_rect().left(), divider_y),
                egui::pos2(ui.min_rect().right(), divider_y),
            ],
            egui::Stroke::new(1.0, SIDEBAR_BORDER),
        );
    }

    ui.add_space(8.0);
    let (new_rect, new_response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), Sense::click());
    ui.painter().text(
        new_rect.left_center() + egui::vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        "+ Open folder",
        FontId::proportional(11.0),
        TEXT_MUTED,
    );
    if new_response.clicked() {
        responses.push(SidebarResponse::OpenFolder);
    }

    responses
}

fn truncate(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        text.to_owned()
    } else {
        text.chars().take(max_chars).collect::<String>() + "..."
    }
}

fn truncate_middle(text: &str, max_chars: usize) -> String {
    let chars: Vec<_> = text.chars().collect();
    if chars.len() <= max_chars || max_chars <= 3 {
        return text.to_owned();
    }

    let left = (max_chars - 3) / 2;
    let right = max_chars - 3 - left;
    let start: String = chars.iter().take(left).collect();
    let end: String = chars.iter().rev().take(right).rev().collect();
    format!("{start}...{end}")
}
