//! Geometría y chrome de la ventana de terminal: rects de controles, radios,
//! LOD por tamaño/zoom, handles de resize y mapeo puntero→celda. Separado del
//! panel para que estas reglas puras se lean y testeen solas.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PanelLod {
    Full,
    Compact,
    Minimal,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PanelRoundings {
    pub(super) panel: Rounding,
    pub(super) title: Rounding,
    pub(super) body: Rounding,
}

impl ResizeHandle {
    pub(super) const ALL: [Self; 8] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
        Self::Left,
        Self::Right,
        Self::Top,
        Self::Bottom,
    ];

    pub(super) fn resizes_left(self) -> bool {
        matches!(self, Self::Left | Self::TopLeft | Self::BottomLeft)
    }

    pub(super) fn resizes_right(self) -> bool {
        matches!(self, Self::Right | Self::TopRight | Self::BottomRight)
    }

    pub(super) fn resizes_top(self) -> bool {
        matches!(self, Self::Top | Self::TopLeft | Self::TopRight)
    }

    pub(super) fn resizes_bottom(self) -> bool {
        matches!(self, Self::Bottom | Self::BottomLeft | Self::BottomRight)
    }

    #[allow(dead_code)]
    pub(super) fn hit_rect(self, screen_rect: Rect) -> Rect {
        match self {
            Self::TopLeft => Rect::from_min_max(
                screen_rect.min,
                screen_rect.min + vec2(RESIZE_CORNER_SIZE, RESIZE_CORNER_SIZE),
            ),
            Self::TopRight => Rect::from_min_max(
                pos2(screen_rect.right() - RESIZE_CORNER_SIZE, screen_rect.top()),
                pos2(screen_rect.right(), screen_rect.top() + RESIZE_CORNER_SIZE),
            ),
            Self::BottomLeft => Rect::from_min_max(
                pos2(
                    screen_rect.left(),
                    screen_rect.bottom() - RESIZE_CORNER_SIZE,
                ),
                pos2(
                    screen_rect.left() + RESIZE_CORNER_SIZE,
                    screen_rect.bottom(),
                ),
            ),
            Self::BottomRight => Rect::from_min_max(
                screen_rect.right_bottom() - vec2(RESIZE_CORNER_SIZE, RESIZE_CORNER_SIZE),
                screen_rect.right_bottom(),
            ),
            Self::Left => Rect::from_min_max(
                pos2(screen_rect.left(), screen_rect.top() + RESIZE_CORNER_SIZE),
                pos2(
                    screen_rect.left() + RESIZE_HIT_THICKNESS,
                    screen_rect.bottom() - RESIZE_CORNER_SIZE,
                ),
            ),
            Self::Right => Rect::from_min_max(
                pos2(
                    screen_rect.right() - RESIZE_HIT_THICKNESS,
                    screen_rect.top() + RESIZE_CORNER_SIZE,
                ),
                pos2(
                    screen_rect.right(),
                    screen_rect.bottom() - RESIZE_CORNER_SIZE,
                ),
            ),
            Self::Top => Rect::from_min_max(
                pos2(screen_rect.left() + RESIZE_CORNER_SIZE, screen_rect.top()),
                pos2(
                    screen_rect.right() - RESIZE_CORNER_SIZE,
                    screen_rect.top() + RESIZE_HIT_THICKNESS,
                ),
            ),
            Self::Bottom => Rect::from_min_max(
                pos2(
                    screen_rect.left() + RESIZE_CORNER_SIZE,
                    screen_rect.bottom() - RESIZE_HIT_THICKNESS,
                ),
                pos2(
                    screen_rect.right() - RESIZE_CORNER_SIZE,
                    screen_rect.bottom(),
                ),
            ),
        }
    }

    pub(super) fn apply_delta(self, rect: Rect, delta: Vec2) -> Rect {
        let mut min = rect.min;
        let mut max = rect.max;

        if self.resizes_left() {
            min.x = (min.x + delta.x).min(max.x - MIN_WIDTH);
        }
        if self.resizes_right() {
            max.x = (max.x + delta.x).max(min.x + MIN_WIDTH);
        }
        if self.resizes_top() {
            min.y = (min.y + delta.y).min(max.y - MIN_HEIGHT);
        }
        if self.resizes_bottom() {
            max.y = (max.y + delta.y).max(min.y + MIN_HEIGHT);
        }

        Rect::from_min_max(min, max)
    }

    pub(super) fn apply_snap_delta(self, rect: Rect, delta: Vec2) -> Rect {
        let mut min = rect.min;
        let mut max = rect.max;

        if self.resizes_left() {
            min.x += delta.x;
        } else if self.resizes_right() {
            max.x += delta.x;
        }

        if self.resizes_top() {
            min.y += delta.y;
        } else if self.resizes_bottom() {
            max.y += delta.y;
        }

        Rect::from_min_max(min, max)
    }
}

pub(super) fn close_rect(title_rect: Rect) -> Rect {
    let chrome_zoom = chrome_zoom_from_title_rect(title_rect);
    Rect::from_center_size(
        pos2(
            title_rect.left() + 26.0 * chrome_zoom,
            title_rect.center().y,
        ),
        vec2(18.0, 18.0) * chrome_zoom,
    )
}

pub(super) fn minimize_rect(title_rect: Rect) -> Rect {
    let chrome_zoom = chrome_zoom_from_title_rect(title_rect);
    Rect::from_center_size(
        pos2(
            title_rect.left() + 46.0 * chrome_zoom,
            title_rect.center().y,
        ),
        vec2(18.0, 18.0) * chrome_zoom,
    )
}

#[allow(dead_code)]
pub(super) fn resize_handle_rect(screen_rect: Rect) -> Rect {
    Rect::from_min_size(
        screen_rect.right_bottom() - vec2(RESIZE_GRIP_SIZE, RESIZE_GRIP_SIZE),
        vec2(RESIZE_GRIP_SIZE, RESIZE_GRIP_SIZE),
    )
}

pub(super) fn title_drag_hit_rect(screen_rect: Rect, title_rect: Rect) -> Rect {
    const MIN_TITLE_DRAG_HIT_HEIGHT: f32 = 18.0;
    const MIN_TITLE_DRAG_HIT_WIDTH: f32 = 28.0;

    let controls_inset = if should_draw_window_controls(screen_rect, title_rect) {
        (90.0 * chrome_zoom_from_title_rect(title_rect)).clamp(42.0, 90.0)
    } else {
        10.0
    };
    let right = screen_rect.right() - RESIZE_HIT_THICKNESS;
    let left = (screen_rect.left() + controls_inset)
        .min(right - MIN_TITLE_DRAG_HIT_WIDTH)
        .max(screen_rect.left() + RESIZE_HIT_THICKNESS);
    let bottom = (title_rect.top() + title_rect.height().max(MIN_TITLE_DRAG_HIT_HEIGHT))
        .min(screen_rect.bottom() - RESIZE_HIT_THICKNESS)
        .max(title_rect.top() + 1.0);

    Rect::from_min_max(pos2(left, title_rect.top()), pos2(right, bottom))
}

pub(super) fn resize_target_from_origin(
    handle: ResizeHandle,
    origin: Rect,
    drag_delta: Vec2,
    zoom: f32,
) -> Rect {
    handle.apply_delta(origin, drag_delta / zoom.max(0.01))
}

pub(super) fn chrome_zoom(zoom: f32) -> f32 {
    zoom.clamp(0.0, CHROME_ZOOM_MAX)
}

pub(super) fn title_bar_height(zoom: f32) -> f32 {
    TITLE_BAR_HEIGHT * chrome_zoom(zoom)
}

pub(super) fn chrome_zoom_from_title_rect(title_rect: Rect) -> f32 {
    (title_rect.height() / TITLE_BAR_HEIGHT).clamp(0.0, CHROME_ZOOM_MAX)
}

pub(super) fn panel_corner_radius(screen_rect: Rect) -> f32 {
    BORDER_RADIUS
        .min(screen_rect.width() * 0.18)
        .min(screen_rect.height() * 0.18)
        .max(0.0)
}

pub(super) fn panel_roundings(
    screen_rect: Rect,
    title_rect: Rect,
    body_rect: Rect,
) -> PanelRoundings {
    let base_radius = panel_corner_radius(screen_rect);
    let top_radius = base_radius
        .min(title_rect.width() * 0.5)
        .min(title_rect.height() * 0.5)
        .max(0.0);
    let bottom_radius = base_radius
        .min(body_rect.width() * 0.5)
        .min(body_rect.height() * 0.5)
        .max(0.0);

    PanelRoundings {
        panel: Rounding {
            nw: top_radius,
            ne: top_radius,
            sw: bottom_radius,
            se: bottom_radius,
        },
        title: Rounding {
            nw: top_radius,
            ne: top_radius,
            sw: 0.0,
            se: 0.0,
        },
        body: Rounding {
            nw: 0.0,
            ne: 0.0,
            sw: bottom_radius,
            se: bottom_radius,
        },
    }
}

pub(super) fn max_panel_corner_radius(roundings: PanelRoundings) -> f32 {
    roundings
        .panel
        .nw
        .max(roundings.panel.ne)
        .max(roundings.panel.sw)
        .max(roundings.panel.se)
}

pub(super) fn panel_lod(screen_rect: Rect, title_rect: Rect) -> PanelLod {
    if screen_rect.width() < 96.0 || screen_rect.height() < 64.0 || title_rect.height() < 8.0 {
        PanelLod::Minimal
    } else if screen_rect.width() < 220.0
        || screen_rect.height() < 120.0
        || title_rect.height() < 14.0
    {
        PanelLod::Compact
    } else {
        PanelLod::Full
    }
}

pub(super) fn should_draw_window_controls(screen_rect: Rect, title_rect: Rect) -> bool {
    screen_rect.width() >= MIN_CONTROL_STRIP_WIDTH && title_rect.height() >= 8.0
}

pub(super) fn should_draw_title_text(screen_rect: Rect, title_rect: Rect) -> bool {
    screen_rect.width() >= MIN_TITLE_TEXT_WIDTH && title_rect.height() >= 10.0
}

#[allow(dead_code)]
pub(super) fn should_draw_resize_grip(screen_rect: Rect) -> bool {
    screen_rect.width() >= MIN_RESIZE_GRIP_WIDTH && screen_rect.height() >= MIN_RESIZE_GRIP_HEIGHT
}

pub(super) fn should_render_terminal_contents(content_rect: Rect, zoom: f32) -> bool {
    zoom >= MIN_TERMINAL_RENDER_ZOOM
        && content_rect.width() >= MIN_TERMINAL_RENDER_WIDTH
        && content_rect.height() >= MIN_TERMINAL_RENDER_HEIGHT
}

pub(super) fn rect_to_saved_bounds(rect: Rect) -> SavedPanelBounds {
    SavedPanelBounds::new([rect.min.x, rect.min.y], [rect.width(), rect.height()])
}

pub(super) fn saved_bounds_to_rect(bounds: SavedPanelBounds) -> Rect {
    Rect::from_min_size(
        pos2(bounds.position[0], bounds.position[1]),
        vec2(bounds.size[0], bounds.size[1]),
    )
}

pub fn snap_slot_rect(slot: SnapSlot, desktop_rect: Rect) -> Rect {
    let half_width = desktop_rect.width() * 0.5;
    let half_height = desktop_rect.height() * 0.5;

    match slot {
        SnapSlot::LeftHalf => {
            Rect::from_min_size(desktop_rect.min, vec2(half_width, desktop_rect.height()))
        }
        SnapSlot::RightHalf => Rect::from_min_size(
            pos2(desktop_rect.center().x, desktop_rect.top()),
            vec2(half_width, desktop_rect.height()),
        ),
        SnapSlot::TopHalf => {
            Rect::from_min_size(desktop_rect.min, vec2(desktop_rect.width(), half_height))
        }
        SnapSlot::BottomHalf => Rect::from_min_size(
            pos2(desktop_rect.left(), desktop_rect.center().y),
            vec2(desktop_rect.width(), half_height),
        ),
        SnapSlot::TopLeft => Rect::from_min_size(desktop_rect.min, vec2(half_width, half_height)),
        SnapSlot::TopRight => Rect::from_min_size(
            pos2(desktop_rect.center().x, desktop_rect.top()),
            vec2(half_width, half_height),
        ),
        SnapSlot::BottomLeft => Rect::from_min_size(
            pos2(desktop_rect.left(), desktop_rect.center().y),
            vec2(half_width, half_height),
        ),
        SnapSlot::BottomRight => {
            Rect::from_min_size(desktop_rect.center(), vec2(half_width, half_height))
        }
        SnapSlot::Maximized => desktop_rect,
    }
}

pub fn normalize_snapped_rect(slot: SnapSlot, rect: Rect, desktop_rect: Rect) -> Rect {
    let min_width = MIN_WIDTH.min(desktop_rect.width());
    let min_height = MIN_HEIGHT.min(desktop_rect.height());

    match slot {
        SnapSlot::LeftHalf => {
            let width = rect.width().clamp(min_width, desktop_rect.width());
            Rect::from_min_max(
                desktop_rect.min,
                pos2(
                    (desktop_rect.left() + width).min(desktop_rect.right()),
                    desktop_rect.bottom(),
                ),
            )
        }
        SnapSlot::RightHalf => {
            let width = rect.width().clamp(min_width, desktop_rect.width());
            Rect::from_min_max(
                pos2(
                    (desktop_rect.right() - width).max(desktop_rect.left()),
                    desktop_rect.top(),
                ),
                desktop_rect.max,
            )
        }
        SnapSlot::TopHalf => {
            let height = rect.height().clamp(min_height, desktop_rect.height());
            Rect::from_min_max(
                desktop_rect.min,
                pos2(
                    desktop_rect.right(),
                    (desktop_rect.top() + height).min(desktop_rect.bottom()),
                ),
            )
        }
        SnapSlot::BottomHalf => {
            let height = rect.height().clamp(min_height, desktop_rect.height());
            Rect::from_min_max(
                pos2(
                    desktop_rect.left(),
                    (desktop_rect.bottom() - height).max(desktop_rect.top()),
                ),
                desktop_rect.max,
            )
        }
        SnapSlot::TopLeft => {
            let width = rect.width().clamp(min_width, desktop_rect.width());
            let height = rect.height().clamp(min_height, desktop_rect.height());
            Rect::from_min_max(
                desktop_rect.min,
                pos2(
                    (desktop_rect.left() + width).min(desktop_rect.right()),
                    (desktop_rect.top() + height).min(desktop_rect.bottom()),
                ),
            )
        }
        SnapSlot::TopRight => {
            let width = rect.width().clamp(min_width, desktop_rect.width());
            let height = rect.height().clamp(min_height, desktop_rect.height());
            Rect::from_min_max(
                pos2(
                    (desktop_rect.right() - width).max(desktop_rect.left()),
                    desktop_rect.top(),
                ),
                pos2(
                    desktop_rect.right(),
                    (desktop_rect.top() + height).min(desktop_rect.bottom()),
                ),
            )
        }
        SnapSlot::BottomLeft => {
            let width = rect.width().clamp(min_width, desktop_rect.width());
            let height = rect.height().clamp(min_height, desktop_rect.height());
            Rect::from_min_max(
                pos2(
                    desktop_rect.left(),
                    (desktop_rect.bottom() - height).max(desktop_rect.top()),
                ),
                pos2(
                    (desktop_rect.left() + width).min(desktop_rect.right()),
                    desktop_rect.bottom(),
                ),
            )
        }
        SnapSlot::BottomRight => {
            let width = rect.width().clamp(min_width, desktop_rect.width());
            let height = rect.height().clamp(min_height, desktop_rect.height());
            Rect::from_min_max(
                pos2(
                    (desktop_rect.right() - width).max(desktop_rect.left()),
                    (desktop_rect.bottom() - height).max(desktop_rect.top()),
                ),
                desktop_rect.max,
            )
        }
        SnapSlot::Maximized => desktop_rect,
    }
}

#[cfg(test)]
pub(super) fn should_render_live_terminal(
    content_rect: Rect,
    zoom: f32,
    lod: PanelLod,
    fast_path_render: bool,
) -> bool {
    matches!(
        render_tier_for_panel(content_rect, zoom, lod, fast_path_render, false, false),
        RenderTier::Full | RenderTier::ReducedLive
    )
}

pub(super) fn should_defer_terminal_resize(
    fast_path_render: bool,
    resize_virtual_rect: Option<Rect>,
) -> bool {
    fast_path_render && resize_virtual_rect.is_some()
}

pub(super) fn body_behaves_like_title_bar(lod: PanelLod) -> bool {
    !matches!(lod, PanelLod::Full)
}

pub(super) fn terminal_mouse_cell_from_pointer(
    content_rect: Rect,
    pointer: Pos2,
    zoom: f32,
) -> Option<(usize, usize)> {
    let metrics = grid_metrics(zoom);
    let rect = Rect::from_min_max(
        Pos2::new(
            content_rect.left() + PAD_X * zoom.max(0.01),
            content_rect.top() + PAD_Y * zoom.max(0.01),
        ),
        content_rect.right_bottom(),
    );
    let point = grid_point_from_position(rect, pointer, &metrics, u16::MAX, u16::MAX)?;
    Some((point.column, point.line))
}

pub(super) fn should_refresh_activity_label(last_scan_at: f64, time: f64) -> bool {
    (time - last_scan_at) >= 0.45
}

pub(super) fn egui_modifiers(modifiers: SerializableModifiers) -> egui::Modifiers {
    egui::Modifiers {
        alt: modifiers.alt,
        ctrl: modifiers.ctrl,
        shift: modifiers.shift,
        mac_cmd: modifiers.command,
        command: modifiers.command,
    }
}

pub(super) fn render_tier_for_panel(
    content_rect: Rect,
    zoom: f32,
    lod: PanelLod,
    fast_path_render: bool,
    focused: bool,
    streaming: bool,
) -> RenderTier {
    let previewable = content_rect.width() >= 24.0 && content_rect.height() >= 18.0;
    if !previewable {
        return RenderTier::Hidden;
    }
    if matches!(lod, PanelLod::Minimal) || !should_render_terminal_contents(content_rect, zoom) {
        return RenderTier::Preview;
    }
    if focused {
        return RenderTier::Full;
    }
    if fast_path_render || streaming {
        return RenderTier::ReducedLive;
    }
    RenderTier::Full
}

pub(super) fn is_streaming_output(pty: &PtyHandle) -> bool {
    pty.output_elapsed() <= STREAMING_OUTPUT_WINDOW
}

pub(super) fn body_input_rect(body_rect: Rect) -> Rect {
    body_rect.shrink2(vec2(RESIZE_HIT_THICKNESS, RESIZE_HIT_THICKNESS))
}
