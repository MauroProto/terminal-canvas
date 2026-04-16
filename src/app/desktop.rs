use std::path::PathBuf;

use egui::{Pos2, Rect};

use crate::canvas::viewport::Viewport;
use crate::panel::CanvasPanel;
use crate::state::{PanelPlacement, Workspace};
use crate::terminal::panel::PanelHitArea;

#[derive(Clone, Copy)]
pub(super) struct PanelHit {
    pub(super) index: usize,
    pub(super) area: PanelHitArea,
    pub(super) pointer: Pos2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SplitResizeAxis {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy)]
pub(super) struct SplitResizeHit {
    pub(super) leading_index: usize,
    pub(super) trailing_index: usize,
    pub(super) axis: SplitResizeAxis,
    pub(super) boundary: f32,
    pub(super) hit_rect: Rect,
}

pub(super) fn panel_scroll_capture_active(
    hovered_panel: bool,
    smooth_scroll_delta: egui::Vec2,
    zoom_delta: f32,
    modifiers: egui::Modifiers,
) -> bool {
    hovered_panel
        && smooth_scroll_delta != egui::Vec2::ZERO
        && !panel_zoom_gesture_active(smooth_scroll_delta, zoom_delta, modifiers)
}

pub(super) fn panel_zoom_gesture_active(
    smooth_scroll_delta: egui::Vec2,
    zoom_delta: f32,
    modifiers: egui::Modifiers,
) -> bool {
    (zoom_delta - 1.0).abs() > f32::EPSILON
        || ((modifiers.ctrl || modifiers.command) && smooth_scroll_delta != egui::Vec2::ZERO)
}

pub(super) fn upsert_workspace_for_folder(workspaces: &mut Vec<Workspace>, path: PathBuf) -> usize {
    if let Some(index) = workspaces
        .iter()
        .position(|workspace| workspace.matches_cwd(&path))
    {
        return index;
    }

    workspaces.push(Workspace::from_folder(path));
    workspaces.len() - 1
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn overview_viewport_for_panels(
    panels: &[CanvasPanel],
    screen_rect: Rect,
    padding: f32,
    max_zoom: f32,
) -> Viewport {
    let Some(bounds) = panels
        .iter()
        .filter(|panel| !panel.minimized())
        .map(|panel| panel.rect())
        .reduce(|a, b| a.union(b))
    else {
        return Viewport::default();
    };

    Viewport::fit_rect(bounds.expand(48.0), screen_rect, padding, max_zoom)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn interpolate_viewport(start: Viewport, target: Viewport, progress: f32) -> Viewport {
    let progress = progress.clamp(0.0, 1.0);
    Viewport {
        pan: start.pan + (target.pan - start.pan) * progress,
        zoom: start.zoom + (target.zoom - start.zoom) * progress,
    }
}

pub(super) fn top_panel_hit(
    workspace: &Workspace,
    pointer: Pos2,
    viewport: &Viewport,
    canvas_rect: Rect,
) -> Option<PanelHit> {
    let mut panel_order: Vec<_> = (0..workspace.panels.len()).collect();
    panel_order.sort_by_key(|index| workspace.panels[*index].z_index());
    panel_order.reverse();

    panel_order.into_iter().find_map(|index| {
        if workspace.panels[index].minimized() {
            None
        } else {
            workspace.panels[index]
                .hit_test(pointer, viewport, canvas_rect)
                .map(|area| PanelHit {
                    index,
                    area,
                    pointer,
                })
        }
    })
}

pub(super) fn top_panel_scroll_hit(
    workspace: &Workspace,
    pointer: Pos2,
    viewport: &Viewport,
    canvas_rect: Rect,
) -> Option<usize> {
    let mut panel_order: Vec<_> = (0..workspace.panels.len()).collect();
    panel_order.sort_by_key(|index| workspace.panels[*index].z_index());
    panel_order.reverse();

    panel_order
        .into_iter()
        .filter(|index| !workspace.panels[*index].minimized())
        .find(|index| workspace.panels[*index].scroll_hit_test(pointer, viewport, canvas_rect))
}

pub(super) fn split_resize_hit(workspace: &Workspace, pointer: Pos2) -> Option<SplitResizeHit> {
    const HIT_THICKNESS: f32 = 12.0;
    const EDGE_EPSILON: f32 = 1.0;

    let candidates: Vec<_> = workspace
        .panels
        .iter()
        .enumerate()
        .filter(|(_, panel)| !panel.minimized())
        .filter(|(_, panel)| matches!(panel.placement(), PanelPlacement::Snapped(_)))
        .map(|(index, panel)| (index, panel.rect()))
        .collect();

    for (i, (left_index, left_rect)) in candidates.iter().enumerate() {
        for (right_index, right_rect) in candidates.iter().skip(i + 1) {
            if (left_rect.right() - right_rect.left()).abs() <= EDGE_EPSILON
                || (right_rect.right() - left_rect.left()).abs() <= EDGE_EPSILON
            {
                let (leading_index, leading_rect, trailing_index, trailing_rect, boundary) =
                    if left_rect.center().x <= right_rect.center().x {
                        (*left_index, *left_rect, *right_index, *right_rect, left_rect.right())
                    } else {
                        (*right_index, *right_rect, *left_index, *left_rect, right_rect.right())
                    };
                let top = leading_rect.top().max(trailing_rect.top());
                let bottom = leading_rect.bottom().min(trailing_rect.bottom());
                if bottom - top > 24.0 {
                    let hit_rect = Rect::from_min_max(
                        egui::pos2(boundary - HIT_THICKNESS * 0.5, top),
                        egui::pos2(boundary + HIT_THICKNESS * 0.5, bottom),
                    );
                    if hit_rect.contains(pointer) {
                        return Some(SplitResizeHit {
                            leading_index,
                            trailing_index,
                            axis: SplitResizeAxis::Vertical,
                            boundary,
                            hit_rect,
                        });
                    }
                }
            }

            if (left_rect.bottom() - right_rect.top()).abs() <= EDGE_EPSILON
                || (right_rect.bottom() - left_rect.top()).abs() <= EDGE_EPSILON
            {
                let (leading_index, leading_rect, trailing_index, trailing_rect, boundary) =
                    if left_rect.center().y <= right_rect.center().y {
                        (*left_index, *left_rect, *right_index, *right_rect, left_rect.bottom())
                    } else {
                        (*right_index, *right_rect, *left_index, *left_rect, right_rect.bottom())
                    };
                let left = leading_rect.left().max(trailing_rect.left());
                let right = leading_rect.right().min(trailing_rect.right());
                if right - left > 24.0 {
                    let hit_rect = Rect::from_min_max(
                        egui::pos2(left, boundary - HIT_THICKNESS * 0.5),
                        egui::pos2(right, boundary + HIT_THICKNESS * 0.5),
                    );
                    if hit_rect.contains(pointer) {
                        return Some(SplitResizeHit {
                            leading_index,
                            trailing_index,
                            axis: SplitResizeAxis::Horizontal,
                            boundary,
                            hit_rect,
                        });
                    }
                }
            }
        }
    }

    None
}
