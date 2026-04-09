use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{point_to_viewport, RenderableCursor, Term};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape as VteCursorShape, NamedColor,
};
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Rounding, Shape, Stroke};

use crate::terminal::colors::{brighten, dim_color, indexed_to_egui};
use crate::terminal::pty::EventProxy;

pub const FONT_SIZE: f32 = 15.0;
pub const MIN_TEXT_RENDER_FONT_SIZE: f32 = 3.4;
pub const PAD_X: f32 = 10.0;
pub const PAD_Y: f32 = 6.0;
pub const CELL_WIDTH_FACTOR: f32 = 0.6;
pub const CELL_HEIGHT_FACTOR: f32 = 1.25;
pub const CURSOR_COLOR: Color32 = Color32::from_rgb(196, 223, 255);
pub const BLINK_ON_MS: f64 = 600.0;
pub const BLINK_OFF_MS: f64 = 400.0;
pub const BLINK_CYCLE: f64 = BLINK_ON_MS + BLINK_OFF_MS;

#[derive(Clone, Copy)]
struct RenderMetrics {
    font_size: f32,
    cell_width: f32,
    cell_height: f32,
    pad_x: f32,
    pad_y: f32,
    zoom: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GridCacheKey {
    rect_min_x_bits: u32,
    rect_min_y_bits: u32,
    rect_width_bits: u32,
    rect_height_bits: u32,
    zoom_bits: u32,
    display_offset: usize,
    revision: u64,
    row_stride: usize,
}

impl GridCacheKey {
    pub(crate) fn new(
        rect: Rect,
        zoom: f32,
        display_offset: usize,
        revision: u64,
        row_stride: usize,
    ) -> Self {
        Self {
            rect_min_x_bits: rect.min.x.to_bits(),
            rect_min_y_bits: rect.min.y.to_bits(),
            rect_width_bits: rect.width().to_bits(),
            rect_height_bits: rect.height().to_bits(),
            zoom_bits: zoom.to_bits(),
            display_offset,
            revision,
            row_stride,
        }
    }
}

#[derive(Clone, Default)]
pub struct TerminalGridCache {
    key: Option<GridCacheKey>,
    shapes: Vec<Shape>,
}

impl TerminalGridCache {
    pub fn matches(&self, key: GridCacheKey) -> bool {
        self.key == Some(key)
    }

    pub fn store(&mut self, key: GridCacheKey, shapes: Vec<Shape>) {
        self.key = Some(key);
        self.shapes = shapes;
    }

    pub fn clear(&mut self) {
        self.key = None;
        self.shapes.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
    HollowBlock,
    Hidden,
}

pub fn blink_phase_visible(time: f64) -> bool {
    let phase = time % (BLINK_CYCLE / 1000.0);
    phase < (BLINK_ON_MS / 1000.0)
}

pub fn cursor_visible(focused: bool, streaming_output: bool, time: f64) -> bool {
    focused && !streaming_output && blink_phase_visible(time)
}

pub fn measure_cell(ctx: &egui::Context, font_size: f32) -> (f32, f32) {
    let font_id = FontId::monospace(font_size);
    let galley = ctx.fonts(|fonts| fonts.layout_no_wrap("M".into(), font_id, Color32::WHITE));
    (galley.size().x, galley.size().y)
}

pub fn compute_grid_size(content_width: f32, content_height: f32) -> (u16, u16) {
    let cell_w = FONT_SIZE * CELL_WIDTH_FACTOR;
    let cell_h = FONT_SIZE * CELL_HEIGHT_FACTOR;
    let cols = ((content_width - PAD_X * 2.0) / cell_w).floor().max(1.0) as u16;
    let rows = ((content_height - PAD_Y * 2.0) / cell_h).floor().max(1.0) as u16;
    (cols, rows)
}

pub fn render_terminal(
    painter: &egui::Painter,
    content_rect: Rect,
    term: &Term<EventProxy>,
    focused: bool,
    time: f64,
    zoom: f32,
    background_rounding: Rounding,
    cache: Option<&mut TerminalGridCache>,
    revision: u64,
) -> bool {
    render_terminal_with_row_stride(
        painter,
        content_rect,
        term,
        focused,
        time,
        zoom,
        background_rounding,
        1,
        cache,
        revision,
    )
}

pub fn render_terminal_reduced(
    painter: &egui::Painter,
    content_rect: Rect,
    term: &Term<EventProxy>,
    focused: bool,
    time: f64,
    zoom: f32,
    background_rounding: Rounding,
    cache: Option<&mut TerminalGridCache>,
    revision: u64,
) -> bool {
    let row_stride = reduced_row_stride(content_rect, zoom);
    render_terminal_with_row_stride(
        painter,
        content_rect,
        term,
        focused,
        time,
        zoom,
        background_rounding,
        row_stride,
        cache,
        revision,
    )
}

fn reduced_row_stride(content_rect: Rect, zoom: f32) -> usize {
    let metrics = scaled_metrics(zoom);
    if metrics.cell_height <= 0.0 {
        return 1;
    }

    let estimated_rows = (content_rect.height() / metrics.cell_height).floor() as usize;
    match estimated_rows {
        0..=12 => 1,
        13..=28 => 2,
        _ => 3,
    }
}

fn render_terminal_with_row_stride(
    painter: &egui::Painter,
    content_rect: Rect,
    term: &Term<EventProxy>,
    focused: bool,
    time: f64,
    zoom: f32,
    background_rounding: Rounding,
    row_stride: usize,
    mut cache: Option<&mut TerminalGridCache>,
    revision: u64,
) -> bool {
    if content_rect.width() <= 0.0 || content_rect.height() <= 0.0 {
        return false;
    }

    let content = term.renderable_content();
    let display_offset = content.display_offset;
    let cursor = content.cursor;
    let selection = content.selection;
    let colors = content.colors;
    let metrics = scaled_metrics(zoom);

    painter.rect_filled(
        content_rect,
        background_rounding,
        terminal_background_color(colors),
    );

    let cache_key = GridCacheKey::new(content_rect, zoom, display_offset, revision, row_stride);
    let selection_active = selection.is_some();
    if let Some(cache) = cache.as_deref_mut() {
        if !selection_active && cache.matches(cache_key) {
            painter.extend(cache.shapes.iter().cloned());
            if cursor_visible(focused, row_stride > 1, time) {
                draw_cursor(painter, content_rect, display_offset, cursor, metrics);
            }
            return true;
        }
    }

    let shapes = build_grid_shapes(painter.ctx(), content_rect, content, metrics, row_stride);
    painter.extend(shapes.iter().cloned());

    if let Some(cache) = cache.as_deref_mut() {
        if selection_active {
            cache.clear();
        } else {
            cache.store(cache_key, shapes);
        }
    }

    if cursor_visible(focused, row_stride > 1, time) {
        draw_cursor(painter, content_rect, display_offset, cursor, metrics);
    }

    false
}

fn build_grid_shapes(
    ctx: &egui::Context,
    content_rect: Rect,
    content: alacritty_terminal::term::RenderableContent<'_>,
    metrics: RenderMetrics,
    row_stride: usize,
) -> Vec<Shape> {
    let display_offset = content.display_offset;
    let cursor = content.cursor;
    let selection = content.selection;
    let colors = content.colors;
    let cells: Vec<_> = content.display_iter.collect();
    let base_x = content_rect.left() + metrics.pad_x;
    let base_y = content_rect.top() + metrics.pad_y;

    let mut background_shapes = Vec::new();
    let mut foreground_shapes = Vec::new();
    let mut current_run: Option<(Color32, f32, f32, f32)> = None;

    for indexed in &cells {
        let Some(point) = point_to_viewport(display_offset, indexed.point) else {
            continue;
        };
        let row = point.line;
        if row_stride > 1 && row % row_stride != 0 {
            continue;
        }
        let col = point.column.0;
        let cell = indexed.cell;
        let bg = background_color(cell, colors);
        let width = metrics.cell_width
            * if cell.flags.contains(Flags::WIDE_CHAR) {
                2.0
            } else {
                1.0
            };
        let sx = base_x + col as f32 * metrics.cell_width;
        let sy = base_y + row as f32 * metrics.cell_height;

        if let Some((run_color, run_x, run_y, run_w)) = current_run.as_mut() {
            if *run_color == bg && (*run_y - sy).abs() < 0.1 && (*run_x + *run_w - sx).abs() < 0.5 {
                *run_w += width;
                continue;
            }
            background_shapes.push(Shape::rect_filled(
                Rect::from_min_size(pos2(*run_x, *run_y), vec2(*run_w, metrics.cell_height)),
                0.0,
                *run_color,
            ));
        }
        current_run = Some((bg, sx, sy, width));
    }

    if let Some((color, x, y, width)) = current_run {
        background_shapes.push(Shape::rect_filled(
            Rect::from_min_size(pos2(x, y), vec2(width, metrics.cell_height)),
            0.0,
            color,
        ));
    }

    ctx.fonts(|fonts| {
        let mut push_text = |foreground_shapes: &mut Vec<Shape>,
                             pos: egui::Pos2,
                             text: String,
                             fg: Color32,
                             italic_offset: f32| {
            if metrics.font_size < MIN_TEXT_RENDER_FONT_SIZE || text.is_empty() {
                return;
            }
            foreground_shapes.push(Shape::text(
                fonts,
                pos + vec2(italic_offset * metrics.zoom.max(0.25), 0.0),
                Align2::LEFT_TOP,
                text,
                FontId::monospace(metrics.font_size),
                fg,
            ));
        };
        if row_stride > 1 {
            build_reduced_foreground_shapes(
                &cells,
                display_offset,
                cursor,
                selection,
                colors,
                base_x,
                base_y,
                metrics,
                row_stride,
                &mut background_shapes,
                &mut foreground_shapes,
                &mut push_text,
            );
        } else {
            build_full_foreground_shapes(
                &cells,
                display_offset,
                cursor,
                selection,
                colors,
                base_x,
                base_y,
                metrics,
                row_stride,
                &mut background_shapes,
                &mut foreground_shapes,
                &mut push_text,
            );
        }
    });

    background_shapes.extend(foreground_shapes);
    background_shapes
}

#[allow(clippy::too_many_arguments)]
fn build_full_foreground_shapes(
    cells: &[alacritty_terminal::grid::Indexed<&Cell>],
    display_offset: usize,
    cursor: RenderableCursor,
    selection: Option<alacritty_terminal::selection::SelectionRange>,
    colors: &Colors,
    base_x: f32,
    base_y: f32,
    metrics: RenderMetrics,
    row_stride: usize,
    background_shapes: &mut Vec<Shape>,
    foreground_shapes: &mut Vec<Shape>,
    push_text: &mut impl FnMut(&mut Vec<Shape>, egui::Pos2, String, Color32, f32),
) {
    for indexed in cells {
        let Some(point) = point_to_viewport(display_offset, indexed.point) else {
            continue;
        };
        let row = point.line;
        if row_stride > 1 && row % row_stride != 0 {
            continue;
        }
        let col = point.column.0;
        let cell = indexed.cell;

        if let Some(selection) = selection {
            if selection.contains_cell(indexed, cursor.point, cursor.shape) {
                let rect = Rect::from_min_size(
                    pos2(
                        base_x + col as f32 * metrics.cell_width,
                        base_y + row as f32 * metrics.cell_height,
                    ),
                    vec2(metrics.cell_width, metrics.cell_height),
                );
                background_shapes.push(Shape::rect_filled(
                    rect,
                    0.0,
                    Color32::from_rgba_premultiplied(80, 130, 200, 80),
                ));
            }
        }

        let ch = cell.c;
        if ch == ' ' || ch == '\0' || cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }

        let text_pos = pos2(
            base_x + col as f32 * metrics.cell_width,
            base_y + row as f32 * metrics.cell_height,
        );
        let (fg, italic_offset) = effective_text_style(cell, colors);
        if cell.flags.contains(Flags::HIDDEN) {
            continue;
        }
        push_text(
            foreground_shapes,
            text_pos,
            ch.to_string(),
            fg,
            italic_offset,
        );

        draw_decoration_shapes(foreground_shapes, text_pos, metrics, cell.flags, fg);
    }
}

#[allow(clippy::too_many_arguments)]
fn build_reduced_foreground_shapes(
    cells: &[alacritty_terminal::grid::Indexed<&Cell>],
    display_offset: usize,
    cursor: RenderableCursor,
    selection: Option<alacritty_terminal::selection::SelectionRange>,
    colors: &Colors,
    base_x: f32,
    base_y: f32,
    metrics: RenderMetrics,
    row_stride: usize,
    background_shapes: &mut Vec<Shape>,
    foreground_shapes: &mut Vec<Shape>,
    push_text: &mut impl FnMut(&mut Vec<Shape>, egui::Pos2, String, Color32, f32),
) {
    #[derive(Default)]
    struct TextRun {
        text: String,
        x: f32,
        y: f32,
        next_x: f32,
        fg: Color32,
        italic_offset: f32,
    }

    let mut run = TextRun::default();
    let mut flush_run = |run: &mut TextRun, foreground_shapes: &mut Vec<Shape>| {
        if run.text.is_empty() {
            run.text.clear();
            return;
        }
        push_text(
            foreground_shapes,
            pos2(run.x, run.y),
            run.text.clone(),
            run.fg,
            run.italic_offset,
        );
        run.text.clear();
    };

    for indexed in cells {
        let Some(point) = point_to_viewport(display_offset, indexed.point) else {
            continue;
        };
        let row = point.line;
        if row % row_stride != 0 {
            continue;
        }
        let col = point.column.0;
        let cell = indexed.cell;
        let text_pos = pos2(
            base_x + col as f32 * metrics.cell_width,
            base_y + row as f32 * metrics.cell_height,
        );

        if let Some(selection) = selection {
            if selection.contains_cell(indexed, cursor.point, cursor.shape) {
                background_shapes.push(Shape::rect_filled(
                    Rect::from_min_size(text_pos, vec2(metrics.cell_width, metrics.cell_height)),
                    0.0,
                    Color32::from_rgba_premultiplied(80, 130, 200, 80),
                ));
            }
        }

        let ch = cell.c;
        let (fg, italic_offset) = effective_text_style(cell, colors);
        let width = metrics.cell_width
            * if cell.flags.contains(Flags::WIDE_CHAR) {
                2.0
            } else {
                1.0
            };
        let drawable = ch != ' '
            && ch != '\0'
            && !cell.flags.contains(Flags::WIDE_CHAR_SPACER)
            && !cell.flags.contains(Flags::HIDDEN)
            && !cell.flags.intersects(Flags::ALL_UNDERLINES)
            && !cell.flags.contains(Flags::STRIKEOUT);
        let same_run = !run.text.is_empty()
            && run.fg == fg
            && (run.y - text_pos.y).abs() < 0.1
            && (run.next_x - text_pos.x).abs() < 0.5
            && (run.italic_offset - italic_offset).abs() < 0.1;

        if drawable && same_run {
            run.text.push(ch);
            run.next_x += width;
            continue;
        }

        flush_run(&mut run, foreground_shapes);

        if drawable {
            run.text.push(ch);
            run.x = text_pos.x;
            run.y = text_pos.y;
            run.next_x = text_pos.x + width;
            run.fg = fg;
            run.italic_offset = italic_offset;
        } else {
            draw_decoration_shapes(foreground_shapes, text_pos, metrics, cell.flags, fg);
        }
    }

    flush_run(&mut run, foreground_shapes);
}

fn effective_text_style(cell: &Cell, colors: &Colors) -> (Color32, f32) {
    let mut fg = foreground_color(cell, colors);
    if cell.flags.contains(Flags::BOLD) {
        fg = brighten(fg);
    }
    if cell.flags.contains(Flags::DIM) {
        fg = dim_color(fg);
    }
    let italic_offset = if cell.flags.contains(Flags::ITALIC) {
        1.5
    } else {
        0.0
    };
    (fg, italic_offset)
}

fn draw_decoration_shapes(
    foreground_shapes: &mut Vec<Shape>,
    text_pos: egui::Pos2,
    metrics: RenderMetrics,
    flags: Flags,
    fg: Color32,
) {
    if flags.intersects(Flags::ALL_UNDERLINES) {
        let y = text_pos.y + metrics.cell_height - 1.0;
        foreground_shapes.push(Shape::line_segment(
            [
                pos2(text_pos.x, y),
                pos2(text_pos.x + metrics.cell_width, y),
            ],
            Stroke::new(1.0, fg),
        ));
    }
    if flags.contains(Flags::STRIKEOUT) {
        let y = text_pos.y + metrics.cell_height * 0.5;
        foreground_shapes.push(Shape::line_segment(
            [
                pos2(text_pos.x, y),
                pos2(text_pos.x + metrics.cell_width, y),
            ],
            Stroke::new(1.0, fg),
        ));
    }
}

pub fn render_terminal_preview(
    painter: &egui::Painter,
    content_rect: Rect,
    focused: bool,
    zoom: f32,
    label: Option<&str>,
) {
    if content_rect.width() < 24.0 || content_rect.height() < 18.0 {
        return;
    }

    let zoom = zoom.clamp(0.35, 1.0);
    let inset_x = (14.0 * zoom).clamp(6.0, 14.0);
    let top = content_rect.top() + (14.0 * zoom).clamp(5.0, 14.0);
    let line_step = (18.0 * zoom).clamp(7.0, 18.0);
    let stroke = Stroke::new((1.2 * zoom).clamp(0.8, 1.2), preview_color(focused, 90));
    let widths = [0.34, 0.62, 0.48, 0.74, 0.57, 0.41];
    let max_lines = ((content_rect.height() / line_step).floor() as usize).clamp(2, 9);

    for index in 0..max_lines {
        let y = top + index as f32 * line_step;
        if y >= content_rect.bottom() - 3.0 {
            break;
        }
        let width = (content_rect.width() - inset_x * 2.0).max(8.0) * widths[index % widths.len()];
        painter.line_segment(
            [
                pos2(content_rect.left() + inset_x, y),
                pos2(content_rect.left() + inset_x + width, y),
            ],
            stroke,
        );
    }

    let cursor_height = (line_step * 0.9).clamp(6.0, 14.0);
    let cursor_width = (5.0 * zoom).clamp(2.0, 5.0);
    painter.rect_filled(
        Rect::from_min_size(
            pos2(content_rect.left() + inset_x, top - cursor_height * 0.65),
            vec2(cursor_width, cursor_height),
        ),
        1.0,
        preview_color(focused, 180),
    );

    if let Some(label) = label.filter(|label| !label.trim().is_empty()) {
        let font_size = (content_rect.height() * 0.12).clamp(10.0, 18.0);
        let max_chars = ((content_rect.width() / (font_size * 0.62)).floor() as usize).max(8);
        let label = truncate_preview_label(label, max_chars);
        let badge_width = (label.chars().count() as f32 * font_size * 0.62 + 22.0)
            .clamp(48.0, (content_rect.width() - 12.0).max(48.0));
        let badge_height = (font_size + 10.0).clamp(18.0, 30.0);
        let badge_rect =
            Rect::from_center_size(content_rect.center(), vec2(badge_width, badge_height));

        painter.rect_filled(
            badge_rect,
            badge_height * 0.5,
            Color32::from_rgba_premultiplied(18, 20, 28, 210),
        );
        painter.rect_stroke(
            badge_rect,
            badge_height * 0.5,
            Stroke::new(1.0, Color32::from_rgba_premultiplied(110, 118, 150, 120)),
        );
        painter.text(
            badge_rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(font_size),
            preview_color(focused, 240),
        );
    }
}

fn draw_cursor(
    painter: &egui::Painter,
    content_rect: Rect,
    display_offset: usize,
    cursor: RenderableCursor,
    metrics: RenderMetrics,
) {
    let Some(cursor_point) = point_to_viewport(display_offset, cursor.point) else {
        return;
    };
    let x = content_rect.left() + metrics.pad_x + cursor_point.column.0 as f32 * metrics.cell_width;
    let y = content_rect.top() + metrics.pad_y + cursor_point.line as f32 * metrics.cell_height;
    let rect = Rect::from_min_size(pos2(x, y), vec2(metrics.cell_width, metrics.cell_height));
    let stroke_width = metrics.zoom.max(1.0);
    let beam_width = (2.0 * metrics.zoom).max(1.0);
    let underline_height = (2.0 * metrics.zoom).max(1.0);

    match cursor.shape {
        VteCursorShape::Block => {
            painter.rect_filled(
                rect,
                0.0,
                Color32::from_rgba_premultiplied(
                    CURSOR_COLOR.r(),
                    CURSOR_COLOR.g(),
                    CURSOR_COLOR.b(),
                    90,
                ),
            );
        }
        VteCursorShape::Beam => {
            painter.rect_filled(
                Rect::from_min_size(rect.min, vec2(beam_width, rect.height())),
                0.0,
                CURSOR_COLOR,
            );
        }
        VteCursorShape::Underline => {
            painter.rect_filled(
                Rect::from_min_size(
                    pos2(rect.left(), rect.bottom() - underline_height),
                    vec2(rect.width(), underline_height),
                ),
                0.0,
                CURSOR_COLOR,
            );
        }
        VteCursorShape::HollowBlock => {
            painter.rect_stroke(rect, 0.0, Stroke::new(stroke_width, CURSOR_COLOR));
        }
        VteCursorShape::Hidden => {}
    }
}

fn preview_color(focused: bool, alpha: u8) -> Color32 {
    let base = if focused {
        Color32::from_rgb(225, 228, 235)
    } else {
        Color32::from_rgb(155, 160, 170)
    };
    Color32::from_rgba_premultiplied(base.r(), base.g(), base.b(), alpha)
}

fn truncate_preview_label(label: &str, max_chars: usize) -> String {
    let count = label.chars().count();
    if count <= max_chars {
        label.to_owned()
    } else {
        let take = max_chars.saturating_sub(1);
        label.chars().take(take).collect::<String>() + "…"
    }
}

fn scaled_metrics(zoom: f32) -> RenderMetrics {
    let zoom = zoom.max(0.01);
    let font_size = FONT_SIZE * zoom;
    RenderMetrics {
        font_size,
        cell_width: font_size * CELL_WIDTH_FACTOR,
        cell_height: font_size * CELL_HEIGHT_FACTOR,
        pad_x: PAD_X * zoom,
        pad_y: PAD_Y * zoom,
        zoom,
    }
}

fn foreground_color(cell: &Cell, colors: &Colors) -> Color32 {
    let mut fg = resolve_color(cell.fg, colors);
    let mut bg = resolve_color(cell.bg, colors);

    if cell.flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }

    fg
}

fn background_color(cell: &Cell, colors: &Colors) -> Color32 {
    let mut fg = resolve_color(cell.fg, colors);
    let mut bg = resolve_color(cell.bg, colors);

    if cell.flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }

    bg
}

fn terminal_background_color(colors: &Colors) -> Color32 {
    named_color(NamedColor::Background, colors)
}

fn resolve_color(color: AnsiColor, colors: &Colors) -> Color32 {
    match color {
        AnsiColor::Named(name) => named_color(name, colors),
        AnsiColor::Spec(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(idx) => indexed_to_egui(idx),
    }
}

fn named_color(name: NamedColor, colors: &Colors) -> Color32 {
    if let Some(rgb) = colors[name] {
        return Color32::from_rgb(rgb.r, rgb.g, rgb.b);
    }

    match name {
        NamedColor::Foreground | NamedColor::BrightForeground => Color32::from_rgb(232, 232, 234),
        NamedColor::Background | NamedColor::DimForeground | NamedColor::DimBlack => {
            Color32::from_rgb(30, 30, 30)
        }
        NamedColor::Cursor => CURSOR_COLOR,
        NamedColor::Black => indexed_to_egui(0),
        NamedColor::Red => indexed_to_egui(1),
        NamedColor::Green => indexed_to_egui(2),
        NamedColor::Yellow => indexed_to_egui(3),
        NamedColor::Blue => indexed_to_egui(4),
        NamedColor::Magenta => indexed_to_egui(5),
        NamedColor::Cyan => indexed_to_egui(6),
        NamedColor::White => indexed_to_egui(7),
        NamedColor::BrightBlack => indexed_to_egui(8),
        NamedColor::BrightRed => indexed_to_egui(9),
        NamedColor::BrightGreen => indexed_to_egui(10),
        NamedColor::BrightYellow => indexed_to_egui(11),
        NamedColor::BrightBlue => indexed_to_egui(12),
        NamedColor::BrightMagenta => indexed_to_egui(13),
        NamedColor::BrightCyan => indexed_to_egui(14),
        NamedColor::BrightWhite => indexed_to_egui(15),
        NamedColor::DimRed => dim_color(indexed_to_egui(1)),
        NamedColor::DimGreen => dim_color(indexed_to_egui(2)),
        NamedColor::DimYellow => dim_color(indexed_to_egui(3)),
        NamedColor::DimBlue => dim_color(indexed_to_egui(4)),
        NamedColor::DimMagenta => dim_color(indexed_to_egui(5)),
        NamedColor::DimCyan => dim_color(indexed_to_egui(6)),
        NamedColor::DimWhite => dim_color(indexed_to_egui(7)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config as TermConfig, Term};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
    use egui::{pos2, vec2, CentralPanel, RawInput, Rect, Rounding};

    use crate::terminal::pty::EventProxy;

    use super::{
        blink_phase_visible, cursor_visible, render_terminal, render_terminal_reduced,
        terminal_background_color, GridCacheKey, TerminalGridCache,
    };

    #[test]
    fn unfocused_cursor_is_hidden() {
        assert!(!cursor_visible(false, false, 0.1));
    }

    #[test]
    fn blinking_cursor_turns_off_during_hidden_phase() {
        assert!(blink_phase_visible(0.2));
        assert!(!blink_phase_visible(0.8));
    }

    #[test]
    fn streaming_output_hides_cursor() {
        assert!(!cursor_visible(true, true, 0.1));
    }

    #[test]
    fn reduced_renderer_keeps_consecutive_rows_visible() {
        let ctx = egui::Context::default();
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(320.0, 240.0))),
            ..Default::default()
        };
        let content_rect = Rect::from_min_size(pos2(20.0, 20.0), vec2(220.0, 120.0));
        let term = sample_term("first\nsecond");

        let output = ctx.run(raw_input, |ctx| {
            CentralPanel::default().show(ctx, |ui| {
                render_terminal_reduced(
                    ui.painter(),
                    content_rect,
                    &term,
                    false,
                    0.0,
                    1.0,
                    Rounding::ZERO,
                    None,
                    0,
                );
            });
        });

        assert!(
            distinct_text_rows(&output.shapes) >= 2,
            "reduced renderer should keep multiple visible rows instead of dropping every other row"
        );
    }

    #[test]
    fn terminal_background_respects_bottom_rounding() {
        let ctx = egui::Context::default();
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(320.0, 240.0))),
            ..Default::default()
        };
        let content_rect = Rect::from_min_size(pos2(20.0, 20.0), vec2(220.0, 120.0));
        let body_rounding = Rounding {
            nw: 0.0,
            ne: 0.0,
            sw: 14.0,
            se: 14.0,
        };
        let term = sample_term("hello");
        let expected_fill = terminal_background_color(term.renderable_content().colors);

        let output = ctx.run(raw_input, |ctx| {
            CentralPanel::default().show(ctx, |ui| {
                render_terminal(
                    ui.painter(),
                    content_rect,
                    &term,
                    false,
                    0.0,
                    1.0,
                    body_rounding,
                    None,
                    0,
                );
            });
        });

        let background = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::epaint::Shape::Rect(rect_shape)
                    if rect_shape.fill == expected_fill && rect_shape.rect == content_rect =>
                {
                    Some(rect_shape)
                }
                _ => None,
            })
            .expect("expected terminal background rect");

        assert_eq!(background.rounding, body_rounding);
    }

    #[test]
    fn grid_cache_key_tracks_revision_and_display_offset() {
        let rect = Rect::from_min_size(pos2(20.0, 20.0), vec2(220.0, 120.0));
        let first = GridCacheKey::new(rect, 1.0, 0, 7, 1);
        let different_revision = GridCacheKey::new(rect, 1.0, 0, 8, 1);
        let different_offset = GridCacheKey::new(rect, 1.0, 1, 7, 1);

        assert_ne!(first, different_revision);
        assert_ne!(first, different_offset);
    }

    #[test]
    fn grid_cache_reuses_shapes_only_for_identical_keys() {
        let rect = Rect::from_min_size(pos2(20.0, 20.0), vec2(220.0, 120.0));
        let key = GridCacheKey::new(rect, 1.0, 0, 7, 1);
        let mut cache = TerminalGridCache::default();

        assert!(!cache.matches(key));
        cache.store(key, Vec::new());
        assert!(cache.matches(key));
        assert!(!cache.matches(GridCacheKey::new(rect, 1.0, 0, 8, 1)));
    }

    fn sample_term(text: &str) -> Term<EventProxy> {
        let (event_tx, _event_rx) = mpsc::channel();
        let mut term = Term::new(
            TermConfig::default(),
            &TermSize::new(24, 8),
            EventProxy::new(event_tx),
        );
        let mut processor = Processor::<StdSyncHandler>::new();
        processor.advance(&mut term, text.as_bytes());
        term
    }

    fn distinct_text_rows(shapes: &[egui::epaint::ClippedShape]) -> usize {
        let mut rows = Vec::new();
        for clipped in shapes {
            if let egui::epaint::Shape::Text(text_shape) = &clipped.shape {
                let y = text_shape.pos.y.round();
                if !rows.iter().any(|row| (row - y).abs() < 0.5) {
                    rows.push(y);
                }
            }
        }
        rows.len()
    }
}
