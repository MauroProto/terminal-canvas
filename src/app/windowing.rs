//! Gestos de ventana del escritorio: drag/resize de paneles, sash global de
//! los layouts divididos y transiciones de minimizar/restaurar.

use egui::{pos2, Color32, Pos2, Rect, Stroke, Vec2};
use uuid::Uuid;

use crate::canvas::config::SNAP_GUIDE_COLOR;
use crate::canvas::snap::{guide_endpoints, SnapGuide};
use crate::state::{PanelPlacement, SnapSlot, Workspace};
use crate::terminal::panel::{PanelHitArea, ResizeHandle};
use crate::theme::colors as palette;

use super::desktop::{self, SplitResizeAxis};
use super::taskbar::{self, clamp_rect_to_desktop, desktop_required_snap_slot_for_pointer};
use super::TerminalApp;

pub(super) const SLOT_DRAG_START_DISTANCE: f32 = 6.0;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GlobalSashAxis {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct GlobalSashDrag {
    pub(super) axis: GlobalSashAxis,
}

pub(super) const SASH_HIT_THICKNESS: f32 = 8.0;

pub(super) fn detect_global_sash(
    visible_count: usize,
    pos: Pos2,
    desktop: Rect,
    split_x: f32,
    split_y: f32,
) -> Option<GlobalSashAxis> {
    if visible_count < 2 {
        return None;
    }
    let split_x = split_x.clamp(taskbar::SPLIT_MIN, taskbar::SPLIT_MAX);
    let split_y = split_y.clamp(taskbar::SPLIT_MIN, taskbar::SPLIT_MAX);
    let mid_x = desktop.left() + desktop.width() * split_x;
    let mid_y = desktop.top() + desktop.height() * split_y;

    let on_vertical = (pos.x - mid_x).abs() <= SASH_HIT_THICKNESS;
    let on_horizontal = (pos.y - mid_y).abs() <= SASH_HIT_THICKNESS;

    match visible_count {
        2 => {
            if on_vertical {
                Some(GlobalSashAxis::Vertical)
            } else {
                None
            }
        }
        3 => {
            if on_vertical {
                Some(GlobalSashAxis::Vertical)
            } else if on_horizontal && pos.x >= mid_x {
                // El sash horizontal solo existe en la columna derecha.
                Some(GlobalSashAxis::Horizontal)
            } else {
                None
            }
        }
        _ => {
            if on_vertical && on_horizontal {
                // Esquina central: priorizamos el más cercano al cursor.
                if (pos.x - mid_x).abs() < (pos.y - mid_y).abs() {
                    Some(GlobalSashAxis::Vertical)
                } else {
                    Some(GlobalSashAxis::Horizontal)
                }
            } else if on_vertical {
                Some(GlobalSashAxis::Vertical)
            } else if on_horizontal {
                Some(GlobalSashAxis::Horizontal)
            } else {
                None
            }
        }
    }
}
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(super) enum PanelGestureKind {
    Drag {
        fallback_slot: SnapSlot,
    },
    Resize {
        handle: ResizeHandle,
        origin: Rect,
    },
    SplitResize {
        other_panel_id: Uuid,
        axis: SplitResizeAxis,
        origin: Rect,
        other_origin: Rect,
        boundary: f32,
    },
}

#[derive(Clone, Copy)]
pub(super) struct PanelGesture {
    pub(super) panel_id: Uuid,
    pub(super) pointer_origin: Pos2,
    pub(super) kind: PanelGestureKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WindowTransitionKind {
    Minimizing,
    Restoring,
}

#[derive(Clone, Copy)]
pub(super) struct WindowTransition {
    pub(super) kind: WindowTransitionKind,
    pub(super) from_rect: Rect,
    pub(super) to_rect: Rect,
    pub(super) started_at: f64,
    pub(super) duration: f64,
}

impl WindowTransition {
    pub(super) fn progress(self, now: f64) -> f32 {
        ((now - self.started_at) / self.duration).clamp(0.0, 1.0) as f32
    }

    pub(super) fn finished(self, now: f64) -> bool {
        self.progress(now) >= 1.0
    }

    pub(super) fn current_rect(self, now: f64) -> Rect {
        let t = self.progress(now);
        let eased = 1.0 - (1.0 - t).powi(3);
        Rect::from_min_max(
            self.from_rect.min + (self.to_rect.min - self.from_rect.min) * eased,
            self.from_rect.max + (self.to_rect.max - self.from_rect.max) * eased,
        )
    }
}

pub(super) fn panel_id_for_hit(workspace: &Workspace, hit: &desktop::PanelHit) -> Option<Uuid> {
    workspace.panels.get(hit.index).map(|panel| panel.id())
}

pub(super) fn panel_id_for_index(workspace: &Workspace, index: usize) -> Option<Uuid> {
    workspace.panels.get(index).map(|panel| panel.id())
}

pub(super) fn split_resize_panel_ids(
    workspace: &Workspace,
    hit: &desktop::SplitResizeHit,
) -> Option<(Uuid, Uuid)> {
    Some((
        workspace.panels.get(hit.leading_index)?.id(),
        workspace.panels.get(hit.trailing_index)?.id(),
    ))
}

pub(super) fn split_resize_snapshot(
    workspace: &Workspace,
    hit: &desktop::SplitResizeHit,
) -> Option<(Uuid, Uuid, Rect, Rect)> {
    Some((
        workspace.panels.get(hit.leading_index)?.id(),
        workspace.panels.get(hit.trailing_index)?.id(),
        workspace.panels.get(hit.leading_index)?.rect(),
        workspace.panels.get(hit.trailing_index)?.rect(),
    ))
}

pub(super) fn drag_fallback_slot(
    placement: &PanelPlacement,
    rect: Rect,
    desktop_rect: Rect,
) -> SnapSlot {
    match placement {
        PanelPlacement::Snapped(slot) => *slot,
        PanelPlacement::Maximized => SnapSlot::Maximized,
        PanelPlacement::Floating => {
            desktop_required_snap_slot_for_pointer(rect.center(), desktop_rect, SnapSlot::Maximized)
        }
    }
}

pub(super) fn slot_drag_started(pointer_delta: Vec2) -> bool {
    pointer_delta.length_sq() >= SLOT_DRAG_START_DISTANCE * SLOT_DRAG_START_DISTANCE
}

impl TerminalApp {
    pub(super) fn is_panel_transitioning(&self, panel_id: Uuid) -> bool {
        self.window_transitions.contains_key(&panel_id)
    }

    pub(super) fn start_window_transition(
        &mut self,
        panel_id: Uuid,
        kind: WindowTransitionKind,
        from_rect: Rect,
        to_rect: Rect,
        now: f64,
    ) {
        self.window_transitions.insert(
            panel_id,
            WindowTransition {
                kind,
                from_rect,
                to_rect,
                started_at: now,
                duration: 0.14,
            },
        );
    }

    pub(super) fn panel_screen_rect(&self, panel_rect: Rect, canvas_rect: Rect) -> Rect {
        Rect::from_min_size(
            self.viewport.canvas_to_screen(panel_rect.min, canvas_rect),
            panel_rect.size() * self.viewport.zoom,
        )
    }

    pub(super) fn sync_window_transitions(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        self.window_transitions
            .retain(|_, transition| !transition.finished(now));
        if !self.window_transitions.is_empty() {
            ctx.request_repaint();
        }
    }

    pub(super) fn draw_window_transitions(&self, ui: &egui::Ui) {
        let now = ui.ctx().input(|i| i.time);
        let painter = ui.painter();
        for transition in self.window_transitions.values() {
            let rect = transition.current_rect(now);
            let progress = transition.progress(now);
            let alpha = match transition.kind {
                WindowTransitionKind::Minimizing => (1.0 - progress) * 110.0 + 18.0,
                WindowTransitionKind::Restoring => (1.0 - progress) * 80.0 + 12.0,
            }
            .round()
            .clamp(0.0, 255.0) as u8;
            let stroke_alpha = match transition.kind {
                WindowTransitionKind::Minimizing => (1.0 - progress) * 150.0 + 40.0,
                WindowTransitionKind::Restoring => (1.0 - progress) * 120.0 + 34.0,
            }
            .round()
            .clamp(0.0, 255.0) as u8;
            painter.rect_filled(
                rect,
                14.0,
                Color32::from_rgba_premultiplied(26, 26, 26, alpha),
            );
            painter.rect_stroke(
                rect,
                14.0,
                Stroke::new(
                    1.0,
                    Color32::from_rgba_premultiplied(244, 244, 244, stroke_alpha),
                ),
            );
        }
    }
}

impl TerminalApp {
    /// Sash global de los layouts divididos: hover, cursor y drag del ratio.
    /// Devuelve si el sash captura el puntero este frame.
    pub(super) fn update_global_sash(
        &mut self,
        ctx: &egui::Context,
        desktop_screen: Rect,
        pointer_pos: Option<Pos2>,
        primary_pressed: bool,
        primary_released: bool,
        primary_down: bool,
    ) -> bool {
        let visible_count = self
            .ws()
            .panels
            .iter()
            .filter(|panel| !panel.minimized())
            .count();
        let sash_hover = pointer_pos.and_then(|pos| {
            if !desktop_screen.contains(pos) {
                return None;
            }
            detect_global_sash(
                visible_count,
                pos,
                desktop_screen,
                self.ws().split_x,
                self.ws().split_y,
            )
        });
        let cursor_axis = self.global_sash_drag.map(|d| d.axis).or(sash_hover);
        if let Some(axis) = cursor_axis {
            ctx.output_mut(|output| {
                output.cursor_icon = match axis {
                    GlobalSashAxis::Vertical => egui::CursorIcon::ResizeHorizontal,
                    GlobalSashAxis::Horizontal => egui::CursorIcon::ResizeVertical,
                };
            });
        }
        if primary_pressed {
            if let Some(axis) = sash_hover {
                self.global_sash_drag = Some(GlobalSashDrag { axis });
            }
        }
        if primary_released {
            self.global_sash_drag = None;
        }
        if primary_down {
            if let (Some(drag), Some(pos)) = (self.global_sash_drag, pointer_pos) {
                let ws = self.ws_mut();
                match drag.axis {
                    GlobalSashAxis::Vertical => {
                        let width = desktop_screen.width().max(1.0);
                        let ratio = ((pos.x - desktop_screen.left()) / width)
                            .clamp(taskbar::SPLIT_MIN, taskbar::SPLIT_MAX);
                        ws.split_x = ratio;
                    }
                    GlobalSashAxis::Horizontal => {
                        let height = desktop_screen.height().max(1.0);
                        let ratio = ((pos.y - desktop_screen.top()) / height)
                            .clamp(taskbar::SPLIT_MIN, taskbar::SPLIT_MAX);
                        ws.split_y = ratio;
                    }
                }
                ctx.request_repaint();
            }
        }
        self.global_sash_drag.is_some() || sash_hover.is_some()
    }

    /// Arranque de gesto en primary_pressed: foco del panel golpeado y alta
    /// del gesto de resize o split-resize (el drag de titlebar está
    /// deshabilitado a propósito).
    pub(super) fn begin_panel_gesture(
        &mut self,
        split_hit: Option<desktop::SplitResizeHit>,
        hovered_hit: Option<desktop::PanelHit>,
        _desktop_rect: Rect,
        pointer_pos: Option<Pos2>,
    ) {
        match (split_hit, hovered_hit) {
            (Some(split_hit), _) => {
                if let Some((leading_id, trailing_id, leading_rect, trailing_rect)) =
                    split_resize_snapshot(self.ws(), &split_hit)
                {
                    self.panel_gesture = Some(PanelGesture {
                        panel_id: leading_id,
                        pointer_origin: pointer_pos.unwrap_or_default(),
                        kind: PanelGestureKind::SplitResize {
                            other_panel_id: trailing_id,
                            axis: split_hit.axis,
                            origin: leading_rect,
                            other_origin: trailing_rect,
                            boundary: split_hit.boundary,
                        },
                    });
                } else {
                    self.panel_gesture = None;
                }
            }
            (None, Some(hit)) => {
                if let Some(panel_id) = panel_id_for_hit(self.ws(), &hit) {
                    let already_focused =
                        self.ws().focused_panel().map(|panel| panel.id()) == Some(panel_id);
                    if !already_focused {
                        self.ws_mut().bring_to_front(panel_id);
                    }
                    match hit.area {
                        PanelHitArea::TitleBar => {
                            // Drag de la titlebar deshabilitado: las
                            // terminales viven solo en los slots fijos
                            // del auto-tile.
                            self.panel_gesture = None;
                            let _ = drag_fallback_slot;
                        }
                        PanelHitArea::Resize(_) => {
                            // El escritorio es tiling fijo: los tamaños se
                            // ajustan con el sash global entre paneles. El
                            // resize libre por borde peleaba con el auto-tile
                            // (el panel flotaba y el próximo re-tile lo
                            // devolvía al slot: "se movía solo").
                            self.panel_gesture = None;
                        }
                        PanelHitArea::Body
                        | PanelHitArea::CloseButton
                        | PanelHitArea::MinimizeButton => {
                            self.panel_gesture = None;
                        }
                    }
                } else {
                    self.panel_gesture = None;
                }
            }
            (None, None) => {
                self.panel_gesture = None;
                self.ws_mut().unfocus_all();
            }
        }
    }

    /// Gesto activo en primary_down: aplica drag/resize/split-resize y
    /// devuelve las guías de snap a dibujar este frame.
    pub(super) fn drive_panel_gesture(
        &mut self,
        canvas_rect: Rect,
        desktop_rect: Rect,
        pointer_pos: Option<Pos2>,
        snap_preview_rect: &mut Option<Rect>,
        split_preview_rect: &mut Option<Rect>,
    ) -> Vec<SnapGuide> {
        let mut guides = Vec::new();
        if let (Some(gesture), Some(pointer)) = (self.panel_gesture, pointer_pos) {
            let pointer_delta = pointer - gesture.pointer_origin;
            let zoom = self.viewport.zoom;
            let pointer_canvas = self.viewport.screen_to_canvas(pointer, canvas_rect);
            guides = match gesture.kind {
                PanelGestureKind::Drag { fallback_slot, .. } => {
                    if slot_drag_started(pointer_delta) {
                        let target_slot = desktop_required_snap_slot_for_pointer(
                            pointer_canvas,
                            desktop_rect,
                            fallback_slot,
                        );
                        if let Some(panel) = self
                            .ws_mut()
                            .panels
                            .iter_mut()
                            .find(|panel| panel.id() == gesture.panel_id)
                        {
                            if matches!(target_slot, SnapSlot::Maximized) {
                                panel.maximize(desktop_rect);
                            } else {
                                panel.snap_to(target_slot, desktop_rect);
                            }
                            panel.set_drag_virtual_pos(None);
                            *snap_preview_rect = Some(crate::terminal::panel::snap_slot_rect(
                                target_slot,
                                desktop_rect,
                            ));
                        } else {
                            *snap_preview_rect = None;
                        }
                    }
                    Vec::new()
                }
                PanelGestureKind::Resize { handle, origin } => {
                    let other_rects = self.ws().panel_rects_except(gesture.panel_id);
                    if let Some(panel) = self
                        .ws_mut()
                        .panels
                        .iter_mut()
                        .find(|panel| panel.id() == gesture.panel_id)
                    {
                        panel.set_resize_virtual_rect(Some(origin));
                        let guides =
                            panel.resize_to(handle, origin, pointer_delta, zoom, &other_rects);
                        let clamped = clamp_rect_to_desktop(panel.rect(), desktop_rect);
                        panel.apply_resize(clamped);
                        panel.set_placement(PanelPlacement::Floating);
                        panel.set_restore_placement(None);
                        panel.set_restore_bounds(Some(clamped));
                        guides
                    } else {
                        Vec::new()
                    }
                }
                PanelGestureKind::SplitResize {
                    other_panel_id,
                    axis,
                    origin,
                    other_origin,
                    boundary,
                } => {
                    if let Some((leading, trailing)) = self
                        .ws_mut()
                        .panel_pair_mut(gesture.panel_id, other_panel_id)
                    {
                        match axis {
                            SplitResizeAxis::Vertical => {
                                let min_boundary =
                                    origin.left() + crate::terminal::panel::MIN_WIDTH;
                                let max_boundary =
                                    other_origin.right() - crate::terminal::panel::MIN_WIDTH;
                                let new_boundary = (boundary + pointer_delta.x / zoom.max(0.01))
                                    .clamp(min_boundary, max_boundary);
                                let leading_rect = Rect::from_min_max(
                                    origin.min,
                                    pos2(new_boundary, origin.max.y),
                                );
                                let trailing_rect = Rect::from_min_max(
                                    pos2(new_boundary, other_origin.min.y),
                                    other_origin.max,
                                );
                                leading.apply_resize(leading_rect);
                                trailing.apply_resize(trailing_rect);
                                leading.set_restore_bounds(Some(leading_rect));
                                trailing.set_restore_bounds(Some(trailing_rect));
                                *split_preview_rect = Some(Rect::from_min_max(
                                    pos2(new_boundary - 2.0, origin.top().max(other_origin.top())),
                                    pos2(
                                        new_boundary + 2.0,
                                        origin.bottom().min(other_origin.bottom()),
                                    ),
                                ));
                            }
                            SplitResizeAxis::Horizontal => {
                                let min_boundary =
                                    origin.top() + crate::terminal::panel::MIN_HEIGHT;
                                let max_boundary =
                                    other_origin.bottom() - crate::terminal::panel::MIN_HEIGHT;
                                let new_boundary = (boundary + pointer_delta.y / zoom.max(0.01))
                                    .clamp(min_boundary, max_boundary);
                                let leading_rect = Rect::from_min_max(
                                    origin.min,
                                    pos2(origin.max.x, new_boundary),
                                );
                                let trailing_rect = Rect::from_min_max(
                                    pos2(other_origin.min.x, new_boundary),
                                    other_origin.max,
                                );
                                leading.apply_resize(leading_rect);
                                trailing.apply_resize(trailing_rect);
                                leading.set_restore_bounds(Some(leading_rect));
                                trailing.set_restore_bounds(Some(trailing_rect));
                                *split_preview_rect = Some(Rect::from_min_max(
                                    pos2(
                                        origin.left().max(other_origin.left()),
                                        new_boundary - 2.0,
                                    ),
                                    pos2(
                                        origin.right().min(other_origin.right()),
                                        new_boundary + 2.0,
                                    ),
                                ));
                            }
                        }
                    }
                    Vec::new()
                }
            };
        }
        guides
    }

    /// Cierre de gesto en primary_released: snap final o clamp al escritorio
    /// y limpieza de los rects virtuales de drag/resize.
    pub(super) fn finish_panel_gesture(
        &mut self,
        canvas_rect: Rect,
        desktop_rect: Rect,
        pointer_pos: Option<Pos2>,
    ) {
        if let Some(gesture) = self.panel_gesture.take() {
            let release_snap = match (gesture.kind, pointer_pos) {
                (PanelGestureKind::Drag { fallback_slot, .. }, Some(pointer))
                    if slot_drag_started(pointer - gesture.pointer_origin) =>
                {
                    let pointer_canvas = self.viewport.screen_to_canvas(pointer, canvas_rect);
                    Some(desktop_required_snap_slot_for_pointer(
                        pointer_canvas,
                        desktop_rect,
                        fallback_slot,
                    ))
                }
                _ => None,
            };
            if matches!(gesture.kind, PanelGestureKind::SplitResize { .. }) {
                if let PanelGestureKind::SplitResize { other_panel_id, .. } = gesture.kind {
                    if let Some((leading, trailing)) = self
                        .ws_mut()
                        .panel_pair_mut(gesture.panel_id, other_panel_id)
                    {
                        leading.set_drag_virtual_pos(None);
                        leading.set_resize_virtual_rect(None);
                        trailing.set_drag_virtual_pos(None);
                        trailing.set_resize_virtual_rect(None);
                    }
                }
            } else if matches!(gesture.kind, PanelGestureKind::Drag { .. })
                && release_snap.is_none()
            {
                if let Some(panel) = self
                    .ws_mut()
                    .panels
                    .iter_mut()
                    .find(|panel| panel.id() == gesture.panel_id)
                {
                    panel.set_drag_virtual_pos(None);
                    panel.set_resize_virtual_rect(None);
                }
            } else if let Some(panel) = self
                .ws_mut()
                .panels
                .iter_mut()
                .find(|panel| panel.id() == gesture.panel_id)
            {
                if let Some(slot) = release_snap {
                    if matches!(slot, SnapSlot::Maximized) {
                        panel.maximize(desktop_rect);
                    } else {
                        panel.snap_to(slot, desktop_rect);
                    }
                } else {
                    let clamped = clamp_rect_to_desktop(panel.rect(), desktop_rect);
                    panel.apply_resize(clamped);
                    panel.set_placement(PanelPlacement::Floating);
                    panel.set_restore_placement(None);
                    panel.set_restore_bounds(Some(clamped));
                }
                panel.set_drag_virtual_pos(None);
                panel.set_resize_virtual_rect(None);
            }
        }
    }

    /// Clicks sobre el chrome del panel: cerrar, minimizar (con transición
    /// hacia la taskbar) y maximizar con doble click en la titlebar.
    pub(super) fn handle_panel_clicks(
        &mut self,
        ctx: &egui::Context,
        canvas_rect: Rect,
        desktop_rect: Rect,
        hovered_hit: Option<desktop::PanelHit>,
        primary_clicked: bool,
        primary_double_clicked: bool,
    ) {
        if primary_clicked {
            if let Some(hit) = hovered_hit {
                if matches!(hit.area, PanelHitArea::CloseButton) {
                    if let Some(panel_id) = panel_id_for_hit(self.ws(), &hit) {
                        self.ws_mut().close_panel(panel_id);
                        self.reconcile_orchestration();
                    }
                } else if matches!(hit.area, PanelHitArea::MinimizeButton) {
                    if let Some(panel_id) = panel_id_for_hit(self.ws(), &hit) {
                        if let Some(panel_rect) =
                            self.ws().panel(panel_id).map(|panel| panel.rect())
                        {
                            let from_rect = self.panel_screen_rect(panel_rect, canvas_rect);
                            self.ws_mut().toggle_minimize_panel(panel_id);
                            if let Some(to_rect) = self.taskbar_button_rects.get(&panel_id).copied()
                            {
                                let now = ctx.input(|i| i.time);
                                self.start_window_transition(
                                    panel_id,
                                    WindowTransitionKind::Minimizing,
                                    from_rect,
                                    to_rect,
                                    now,
                                );
                            }
                        }
                    }
                }
            }
        }

        if primary_double_clicked {
            if let Some(hit) = hovered_hit {
                if matches!(hit.area, PanelHitArea::TitleBar) {
                    if let Some(panel_id) = panel_id_for_hit(self.ws(), &hit) {
                        self.ws_mut().maximize_panel(panel_id, desktop_rect);
                    }
                }
            }
        }
    }

    /// Overlays del escritorio: previews de snap/split, guías de alineación y
    /// transiciones de minimizar/restaurar.
    pub(super) fn draw_desktop_overlays(
        &self,
        ui: &egui::Ui,
        canvas_rect: Rect,
        snap_preview_rect: Option<Rect>,
        split_preview_rect: Option<Rect>,
        guides: Vec<SnapGuide>,
    ) {
        if let Some(snap_rect) = snap_preview_rect {
            let preview_screen = Rect::from_min_size(
                self.viewport.canvas_to_screen(snap_rect.min, canvas_rect),
                snap_rect.size() * self.viewport.zoom,
            );
            ui.painter().rect_filled(
                preview_screen,
                14.0,
                Color32::from_rgba_premultiplied(244, 244, 244, 28),
            );
            ui.painter()
                .rect_stroke(preview_screen, 14.0, Stroke::new(1.0, palette::TEXT_STRONG));
        }

        if let Some(split_rect) = split_preview_rect {
            let preview_screen = Rect::from_min_size(
                self.viewport.canvas_to_screen(split_rect.min, canvas_rect),
                split_rect.size() * self.viewport.zoom,
            );
            ui.painter().rect_filled(
                preview_screen,
                4.0,
                Color32::from_rgba_premultiplied(244, 244, 244, 120),
            );
        }

        for guide in guides {
            let [start, end] = guide_endpoints(guide);
            let start = self.viewport.canvas_to_screen(start, canvas_rect);
            let end = self.viewport.canvas_to_screen(end, canvas_rect);
            ui.painter()
                .line_segment([start, end], egui::Stroke::new(1.0, SNAP_GUIDE_COLOR));
        }

        self.draw_window_transitions(ui);
    }
}
