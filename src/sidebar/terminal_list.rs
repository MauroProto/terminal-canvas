use egui::{Align2, Color32, FontId, Sense, Ui};

use crate::sidebar::{SidebarResponse, HOVER_BG, ITEM_BG, TEXT_PRIMARY, TEXT_SECONDARY};
use crate::state::Workspace;

pub fn draw_terminal_list(ui: &mut Ui, workspace: &Workspace) -> Vec<SidebarResponse> {
    let mut responses = Vec::new();

    for panel in &workspace.panels {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), Sense::click());
        if panel.focused() {
            ui.painter().rect_filled(rect, 8.0, ITEM_BG);
        } else if response.hovered() {
            ui.painter().rect_filled(rect, 8.0, HOVER_BG);
        }
        let color = if panel.is_alive() {
            panel.color()
        } else {
            Color32::from_rgb(90, 90, 90)
        };
        ui.painter()
            .circle_filled(rect.left_center() + egui::vec2(10.0, 0.0), 3.0, color);
        ui.painter().text(
            rect.left_center() + egui::vec2(22.0, 0.0),
            Align2::LEFT_CENTER,
            truncate(panel.title(), 24),
            FontId::proportional(11.0),
            if panel.focused() {
                TEXT_PRIMARY
            } else if panel.minimized() {
                Color32::from_rgb(126, 126, 134)
            } else {
                TEXT_SECONDARY
            },
        );
        if response.clicked() {
            responses.push(SidebarResponse::FocusPanel(panel.id()));
        }
        if response.secondary_clicked() {
            responses.push(SidebarResponse::RenamePanel(panel.id()));
        }
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
