use egui::{pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui};

use crate::canvas::config::{
    MINIMAP_BG, MINIMAP_HEIGHT, MINIMAP_PADDING, MINIMAP_VIEWPORT_BORDER, MINIMAP_WIDTH,
};
use crate::canvas::viewport::Viewport;
use crate::panel::CanvasPanel;

#[derive(Debug, Clone, Copy, Default)]
pub struct MinimapResult {
    pub navigate_to: Option<Pos2>,
    pub hide_clicked: bool,
    pub focus_all_clicked: bool,
}

pub fn minimap_to_canvas(minimap_pos: Pos2, minimap_rect: Rect, canvas_bounds: Rect) -> Pos2 {
    let normalized = pos2(
        (minimap_pos.x - minimap_rect.left()) / minimap_rect.width(),
        (minimap_pos.y - minimap_rect.top()) / minimap_rect.height(),
    );
    pos2(
        canvas_bounds.left() + normalized.x * canvas_bounds.width(),
        canvas_bounds.top() + normalized.y * canvas_bounds.height(),
    )
}

pub fn show(
    ui: &mut Ui,
    panels: &[CanvasPanel],
    viewport: &Viewport,
    canvas_rect: Rect,
) -> MinimapResult {
    let rect = Rect::from_min_size(
        pos2(
            canvas_rect.right() - MINIMAP_WIDTH - MINIMAP_PADDING,
            canvas_rect.bottom() - MINIMAP_HEIGHT - MINIMAP_PADDING,
        ),
        vec2(MINIMAP_WIDTH, MINIMAP_HEIGHT),
    );
    let response = ui.interact(rect, ui.id().with("minimap"), Sense::click_and_drag());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, MINIMAP_BG);

    let reset_rect = Rect::from_min_size(
        pos2(
            rect.right() - 86.0,
            (rect.top() - 28.0).max(canvas_rect.top() + MINIMAP_PADDING),
        ),
        vec2(86.0, 22.0),
    );
    if ui
        .interact(
            reset_rect,
            ui.id().with("minimap-focus-all"),
            Sense::click(),
        )
        .clicked()
    {
        return MinimapResult {
            navigate_to: None,
            hide_clicked: false,
            focus_all_clicked: true,
        };
    }
    painter.rect_filled(
        reset_rect,
        11.0,
        Color32::from_rgba_premultiplied(18, 18, 22, 220),
    );
    painter.rect_stroke(
        reset_rect,
        11.0,
        Stroke::new(1.0, Color32::from_rgb(65, 65, 78)),
    );
    painter.text(
        reset_rect.center(),
        Align2::CENTER_CENTER,
        "Show All",
        FontId::proportional(11.5),
        Color32::from_rgb(205, 205, 214),
    );

    let mut bounds = viewport.visible_canvas_rect(canvas_rect);
    for panel in panels {
        bounds = bounds.union(panel.rect());
    }
    bounds = bounds.expand(100.0);

    let scale_x = rect.width() / bounds.width().max(1.0);
    let scale_y = rect.height() / bounds.height().max(1.0);
    let scale = scale_x.min(scale_y);
    let inset = pos2(
        rect.left() + (rect.width() - bounds.width() * scale) * 0.5,
        rect.top() + (rect.height() - bounds.height() * scale) * 0.5,
    );

    let to_minimap = |canvas_pos: Pos2| -> Pos2 {
        pos2(
            inset.x + (canvas_pos.x - bounds.left()) * scale,
            inset.y + (canvas_pos.y - bounds.top()) * scale,
        )
    };

    for panel in panels {
        let panel_rect = panel.rect();
        let mini_rect = Rect::from_min_max(to_minimap(panel_rect.min), to_minimap(panel_rect.max));
        let color = panel.color();
        painter.rect_filled(
            mini_rect,
            2.0,
            Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 180),
        );
    }

    let visible = viewport.visible_canvas_rect(canvas_rect);
    let visible_rect = Rect::from_min_max(to_minimap(visible.min), to_minimap(visible.max));
    painter.rect_stroke(visible_rect, 2.0, Stroke::new(1.0, MINIMAP_VIEWPORT_BORDER));

    let hide_rect = Rect::from_min_size(rect.right_top() - vec2(22.0, -4.0), vec2(18.0, 18.0));
    if ui
        .interact(hide_rect, ui.id().with("minimap-close"), Sense::click())
        .clicked()
    {
        return MinimapResult {
            navigate_to: None,
            hide_clicked: true,
            focus_all_clicked: false,
        };
    }
    painter.text(
        hide_rect.center(),
        Align2::CENTER_CENTER,
        "×",
        FontId::proportional(13.0),
        Color32::from_rgb(180, 180, 180),
    );
    painter.text(
        rect.center_bottom() - vec2(0.0, 6.0),
        Align2::CENTER_BOTTOM,
        format!("{:.0}%", viewport.zoom * 100.0),
        FontId::proportional(11.0),
        Color32::from_rgb(163, 163, 163),
    );

    if response.clicked() || response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let target = minimap_to_canvas(pos, rect, bounds);
            return MinimapResult {
                navigate_to: Some(target),
                hide_clicked: false,
                focus_all_clicked: false,
            };
        }
    }

    MinimapResult::default()
}
