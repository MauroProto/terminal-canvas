use super::*;
use crate::state::SnapSlot;
use crate::terminal::panel::{normalize_snapped_rect, snap_slot_rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TaskbarLayoutPreset {
    SideBySide,
    Stacked,
    Grid,
    Cascade,
}

impl TaskbarLayoutPreset {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::SideBySide => "Lado a lado",
            Self::Stacked => "Arriba y abajo",
            Self::Grid => "Grilla",
            Self::Cascade => "Cascada",
        }
    }
}

pub(super) fn taskbar_summary_label(total: usize, minimized: usize) -> String {
    format!("{total} abiertas · {minimized} minimizadas")
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

pub(super) fn taskbar_button_colors(
    provider: AgentProvider,
    focused: bool,
    minimized: bool,
) -> (Color32, Color32, Color32) {
    let (focused_fill, idle_fill, minimized_fill, focused_stroke, idle_stroke, text, dim_text) =
        match provider {
            AgentProvider::CodexCli => (
                Color32::from_rgb(38, 82, 156),
                Color32::from_rgb(24, 56, 108),
                Color32::from_rgb(18, 38, 74),
                Color32::from_rgb(126, 184, 255),
                Color32::from_rgb(90, 142, 220),
                Color32::from_rgb(232, 242, 255),
                Color32::from_rgb(174, 198, 230),
            ),
            AgentProvider::ClaudeCode => (
                Color32::from_rgb(136, 82, 28),
                Color32::from_rgb(94, 54, 18),
                Color32::from_rgb(66, 38, 14),
                Color32::from_rgb(255, 182, 92),
                Color32::from_rgb(222, 142, 62),
                Color32::from_rgb(255, 238, 216),
                Color32::from_rgb(226, 196, 160),
            ),
            AgentProvider::OpenCode => (
                Color32::from_rgb(78, 78, 84),
                Color32::from_rgb(52, 52, 58),
                Color32::from_rgb(36, 36, 42),
                Color32::from_rgb(164, 164, 172),
                Color32::from_rgb(124, 124, 132),
                Color32::from_rgb(236, 236, 240),
                Color32::from_rgb(182, 182, 188),
            ),
            AgentProvider::GeminiCli => (
                Color32::from_rgb(24, 108, 96),
                Color32::from_rgb(18, 76, 68),
                Color32::from_rgb(14, 56, 50),
                Color32::from_rgb(108, 224, 198),
                Color32::from_rgb(78, 182, 160),
                Color32::from_rgb(224, 248, 242),
                Color32::from_rgb(170, 210, 202),
            ),
            AgentProvider::Aider => (
                Color32::from_rgb(106, 56, 136),
                Color32::from_rgb(74, 38, 98),
                Color32::from_rgb(52, 30, 70),
                Color32::from_rgb(214, 152, 255),
                Color32::from_rgb(174, 118, 220),
                Color32::from_rgb(246, 232, 255),
                Color32::from_rgb(208, 186, 228),
            ),
            AgentProvider::Unknown => (
                Color32::from_rgb(52, 52, 62),
                Color32::from_rgb(34, 34, 40),
                Color32::from_rgb(28, 28, 34),
                Color32::from_rgb(98, 98, 112),
                Color32::from_rgb(56, 56, 66),
                Color32::from_rgb(214, 214, 220),
                Color32::from_rgb(164, 164, 172),
            ),
        };

    let fill = if focused {
        focused_fill
    } else if minimized {
        minimized_fill
    } else {
        idle_fill
    };
    let stroke = if focused { focused_stroke } else { idle_stroke };
    let text_color = if minimized { dim_text } else { text };
    (fill, stroke, text_color)
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

pub(super) fn desktop_taskbar_layout_rects(
    preset: TaskbarLayoutPreset,
    count: usize,
    desktop_rect: Rect,
) -> Vec<Rect> {
    match preset {
        TaskbarLayoutPreset::SideBySide => tiled_layout_rects(count, desktop_rect, count.max(1), 1),
        TaskbarLayoutPreset::Stacked => tiled_layout_rects(count, desktop_rect, 1, count.max(1)),
        TaskbarLayoutPreset::Grid => {
            let cols = ((count as f32).sqrt().ceil() as usize).max(1);
            let rows = count.div_ceil(cols).max(1);
            tiled_layout_rects(count, desktop_rect, cols, rows)
        }
        TaskbarLayoutPreset::Cascade => cascaded_layout_rects(count, desktop_rect),
    }
}

fn tiled_layout_rects(count: usize, desktop_rect: Rect, cols: usize, rows: usize) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }

    let mut rects = Vec::with_capacity(count);
    for index in 0..count {
        let col = index % cols;
        let row = index / cols;
        let left = desktop_rect.left() + desktop_rect.width() * (col as f32 / cols as f32);
        let right = if col + 1 == cols {
            desktop_rect.right()
        } else {
            desktop_rect.left() + desktop_rect.width() * ((col + 1) as f32 / cols as f32)
        };
        let top = desktop_rect.top() + desktop_rect.height() * (row as f32 / rows as f32);
        let bottom = if row + 1 == rows {
            desktop_rect.bottom()
        } else {
            desktop_rect.top() + desktop_rect.height() * ((row + 1) as f32 / rows as f32)
        };
        rects.push(Rect::from_min_max(pos2(left, top), pos2(right, bottom)));
    }
    rects
}

fn cascaded_layout_rects(count: usize, desktop_rect: Rect) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }

    let width = (desktop_rect.width() * 0.82).clamp(
        crate::terminal::panel::MIN_WIDTH.min(desktop_rect.width()),
        desktop_rect.width(),
    );
    let height = (desktop_rect.height() * 0.82).clamp(
        crate::terminal::panel::MIN_HEIGHT.min(desktop_rect.height()),
        desktop_rect.height(),
    );
    let max_offset_x = (desktop_rect.width() - width).max(0.0);
    let max_offset_y = (desktop_rect.height() - height).max(0.0);
    let divisor = count.saturating_sub(1).max(1) as f32;
    let step_x = (max_offset_x / divisor).min(36.0);
    let step_y = (max_offset_y / divisor).min(30.0);

    (0..count)
        .map(|index| {
            let min = pos2(
                (desktop_rect.left() + step_x * index as f32).min(desktop_rect.right() - width),
                (desktop_rect.top() + step_y * index as f32).min(desktop_rect.bottom() - height),
            );
            Rect::from_min_size(min, vec2(width, height))
        })
        .collect()
}

pub(super) fn apply_taskbar_layout_to_workspace(
    workspace: &mut Workspace,
    preset: TaskbarLayoutPreset,
    desktop_rect: Rect,
) {
    let mut visible_indices: Vec<_> = workspace
        .panels
        .iter()
        .enumerate()
        .filter(|(_, panel)| !panel.minimized())
        .map(|(index, panel)| (panel.z_index(), index))
        .collect();
    visible_indices.sort_by_key(|(z, _)| *z);

    let target_rects = desktop_taskbar_layout_rects(preset, visible_indices.len(), desktop_rect);
    for ((_, index), rect) in visible_indices.into_iter().zip(target_rects) {
        let panel = &mut workspace.panels[index];
        panel.set_placement(crate::state::PanelPlacement::Floating);
        panel.set_restore_placement(None);
        panel.set_restore_bounds(Some(rect));
        panel.apply_resize(rect);
        panel.set_drag_virtual_pos(None);
        panel.set_resize_virtual_rect(None);
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
    let horizontal_band = DESKTOP_SNAP_EDGE
        .max(64.0)
        .min(desktop_rect.width() * 0.22);
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

pub(super) fn desktop_snap_rect_for_pointer(pointer: Pos2, desktop_rect: Rect) -> Option<Rect> {
    desktop_snap_slot_for_pointer(pointer, desktop_rect)
        .map(|slot| snap_slot_rect(slot, desktop_rect))
}

pub(super) fn clamp_workspace_panels_to_desktop(workspace: &mut Workspace, desktop_rect: Rect) {
    for panel in &mut workspace.panels {
        if panel.minimized() {
            continue;
        }
        match panel.placement() {
            crate::state::PanelPlacement::Floating => {
                let clamped = clamp_rect_to_desktop(panel.rect(), desktop_rect);
                panel.apply_resize(clamped);
                panel.set_restore_bounds(Some(clamped));
            }
            crate::state::PanelPlacement::Snapped(slot) => {
                let normalized = normalize_snapped_rect(*slot, panel.rect(), desktop_rect);
                panel.apply_resize(normalized);
            }
            crate::state::PanelPlacement::Maximized => {
                panel.apply_resize(desktop_rect);
            }
        }
    }
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
