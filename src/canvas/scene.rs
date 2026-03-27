use egui::{PointerButton, Rect, Ui};

use crate::canvas::viewport::Viewport;

#[derive(Debug, Clone, Copy, Default)]
pub struct CanvasInteractionState {
    pub navigating: bool,
    pub viewport_changed: bool,
}

pub fn handle_canvas_input(
    ui: &mut Ui,
    viewport: &mut Viewport,
    canvas_rect: Rect,
    hovered_panel: bool,
    scroll_captured_by_panel: bool,
) -> CanvasInteractionState {
    let (hover_pos, pointer_delta, middle_down, primary_down, scroll, zoom_delta, modifiers) =
        ui.ctx().input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.delta(),
                i.pointer.button_down(PointerButton::Middle),
                i.pointer.button_down(PointerButton::Primary),
                i.smooth_scroll_delta,
                i.zoom_delta(),
                i.modifiers,
            )
        });

    let pointer_in_canvas = hover_pos.is_some_and(|pos| canvas_rect.contains(pos));
    let mut state = CanvasInteractionState::default();

    if pointer_in_canvas
        && (middle_down || (primary_down && !hovered_panel))
        && pointer_delta != egui::Vec2::ZERO
    {
        viewport.pan += pointer_delta;
        state.navigating = true;
        state.viewport_changed = true;
    }

    let scroll_zoom = if !scroll_captured_by_panel
        && pointer_in_canvas
        && (modifiers.ctrl || modifiers.command)
        && scroll.y.abs() > f32::EPSILON
    {
        Some((1.0 + scroll.y * 0.0015).clamp(0.88, 1.12))
    } else {
        None
    };
    let pinch_zoom = if !scroll_captured_by_panel
        && pointer_in_canvas
        && (zoom_delta - 1.0).abs() > f32::EPSILON
    {
        Some(zoom_delta.clamp(0.88, 1.12))
    } else {
        None
    };

    if let Some(factor) = pinch_zoom.or(scroll_zoom) {
        if let Some(pos) = hover_pos {
            viewport.zoom_around(pos, canvas_rect, factor);
            state.navigating = true;
            state.viewport_changed = true;
        }
    } else if !scroll_captured_by_panel && !hovered_panel && scroll != egui::Vec2::ZERO {
        viewport.pan += scroll;
        state.navigating = true;
        state.viewport_changed = true;
    }

    state
}

#[cfg(test)]
mod tests {
    use egui::{pos2, vec2, CentralPanel, Event, MouseWheelUnit, RawInput};

    use super::handle_canvas_input;
    use crate::canvas::viewport::Viewport;

    #[test]
    fn captured_scroll_does_not_pan_canvas() {
        let ctx = egui::Context::default();
        let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let mut viewport = Viewport::default();

        let _ = ctx.run(
            RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    Event::PointerMoved(pos2(200.0, 200.0)),
                    Event::MouseWheel {
                        unit: MouseWheelUnit::Point,
                        delta: vec2(0.0, 120.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ctx| {
                CentralPanel::default().show(ctx, |ui| {
                    let state = handle_canvas_input(ui, &mut viewport, screen_rect, false, true);
                    assert!(!state.viewport_changed);
                });
            },
        );

        assert_eq!(viewport.pan, egui::Vec2::ZERO);
    }

    #[test]
    fn uncaptured_scroll_pans_canvas_when_not_over_panel() {
        let ctx = egui::Context::default();
        let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let mut viewport = Viewport::default();

        let _ = ctx.run(
            RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    Event::PointerMoved(pos2(200.0, 200.0)),
                    Event::MouseWheel {
                        unit: MouseWheelUnit::Point,
                        delta: vec2(0.0, 120.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ctx| {
                CentralPanel::default().show(ctx, |ui| {
                    let state = handle_canvas_input(ui, &mut viewport, screen_rect, false, false);
                    assert!(state.viewport_changed);
                });
            },
        );

        assert_eq!(viewport.pan.x, 0.0);
        assert!(viewport.pan.y > 0.0);
    }

    #[test]
    fn captured_pinch_does_not_zoom_canvas() {
        let ctx = egui::Context::default();
        let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let mut viewport = Viewport::default();

        let _ = ctx.run(
            RawInput {
                screen_rect: Some(screen_rect),
                events: vec![Event::PointerMoved(pos2(200.0, 200.0)), Event::Zoom(1.2)],
                ..Default::default()
            },
            |ctx| {
                CentralPanel::default().show(ctx, |ui| {
                    let state = handle_canvas_input(ui, &mut viewport, screen_rect, false, true);
                    assert!(!state.viewport_changed);
                });
            },
        );

        assert_eq!(viewport.zoom, 1.0);
    }
}
