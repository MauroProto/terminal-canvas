use egui::{Pos2, Rect};

use crate::canvas::config::{GRID_COLOR, GRID_SPACING};
use crate::canvas::viewport::Viewport;

pub fn draw_grid(painter: &egui::Painter, viewport: &Viewport, screen_rect: Rect) {
    let visible = viewport.visible_canvas_rect(screen_rect);

    let start_x = (visible.min.x / GRID_SPACING).floor() as i32;
    let end_x = (visible.max.x / GRID_SPACING).ceil() as i32;
    let start_y = (visible.min.y / GRID_SPACING).floor() as i32;
    let end_y = (visible.max.y / GRID_SPACING).ceil() as i32;
    let stride = sampling_stride(start_x, end_x, start_y, end_y, viewport.zoom);

    let dot_radius = (0.8 * viewport.zoom).clamp(0.3, 2.0);

    for gx in (align_to_stride(start_x, stride)..=end_x).step_by(stride as usize) {
        for gy in (align_to_stride(start_y, stride)..=end_y).step_by(stride as usize) {
            let canvas_pos = Pos2::new(gx as f32 * GRID_SPACING, gy as f32 * GRID_SPACING);
            let screen_pos = viewport.canvas_to_screen(canvas_pos, screen_rect);
            painter.circle_filled(screen_pos, dot_radius, GRID_COLOR);
        }
    }
}

fn sampling_stride(start_x: i32, end_x: i32, start_y: i32, end_y: i32, zoom: f32) -> i32 {
    const TARGET_DOT_COUNT: f32 = 6_000.0;

    let columns = (end_x - start_x + 1).max(1) as f32;
    let rows = (end_y - start_y + 1).max(1) as f32;
    let visible_points = columns * rows;
    let adaptive_stride = (visible_points / TARGET_DOT_COUNT).sqrt().ceil() as i32;
    let zoom_stride = if zoom < 0.18 {
        8
    } else if zoom < 0.3 {
        4
    } else if zoom < 0.55 {
        2
    } else {
        1
    };

    adaptive_stride.max(zoom_stride).max(1)
}

fn align_to_stride(value: i32, stride: i32) -> i32 {
    value.div_euclid(stride) * stride
}

#[cfg(test)]
mod tests {
    use super::sampling_stride;

    #[test]
    fn sampling_stride_grows_when_zooming_out() {
        let at_normal_zoom = sampling_stride(-10, 10, -10, 10, 1.0);
        let zoomed_out = sampling_stride(-10, 10, -10, 10, 0.15);

        assert_eq!(at_normal_zoom, 1);
        assert!(zoomed_out > at_normal_zoom);
    }

    #[test]
    fn sampling_stride_grows_for_large_visible_areas() {
        let compact = sampling_stride(-20, 20, -20, 20, 1.0);
        let huge = sampling_stride(-400, 400, -300, 300, 1.0);

        assert_eq!(compact, 1);
        assert!(huge > compact);
    }
}
