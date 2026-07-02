use super::*;
use crate::state::{PanelPlacement, SnapSlot};
use crate::terminal::panel::snap_slot_rect;

const MAX_VISIBLE_PANELS: usize = 4;
pub(super) const SPLIT_MIN: f32 = 0.18;
pub(super) const SPLIT_MAX: f32 = 0.82;

pub(super) fn auto_layout_slots(visible_count: usize) -> Vec<SnapSlot> {
    match visible_count {
        0 => Vec::new(),
        1 => vec![SnapSlot::Maximized],
        2 => vec![SnapSlot::LeftHalf, SnapSlot::RightHalf],
        3 => vec![
            SnapSlot::LeftHalf,
            SnapSlot::TopRight,
            SnapSlot::BottomRight,
        ],
        _ => vec![
            SnapSlot::TopLeft,
            SnapSlot::TopRight,
            SnapSlot::BottomLeft,
            SnapSlot::BottomRight,
        ],
    }
}

pub(super) fn slot_rect_with_splits(
    slot: SnapSlot,
    desktop_rect: Rect,
    split_x: f32,
    split_y: f32,
) -> Rect {
    let split_x = split_x.clamp(SPLIT_MIN, SPLIT_MAX);
    let split_y = split_y.clamp(SPLIT_MIN, SPLIT_MAX);
    let mid_x = desktop_rect.left() + desktop_rect.width() * split_x;
    let mid_y = desktop_rect.top() + desktop_rect.height() * split_y;
    match slot {
        SnapSlot::Maximized => desktop_rect,
        SnapSlot::LeftHalf => {
            Rect::from_min_max(desktop_rect.min, egui::pos2(mid_x, desktop_rect.bottom()))
        }
        SnapSlot::RightHalf => {
            Rect::from_min_max(egui::pos2(mid_x, desktop_rect.top()), desktop_rect.max)
        }
        SnapSlot::TopHalf => {
            Rect::from_min_max(desktop_rect.min, egui::pos2(desktop_rect.right(), mid_y))
        }
        SnapSlot::BottomHalf => {
            Rect::from_min_max(egui::pos2(desktop_rect.left(), mid_y), desktop_rect.max)
        }
        SnapSlot::TopLeft => Rect::from_min_max(desktop_rect.min, egui::pos2(mid_x, mid_y)),
        SnapSlot::TopRight => Rect::from_min_max(
            egui::pos2(mid_x, desktop_rect.top()),
            egui::pos2(desktop_rect.right(), mid_y),
        ),
        SnapSlot::BottomLeft => Rect::from_min_max(
            egui::pos2(desktop_rect.left(), mid_y),
            egui::pos2(mid_x, desktop_rect.bottom()),
        ),
        SnapSlot::BottomRight => Rect::from_min_max(egui::pos2(mid_x, mid_y), desktop_rect.max),
    }
}

pub(super) fn auto_tile_workspace(workspace: &mut Workspace, desktop_rect: Rect) {
    let visible_count = workspace
        .panels
        .iter()
        .filter(|panel| !panel.minimized())
        .count();

    let signature = (
        visible_count,
        [
            desktop_rect.min.x.round() as i32,
            desktop_rect.min.y.round() as i32,
            desktop_rect.max.x.round() as i32,
            desktop_rect.max.y.round() as i32,
        ],
        (workspace.split_x * 1000.0).round() as i32,
        (workspace.split_y * 1000.0).round() as i32,
    );
    if workspace.last_auto_tile_signature == Some(signature) {
        return;
    }

    if visible_count > MAX_VISIBLE_PANELS {
        let mut by_z: Vec<(usize, u32)> = workspace
            .panels
            .iter()
            .enumerate()
            .filter(|(_, panel)| !panel.minimized())
            .map(|(idx, panel)| (idx, panel.z_index()))
            .collect();
        by_z.sort_by_key(|(_, z)| *z);
        for (idx, _) in by_z.iter().take(visible_count - MAX_VISIBLE_PANELS) {
            workspace.panels[*idx].set_minimized(true);
        }
    }

    let mut visible: Vec<(u32, usize)> = workspace
        .panels
        .iter()
        .enumerate()
        .filter(|(_, panel)| !panel.minimized())
        .map(|(idx, panel)| (panel.z_index(), idx))
        .collect();
    visible.sort_by_key(|(z, _)| *z);

    let slots = auto_layout_slots(visible.len());
    let split_x = workspace.split_x;
    let split_y = workspace.split_y;
    for ((_, idx), slot) in visible.iter().zip(slots.iter()) {
        let panel = &mut workspace.panels[*idx];
        let rect = slot_rect_with_splits(*slot, desktop_rect, split_x, split_y);
        panel.set_placement(PanelPlacement::Snapped(*slot));
        panel.set_restore_placement(None);
        panel.set_restore_bounds(Some(rect));
        panel.apply_resize(rect);
        panel.set_drag_virtual_pos(None);
        panel.set_resize_virtual_rect(None);
    }

    workspace.last_auto_tile_signature = Some(signature);
}

pub(super) fn taskbar_provider_label(
    overlay_provider: Option<AgentProvider>,
    panel_provider: Option<AgentProvider>,
    title: &str,
) -> AgentProvider {
    overlay_provider
        .or(panel_provider)
        .or_else(|| AgentProvider::detect(title))
        .unwrap_or(AgentProvider::Unknown)
}

pub(super) fn taskbar_provider_accent(provider: AgentProvider) -> Color32 {
    match provider {
        AgentProvider::CodexCli => Color32::from_rgb(120, 190, 255),
        AgentProvider::ClaudeCode => Color32::from_rgb(255, 180, 90),
        AgentProvider::OpenCode => Color32::from_rgb(176, 176, 184),
        AgentProvider::GeminiCli => Color32::from_rgb(96, 230, 196),
        AgentProvider::Aider => Color32::from_rgb(214, 152, 255),
        AgentProvider::Unknown => Color32::from_rgb(108, 108, 116),
    }
}

pub(super) fn desktop_canvas_rect(canvas_rect: Rect) -> Rect {
    let width = canvas_rect.width().max(DESKTOP_MARGIN * 2.0 + 1.0);
    let height = canvas_rect.height().max(DESKTOP_MARGIN * 2.0 + 1.0);
    Rect::from_min_max(
        pos2(DESKTOP_MARGIN, DESKTOP_MARGIN),
        pos2(width - DESKTOP_MARGIN, height - DESKTOP_MARGIN),
    )
}

pub(super) fn desktop_screen_rect(canvas_rect: Rect, desktop_rect: Rect) -> Rect {
    Rect::from_min_max(
        canvas_rect.min + desktop_rect.min.to_vec2(),
        canvas_rect.min + desktop_rect.max.to_vec2(),
    )
}

pub(super) fn clamp_rect_to_desktop(rect: Rect, desktop_rect: Rect) -> Rect {
    let min_width = crate::terminal::panel::MIN_WIDTH.min(desktop_rect.width());
    let min_height = crate::terminal::panel::MIN_HEIGHT.min(desktop_rect.height());
    let width = rect.width().clamp(min_width, desktop_rect.width());
    let height = rect.height().clamp(min_height, desktop_rect.height());
    let max_x = desktop_rect.right() - width;
    let max_y = desktop_rect.bottom() - height;
    let min = pos2(
        rect.min
            .x
            .clamp(desktop_rect.left(), max_x.max(desktop_rect.left())),
        rect.min
            .y
            .clamp(desktop_rect.top(), max_y.max(desktop_rect.top())),
    );
    Rect::from_min_size(min, vec2(width, height))
}

pub(super) fn desktop_snap_slot_for_pointer(pointer: Pos2, desktop_rect: Rect) -> Option<SnapSlot> {
    let horizontal_band = DESKTOP_SNAP_EDGE.max(64.0).min(desktop_rect.width() * 0.22);
    let vertical_band = DESKTOP_SNAP_EDGE
        .max(64.0)
        .min(desktop_rect.height() * 0.22);
    let near_left = pointer.x <= desktop_rect.left() + horizontal_band;
    let near_right = pointer.x >= desktop_rect.right() - horizontal_band;
    let near_top = pointer.y <= desktop_rect.top() + vertical_band;
    let near_bottom = pointer.y >= desktop_rect.bottom() - vertical_band;
    let left_third = desktop_rect.left() + desktop_rect.width() / 3.0;
    let right_third = desktop_rect.right() - desktop_rect.width() / 3.0;
    let top_third = desktop_rect.top() + desktop_rect.height() / 3.0;
    let bottom_third = desktop_rect.bottom() - desktop_rect.height() / 3.0;

    if near_top {
        if pointer.x <= left_third {
            Some(SnapSlot::TopLeft)
        } else if pointer.x >= right_third {
            Some(SnapSlot::TopRight)
        } else {
            Some(SnapSlot::Maximized)
        }
    } else if near_bottom {
        if pointer.x <= left_third {
            Some(SnapSlot::BottomLeft)
        } else if pointer.x >= right_third {
            Some(SnapSlot::BottomRight)
        } else {
            Some(SnapSlot::BottomHalf)
        }
    } else if near_left {
        if pointer.y <= top_third {
            Some(SnapSlot::TopLeft)
        } else if pointer.y >= bottom_third {
            Some(SnapSlot::BottomLeft)
        } else {
            Some(SnapSlot::LeftHalf)
        }
    } else if near_right {
        if pointer.y <= top_third {
            Some(SnapSlot::TopRight)
        } else if pointer.y >= bottom_third {
            Some(SnapSlot::BottomRight)
        } else {
            Some(SnapSlot::RightHalf)
        }
    } else {
        None
    }
}

pub(super) fn desktop_required_snap_slot_for_pointer(
    pointer: Pos2,
    desktop_rect: Rect,
    fallback: SnapSlot,
) -> SnapSlot {
    if let Some(slot) = desktop_snap_slot_for_pointer(pointer, desktop_rect) {
        return slot;
    }

    let width = desktop_rect.width().max(1.0);
    let height = desktop_rect.height().max(1.0);
    let x = ((pointer.x - desktop_rect.left()) / width).clamp(0.0, 1.0);
    let y = ((pointer.y - desktop_rect.top()) / height).clamp(0.0, 1.0);
    let left = x < 0.33;
    let right = x > 0.67;
    let top = y < 0.33;
    let bottom = y > 0.67;

    match (left, right, top, bottom) {
        (true, _, true, _) => SnapSlot::TopLeft,
        (true, _, _, true) => SnapSlot::BottomLeft,
        (true, _, _, _) => SnapSlot::LeftHalf,
        (_, true, true, _) => SnapSlot::TopRight,
        (_, true, _, true) => SnapSlot::BottomRight,
        (_, true, _, _) => SnapSlot::RightHalf,
        (_, _, true, _) => SnapSlot::TopHalf,
        (_, _, _, true) => SnapSlot::BottomHalf,
        _ => fallback,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn desktop_snap_rect_for_pointer(pointer: Pos2, desktop_rect: Rect) -> Option<Rect> {
    desktop_snap_slot_for_pointer(pointer, desktop_rect)
        .map(|slot| snap_slot_rect(slot, desktop_rect))
}

pub(super) fn clamp_workspace_panels_to_desktop(workspace: &mut Workspace, desktop_rect: Rect) {
    auto_tile_workspace(workspace, desktop_rect);
}

pub(super) fn truncate_taskbar_title(title: &str) -> String {
    const MAX_CHARS: usize = 18;
    let count = title.chars().count();
    if count <= MAX_CHARS {
        title.to_owned()
    } else {
        format!(
            "{}…",
            title
                .chars()
                .take(MAX_CHARS.saturating_sub(1))
                .collect::<String>()
        )
    }
}

impl TerminalApp {
    /// Barra inferior de ventanas: un botón por panel (con títulos
    /// desambiguados) y restauración animada desde el botón.
    pub(super) fn show_taskbar(&mut self, ctx: &egui::Context) {
        if !matches!(self.collab.mode(), CollabMode::Guest) {
            let mut requested_panel = None;
            let panel_count = self.ws().panels.len();
            let mut taskbar_button_rects = HashMap::with_capacity(panel_count);
            // Stable creation order so clicks don't reshuffle items by z-index.
            let taskbar_panels: Vec<_> = self
                .ws()
                .panels
                .iter()
                .map(|panel| {
                    let provider = taskbar_provider_label(
                        self.orchestrator
                            .panel_overlay(panel.id())
                            .map(|overlay| overlay.provider),
                        panel.provider_hint(),
                        panel.title(),
                    );
                    (
                        panel.id(),
                        panel.title().to_owned(),
                        panel.minimized(),
                        panel.focused(),
                        provider,
                    )
                })
                .collect();

            // Disambiguate duplicate titles: append " N" to repeated entries so
            // the user can tell them apart in the bottom selector.
            let mut title_counts: HashMap<String, usize> = HashMap::with_capacity(panel_count);
            for (_, title, ..) in &taskbar_panels {
                if let Some(c) = title_counts.get_mut(title.as_str()) {
                    *c += 1;
                } else {
                    title_counts.insert(title.clone(), 1);
                }
            }
            let mut title_seen: HashMap<String, usize> = HashMap::with_capacity(panel_count);
            let taskbar_panels: Vec<_> = taskbar_panels
                .into_iter()
                .map(|(id, title, minimized, focused, provider)| {
                    let total = title_counts.get(&title).copied().unwrap_or(1);
                    let display = if total > 1 {
                        let n = if let Some(n) = title_seen.get_mut(title.as_str()) {
                            *n += 1;
                            *n
                        } else {
                            title_seen.insert(title.clone(), 1);
                            1
                        };
                        format!("{title} {n}")
                    } else {
                        title
                    };
                    (id, display, minimized, focused, provider)
                })
                .collect();
            TopBottomPanel::bottom("window-taskbar")
                .resizable(false)
                .exact_height(34.0)
                .frame(
                    egui::Frame::none()
                        .fill(palette::INK)
                        .inner_margin(egui::Margin::symmetric(14.0, 6.0)),
                )
                .show_separator_line(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.style_mut().spacing.item_spacing.x = 6.0;
                        for (panel_id, title, minimized, focused, provider) in &taskbar_panels {
                            let truncated = truncate_taskbar_title(title);
                            let font = egui::FontId::proportional(11.5);
                            let text_w = ui.fonts(|f| {
                                f.layout_no_wrap(truncated.clone(), font.clone(), palette::TEXT)
                                    .size()
                                    .x
                            });
                            let dot_size = 7.0;
                            let dot_pad = 8.0;
                            let pad_x = 10.0;
                            let item_w = text_w + dot_size + dot_pad + pad_x * 2.0;
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(item_w, 26.0),
                                egui::Sense::click(),
                            );
                            let dot_x = rect.left() + pad_x + dot_size * 0.5;
                            let text_x = rect.left() + pad_x + dot_size + dot_pad;
                            if response.hovered() && !*focused {
                                ui.painter().rect_filled(rect, 4.0, palette::RAISED);
                            }
                            let accent = taskbar_provider_accent(*provider);
                            let dot_color = if *minimized {
                                accent.linear_multiply(0.55)
                            } else {
                                accent
                            };
                            ui.painter().circle_filled(
                                egui::pos2(dot_x, rect.center().y),
                                dot_size * 0.5,
                                dot_color,
                            );
                            let text_color = if *focused {
                                palette::TEXT_STRONG
                            } else if *minimized {
                                palette::DIM
                            } else if response.hovered() {
                                palette::TEXT_STRONG
                            } else {
                                palette::TEXT
                            };
                            ui.painter().text(
                                egui::pos2(text_x, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                &truncated,
                                font,
                                text_color,
                            );
                            if *focused {
                                let underline_y = rect.bottom() - 3.0;
                                ui.painter().line_segment(
                                    [
                                        egui::pos2(text_x, underline_y),
                                        egui::pos2(text_x + text_w, underline_y),
                                    ],
                                    Stroke::new(1.0, palette::TEXT_STRONG),
                                );
                            }
                            taskbar_button_rects.insert(*panel_id, rect);
                            if response.clicked() {
                                requested_panel = Some(*panel_id);
                            }
                        }
                    });
                });
            self.taskbar_button_rects = taskbar_button_rects;
            self.layout_menu_open = false;
            if let Some(panel_id) = requested_panel {
                if self
                    .ws()
                    .panel(panel_id)
                    .map(|panel| panel.minimized())
                    .unwrap_or(false)
                {
                    if let (Some(canvas_rect), Some(button_rect)) = (
                        Some(ctx.available_rect()),
                        self.taskbar_button_rects.get(&panel_id).copied(),
                    ) {
                        let desktop_rect = desktop_canvas_rect(canvas_rect);
                        self.ws_mut()
                            .restore_panel_with_desktop(panel_id, desktop_rect);
                        if let Some(panel) = self.ws().panel(panel_id) {
                            let target_rect = self.panel_screen_rect(panel.rect(), canvas_rect);
                            let now = ctx.input(|i| i.time);
                            self.start_window_transition(
                                panel_id,
                                WindowTransitionKind::Restoring,
                                button_rect,
                                target_rect,
                                now,
                            );
                        }
                    } else {
                        self.focus_panel_across_workspaces(panel_id, Some(ctx.available_rect()));
                    }
                } else {
                    self.focus_panel_across_workspaces(panel_id, Some(ctx.available_rect()));
                }
            }
        }
    }
}
