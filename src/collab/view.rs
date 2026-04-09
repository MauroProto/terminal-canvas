use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Stroke};

use crate::canvas::viewport::Viewport;

use super::{CollabSessionState, GuestId, SharedWorkspaceSnapshot};

const PANEL_BG: Color32 = Color32::from_rgb(30, 30, 30);
const TITLE_BG: Color32 = Color32::from_rgb(42, 44, 52);
const BORDER: Color32 = Color32::from_rgb(95, 97, 110);
const FG: Color32 = Color32::from_rgb(220, 220, 220);
const MUTED: Color32 = Color32::from_rgb(150, 150, 150);
const ACCENT: Color32 = Color32::from_rgb(116, 147, 255);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePanelAction {
    Focus(uuid::Uuid),
    RequestControl(uuid::Uuid),
}

pub fn draw_remote_workspace(
    ui: &mut egui::Ui,
    snapshot: &SharedWorkspaceSnapshot,
    viewport: &Viewport,
    canvas_rect: Rect,
    session_state: CollabSessionState,
    focused_panel: Option<uuid::Uuid>,
    my_guest_id: Option<GuestId>,
    scroll_offsets: &std::collections::HashMap<uuid::Uuid, usize>,
) -> Option<RemotePanelAction> {
    let painter = ui.painter().with_clip_rect(canvas_rect);
    let mut action = None;

    let mut panels = snapshot.panels.clone();
    panels.sort_by_key(|panel| panel.z_index);

    for panel in panels {
        if panel.minimized {
            continue;
        }
        let rect = Rect::from_min_size(
            pos2(panel.position[0], panel.position[1]),
            vec2(panel.size[0], panel.size[1]),
        );
        let screen_rect = Rect::from_min_size(
            viewport.canvas_to_screen(rect.min, canvas_rect),
            rect.size() * viewport.zoom,
        );
        if !screen_rect.intersects(canvas_rect) {
            continue;
        }
        let title_height = (34.0 * viewport.zoom).clamp(18.0, 34.0);
        let title_rect = Rect::from_min_max(
            screen_rect.min,
            pos2(screen_rect.right(), screen_rect.top() + title_height),
        );
        let body_rect = Rect::from_min_max(
            pos2(screen_rect.left(), title_rect.bottom()),
            screen_rect.max,
        );
        painter.rect_filled(screen_rect, 20.0 * viewport.zoom.clamp(0.3, 1.0), PANEL_BG);
        painter.rect_filled(title_rect, 20.0 * viewport.zoom.clamp(0.3, 1.0), TITLE_BG);
        painter.rect_stroke(
            screen_rect.shrink(0.5),
            20.0 * viewport.zoom.clamp(0.3, 1.0),
            Stroke::new(
                if focused_panel == Some(panel.panel_id) {
                    1.5
                } else {
                    1.0
                },
                if focused_panel == Some(panel.panel_id) {
                    ACCENT
                } else {
                    BORDER
                },
            ),
        );

        painter.text(
            title_rect.left_center() + vec2(16.0, 0.0),
            Align2::LEFT_CENTER,
            &panel.title,
            FontId::proportional((15.0 * viewport.zoom).clamp(8.0, 15.0)),
            FG,
        );

        let control_label = match (panel.controller, my_guest_id, session_state) {
            (_, _, CollabSessionState::Disconnected) => Some("Host reconnecting"),
            (_, _, CollabSessionState::Starting) => Some("Connecting"),
            (_, _, CollabSessionState::Ended) => Some("Session ended"),
            (Some(controller), Some(me), _) if controller == me => Some("You control"),
            (Some(_), _, _) => Some(panel.controller_name.as_deref().unwrap_or("Controlled")),
            (None, _, _) => Some("Available"),
        };
        if let Some(control_label) = control_label {
            painter.text(
                title_rect.right_center() - vec2(14.0, 0.0),
                Align2::RIGHT_CENTER,
                control_label,
                FontId::proportional((10.0 * viewport.zoom).clamp(6.0, 10.0)),
                MUTED,
            );
        }

        let body_id = ui.id().with(("remote-panel", panel.panel_id));
        let response = ui.interact(
            body_rect.intersect(canvas_rect),
            body_id,
            egui::Sense::click(),
        );
        if response.clicked() {
            action = Some(RemotePanelAction::Focus(panel.panel_id));
        }

        let mut lines: Vec<&str> = panel.history_text.lines().collect();
        if lines.is_empty() {
            lines = panel.visible_text.lines().collect();
        }
        let scroll_offset = scroll_offsets.get(&panel.panel_id).copied().unwrap_or(0);
        let start = scroll_offset.min(lines.len());
        let visible = &lines[start..];
        let mut y = body_rect.top() + 12.0;
        let line_height = (14.0 * viewport.zoom).clamp(6.0, 14.0);
        for line in visible
            .iter()
            .take(((body_rect.height() - 24.0) / line_height) as usize)
        {
            painter.text(
                pos2(body_rect.left() + 14.0, y),
                Align2::LEFT_TOP,
                line,
                FontId::monospace((13.0 * viewport.zoom).clamp(6.0, 13.0)),
                FG,
            );
            y += line_height;
        }

        if panel.controller != my_guest_id && matches!(session_state, CollabSessionState::Live) {
            let cta_rect = Rect::from_min_size(
                pos2(body_rect.right() - 138.0, body_rect.bottom() - 38.0),
                vec2(124.0, 26.0),
            );
            let cta_response = ui.interact(
                cta_rect.intersect(canvas_rect),
                ui.id().with(("request-control", panel.panel_id)),
                egui::Sense::click(),
            );
            painter.rect_filled(
                cta_rect,
                13.0,
                Color32::from_rgba_premultiplied(22, 24, 34, 220),
            );
            painter.rect_stroke(cta_rect, 13.0, Stroke::new(1.0, ACCENT));
            painter.text(
                cta_rect.center(),
                Align2::CENTER_CENTER,
                "Request control",
                FontId::proportional((11.0 * viewport.zoom).clamp(8.0, 11.0)),
                FG,
            );
            if cta_response.clicked() {
                action = Some(RemotePanelAction::RequestControl(panel.panel_id));
            }
        }
    }

    action
}
