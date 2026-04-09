use alacritty_terminal::index::Side;
use egui::{Pos2, Rect};

use super::input::GridPoint;
use crate::terminal::renderer::{CELL_HEIGHT_FACTOR, CELL_WIDTH_FACTOR, FONT_SIZE, PAD_X, PAD_Y};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridMetrics {
    pub char_width: f32,
    pub line_height: f32,
}

pub fn grid_metrics(zoom: f32) -> GridMetrics {
    let zoom = zoom.max(0.01);
    let font_size = FONT_SIZE * zoom;
    GridMetrics {
        char_width: font_size * CELL_WIDTH_FACTOR,
        line_height: font_size * CELL_HEIGHT_FACTOR,
    }
}

pub fn grid_padding(zoom: f32) -> (f32, f32) {
    let zoom = zoom.max(0.01);
    (PAD_X * zoom, PAD_Y * zoom)
}

pub fn grid_point_from_position(
    rect: Rect,
    position: Pos2,
    metrics: &GridMetrics,
    visible_rows: u16,
    visible_cols: u16,
) -> Option<GridPoint> {
    if !rect.contains(position) {
        return None;
    }

    let relative = position - rect.min;
    let row = (relative.y / metrics.line_height).floor().max(0.0);
    let column = (relative.x / metrics.char_width).floor().max(0.0);

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(GridPoint {
        line: (row as usize).min(usize::from(visible_rows.saturating_sub(1))),
        column: (column as usize).min(usize::from(visible_cols.saturating_sub(1))),
    })
}

pub fn terminal_cell_from_pointer(
    content_rect: Rect,
    pointer: Pos2,
    zoom: f32,
    visible_rows: u16,
    visible_cols: u16,
) -> Option<GridPoint> {
    if !content_rect.contains(pointer) {
        return None;
    }

    let metrics = grid_metrics(zoom);
    let (pad_x, pad_y) = grid_padding(zoom);
    let rect = Rect::from_min_max(
        Pos2::new(content_rect.left() + pad_x, content_rect.top() + pad_y),
        content_rect.right_bottom(),
    );

    grid_point_from_position(rect, pointer, &metrics, visible_rows, visible_cols)
}

pub fn cell_side_from_position(
    content_rect: Rect,
    pointer: Pos2,
    zoom: f32,
    point: GridPoint,
) -> Side {
    let metrics = grid_metrics(zoom);
    let (pad_x, _) = grid_padding(zoom);
    let local_x = (pointer.x - content_rect.left() - pad_x).max(0.0);
    let cell_left = point.column as f32 * metrics.char_width;
    if local_x - cell_left >= metrics.char_width * 0.5 {
        Side::Right
    } else {
        Side::Left
    }
}
