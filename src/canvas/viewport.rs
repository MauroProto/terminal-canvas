use egui::{emath::TSTransform, pos2, vec2, Pos2, Rect, Vec2};

use crate::canvas::config::{ZOOM_MAX, ZOOM_MIN};

#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub pan: Vec2,
    pub zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            pan: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

impl Viewport {
    pub fn transform(&self, canvas_rect: Rect) -> TSTransform {
        TSTransform::new(canvas_rect.min.to_vec2() + self.pan, self.zoom)
    }

    pub fn screen_to_canvas(&self, screen_pos: Pos2, screen_rect: Rect) -> Pos2 {
        let rel = screen_pos - screen_rect.min;
        pos2(
            (rel.x - self.pan.x) / self.zoom,
            (rel.y - self.pan.y) / self.zoom,
        )
    }

    pub fn canvas_to_screen(&self, canvas_pos: Pos2, screen_rect: Rect) -> Pos2 {
        pos2(
            canvas_pos.x * self.zoom + self.pan.x + screen_rect.min.x,
            canvas_pos.y * self.zoom + self.pan.y + screen_rect.min.y,
        )
    }

    pub fn zoom_around(&mut self, screen_pos: Pos2, screen_rect: Rect, factor: f32) {
        let canvas_pos = self.screen_to_canvas(screen_pos, screen_rect);
        self.zoom = (self.zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        self.pan = vec2(
            screen_pos.x - screen_rect.min.x - canvas_pos.x * self.zoom,
            screen_pos.y - screen_rect.min.y - canvas_pos.y * self.zoom,
        );
    }

    pub fn pan_to_center(&mut self, canvas_pos: Pos2, screen_rect: Rect) {
        let center = screen_rect.center();
        self.pan = vec2(
            center.x - screen_rect.min.x - canvas_pos.x * self.zoom,
            center.y - screen_rect.min.y - canvas_pos.y * self.zoom,
        );
    }

    pub fn focus_on_rect(
        &self,
        panel_rect: Rect,
        screen_rect: Rect,
        padding: f32,
        max_focus_zoom: f32,
    ) -> Self {
        let available_width = (screen_rect.width() - padding * 2.0).max(1.0);
        let available_height = (screen_rect.height() - padding * 2.0).max(1.0);
        let fit_zoom = (available_width / panel_rect.width().max(1.0))
            .min(available_height / panel_rect.height().max(1.0));
        let clamped_fit_zoom = fit_zoom.clamp(ZOOM_MIN, ZOOM_MAX.min(max_focus_zoom));
        let mut focused = Self {
            pan: self.pan,
            zoom: self.zoom.max(clamped_fit_zoom).clamp(ZOOM_MIN, ZOOM_MAX),
        };
        focused.pan_to_center(panel_rect.center(), screen_rect);
        focused
    }

    pub fn fit_rect(panel_rect: Rect, screen_rect: Rect, padding: f32, max_zoom: f32) -> Self {
        let available_width = (screen_rect.width() - padding * 2.0).max(1.0);
        let available_height = (screen_rect.height() - padding * 2.0).max(1.0);
        let fit_zoom = (available_width / panel_rect.width().max(1.0))
            .min(available_height / panel_rect.height().max(1.0));
        let zoom = fit_zoom.clamp(ZOOM_MIN, ZOOM_MAX.min(max_zoom));
        let mut fitted = Self {
            pan: Vec2::ZERO,
            zoom,
        };
        fitted.pan_to_center(panel_rect.center(), screen_rect);
        fitted
    }

    pub fn visible_canvas_rect(&self, screen_rect: Rect) -> Rect {
        let min = self.screen_to_canvas(screen_rect.min, screen_rect);
        let max = self.screen_to_canvas(screen_rect.max, screen_rect);
        Rect::from_min_max(min, max)
    }

    pub fn is_visible(&self, panel_rect: Rect, screen_rect: Rect) -> bool {
        self.visible_canvas_rect(screen_rect).intersects(panel_rect)
    }
}

#[cfg(test)]
mod tests {
    use egui::{pos2, vec2, Rect};

    use super::Viewport;

    #[test]
    fn pan_to_center_places_canvas_point_at_screen_center() {
        let mut viewport = Viewport {
            pan: vec2(0.0, 0.0),
            zoom: 1.5,
        };
        let screen = Rect::from_min_max(pos2(50.0, 40.0), pos2(850.0, 640.0));
        let point = pos2(120.0, 60.0);

        viewport.pan_to_center(point, screen);

        let mapped = viewport.canvas_to_screen(point, screen);
        assert!((mapped.x - screen.center().x).abs() < 0.001);
        assert!((mapped.y - screen.center().y).abs() < 0.001);
    }

    #[test]
    fn zoom_around_keeps_anchor_position_stable() {
        let mut viewport = Viewport::default();
        let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 700.0));
        let anchor = pos2(300.0, 200.0);
        let before = viewport.screen_to_canvas(anchor, screen);

        viewport.zoom_around(anchor, screen, 1.5);

        let after = viewport.canvas_to_screen(before, screen);
        assert!((after.x - anchor.x).abs() < 0.001);
        assert!((after.y - anchor.y).abs() < 0.001);
    }

    #[test]
    fn focus_on_rect_centers_panel_and_zooms_in_when_far() {
        let viewport = Viewport::default();
        let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(1200.0, 800.0));
        let panel = Rect::from_min_max(pos2(200.0, 120.0), pos2(620.0, 420.0));

        let focused = viewport.focus_on_rect(panel, screen, 72.0, 2.0);
        let center = focused.canvas_to_screen(panel.center(), screen);

        assert!((center.x - screen.center().x).abs() < 0.001);
        assert!((center.y - screen.center().y).abs() < 0.001);
        assert!(focused.zoom > viewport.zoom);
    }

    #[test]
    fn focus_on_rect_never_zooms_out_existing_close_view() {
        let viewport = Viewport {
            pan: vec2(-340.0, -210.0),
            zoom: 2.4,
        };
        let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(1200.0, 800.0));
        let panel = Rect::from_min_max(pos2(200.0, 120.0), pos2(620.0, 420.0));

        let focused = viewport.focus_on_rect(panel, screen, 72.0, 2.0);

        assert_eq!(focused.zoom, viewport.zoom);
    }

    #[test]
    fn fit_rect_zooms_out_to_show_large_bounds() {
        let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(1200.0, 800.0));
        let panel = Rect::from_min_max(pos2(-200.0, -120.0), pos2(1800.0, 1280.0));

        let fitted = Viewport::fit_rect(panel, screen, 72.0, 1.0);
        let visible = fitted.visible_canvas_rect(screen);

        assert!(visible.contains_rect(panel));
        assert!(fitted.zoom < 1.0);
    }
}
