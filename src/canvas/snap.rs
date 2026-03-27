use egui::{Pos2, Rect, Vec2};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapGuide {
    pub vertical: bool,
    pub position: f32,
    pub start: f32,
    pub end: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SnapResult {
    pub delta: Vec2,
    pub guides: Vec<SnapGuide>,
}

pub fn snap_drag(moving_rect: Rect, other_panels: &[Rect], threshold: f32) -> SnapResult {
    let mut best_dx: Option<(f32, SnapGuide)> = None;
    let mut best_dy: Option<(f32, SnapGuide)> = None;

    for other in other_panels {
        for &(my_x, other_x) in &[
            (moving_rect.left(), other.left()),
            (moving_rect.left(), other.center().x),
            (moving_rect.left(), other.right()),
            (moving_rect.center().x, other.left()),
            (moving_rect.center().x, other.center().x),
            (moving_rect.center().x, other.right()),
            (moving_rect.right(), other.left()),
            (moving_rect.right(), other.center().x),
            (moving_rect.right(), other.right()),
        ] {
            let dx = other_x - my_x;
            if dx.abs() < threshold
                && best_dx
                    .as_ref()
                    .map(|candidate| dx.abs() < candidate.0.abs())
                    .unwrap_or(true)
            {
                best_dx = Some((
                    dx,
                    SnapGuide {
                        vertical: true,
                        position: other_x,
                        start: moving_rect.top().min(other.top()),
                        end: moving_rect.bottom().max(other.bottom()),
                    },
                ));
            }
        }

        for &(my_y, other_y) in &[
            (moving_rect.top(), other.top()),
            (moving_rect.top(), other.center().y),
            (moving_rect.top(), other.bottom()),
            (moving_rect.center().y, other.top()),
            (moving_rect.center().y, other.center().y),
            (moving_rect.center().y, other.bottom()),
            (moving_rect.bottom(), other.top()),
            (moving_rect.bottom(), other.center().y),
            (moving_rect.bottom(), other.bottom()),
        ] {
            let dy = other_y - my_y;
            if dy.abs() < threshold
                && best_dy
                    .as_ref()
                    .map(|candidate| dy.abs() < candidate.0.abs())
                    .unwrap_or(true)
            {
                best_dy = Some((
                    dy,
                    SnapGuide {
                        vertical: false,
                        position: other_y,
                        start: moving_rect.left().min(other.left()),
                        end: moving_rect.right().max(other.right()),
                    },
                ));
            }
        }
    }

    let mut delta = Vec2::ZERO;
    let mut guides = Vec::new();

    if let Some((dx, guide)) = best_dx {
        delta.x = dx;
        guides.push(guide);
    }
    if let Some((dy, guide)) = best_dy {
        delta.y = dy;
        guides.push(guide);
    }

    SnapResult { delta, guides }
}

pub fn snap_resize(
    moving_rect: Rect,
    other_panels: &[Rect],
    threshold: f32,
    resize_left: bool,
    resize_bottom: bool,
) -> SnapResult {
    let mut result = SnapResult::default();

    let my_x = if resize_left {
        moving_rect.left()
    } else {
        moving_rect.right()
    };
    let my_y = if resize_bottom {
        moving_rect.bottom()
    } else {
        moving_rect.top()
    };

    for other in other_panels {
        for &other_x in &[other.left(), other.center().x, other.right()] {
            let dx = other_x - my_x;
            if dx.abs() < threshold && (result.delta.x == 0.0 || dx.abs() < result.delta.x.abs()) {
                result.delta.x = dx;
                result.guides.retain(|guide| !guide.vertical);
                result.guides.push(SnapGuide {
                    vertical: true,
                    position: other_x,
                    start: moving_rect.top().min(other.top()),
                    end: moving_rect.bottom().max(other.bottom()),
                });
            }
        }
        for &other_y in &[other.top(), other.center().y, other.bottom()] {
            let dy = other_y - my_y;
            if dy.abs() < threshold && (result.delta.y == 0.0 || dy.abs() < result.delta.y.abs()) {
                result.delta.y = dy;
                result.guides.retain(|guide| guide.vertical);
                result.guides.push(SnapGuide {
                    vertical: false,
                    position: other_y,
                    start: moving_rect.left().min(other.left()),
                    end: moving_rect.right().max(other.right()),
                });
            }
        }
    }

    result
}

pub fn guide_endpoints(guide: SnapGuide) -> [Pos2; 2] {
    if guide.vertical {
        [
            Pos2::new(guide.position, guide.start),
            Pos2::new(guide.position, guide.end),
        ]
    } else {
        [
            Pos2::new(guide.start, guide.position),
            Pos2::new(guide.end, guide.position),
        ]
    }
}

#[cfg(test)]
mod tests {
    use egui::{pos2, vec2, Rect};

    use super::snap_drag;

    #[test]
    fn picks_the_closest_snap_candidate() {
        let moving = Rect::from_min_size(pos2(98.0, 102.0), vec2(100.0, 100.0));
        let other_panels = [
            Rect::from_min_size(pos2(200.0, 100.0), vec2(100.0, 100.0)),
            Rect::from_min_size(pos2(100.0, 205.0), vec2(100.0, 100.0)),
        ];

        let result = snap_drag(moving, &other_panels, 8.0);

        assert_eq!(result.delta.x, 2.0);
        assert_eq!(result.delta.y, -2.0);
        assert_eq!(result.guides.len(), 2);
    }
}
