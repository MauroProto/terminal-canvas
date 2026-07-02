use egui::{pos2, vec2, Pos2, Rect};

pub const SCROLLBAR_WIDTH: f32 = 12.0;
pub const SCROLLBAR_GAP: f32 = 8.0;
const SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 18.0;

pub fn terminal_body_rect(body_rect: Rect) -> Rect {
    let reserved = SCROLLBAR_WIDTH + SCROLLBAR_GAP;
    let width = (body_rect.width() - reserved).max(0.0);
    Rect::from_min_size(body_rect.min, vec2(width, body_rect.height()))
}

pub fn terminal_scrollbar_rect(body_rect: Rect) -> Rect {
    Rect::from_min_size(
        pos2(body_rect.right() - SCROLLBAR_WIDTH, body_rect.top() + 4.0),
        vec2(SCROLLBAR_WIDTH, (body_rect.height() - 8.0).max(24.0)),
    )
}

pub fn scrollbar_thumb_height(
    track_height: f32,
    visible_rows: usize,
    scrollback_limit: usize,
) -> f32 {
    if track_height <= SCROLLBAR_MIN_THUMB_HEIGHT {
        return track_height.max(0.0);
    }

    let visible_rows = visible_rows.max(1) as f32;
    let total_rows = (visible_rows + scrollback_limit.max(1) as f32).max(visible_rows);
    (track_height * (visible_rows / total_rows)).clamp(SCROLLBAR_MIN_THUMB_HEIGHT, track_height)
}

pub fn scrollbar_pointer_to_scrollback(
    pointer_position: Pos2,
    track_rect: Rect,
    thumb_height: f32,
    scrollback_limit: usize,
) -> usize {
    let clamped_y = pointer_position.y.clamp(track_rect.min.y, track_rect.max.y);
    let travel = (track_rect.height() - thumb_height).max(1.0);
    let relative = (track_rect.max.y - thumb_height - clamped_y).clamp(0.0, travel);
    let ratio = (relative / travel).clamp(0.0, 1.0);
    (ratio * scrollback_limit.max(1) as f32).round() as usize
}
