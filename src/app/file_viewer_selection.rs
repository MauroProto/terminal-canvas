//! Bounded source viewer with one keyboard owner and original-byte selection.

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use egui::{Color32, Event, FontId, Galley, Id, Key, Pos2, Rect, Sense, Vec2};
use unicode_segmentation::UnicodeSegmentation;

use super::code_highlight::HighlightedLine;
#[cfg(test)]
use super::file_viewer_document::OffsetError;
use super::file_viewer_document::{SourceDocument, SourceSelection, FRAGMENT_BYTE_CAP};

const FONT_SIZE: f32 = 12.5;
const ROW_SPACING: f32 = 3.0;
const CODE_INSET: f32 = 10.0;

fn owner_id() -> Id {
    Id::new("file-viewer-source-selection")
}

pub(super) fn viewer_has_keyboard_focus(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.raw.focused) && ctx.memory(|memory| memory.has_focus(owner_id()))
}

/// Call before opening a palette/TextEdit, closing the viewer, or routing focus
/// to a terminal. This never surrenders a different widget's focus.
pub(super) fn release_keyboard_focus(ctx: &egui::Context) {
    ctx.memory_mut(|memory| memory.surrender_focus(owner_id()));
}

pub(super) fn request_keyboard_focus(ctx: &egui::Context) {
    if ctx.input(|input| input.raw.focused) {
        ctx.memory_mut(|memory| memory.request_focus(owner_id()));
    }
}

pub(super) fn register_placeholder_keyboard_owner(ui: &mut egui::Ui, rect: Rect) -> egui::Response {
    if !ui.is_enabled() || !ui.input(|input| input.raw.focused) {
        release_keyboard_focus(ui.ctx());
    }
    let response = ui.interact(rect, owner_id(), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::TextEdit,
            ui.is_enabled(),
            "Visor de código de sólo lectura",
        )
    });
    ui.ctx().accesskit_node_builder(owner_id(), |node| {
        node.set_role(egui::accesskit::Role::MultilineTextInput);
        node.set_read_only();
    });
    ui.ctx().memory_mut(|memory| {
        memory.set_focus_lock_filter(
            owner_id(),
            egui::EventFilter {
                horizontal_arrows: true,
                vertical_arrows: true,
                escape: true,
                ..Default::default()
            },
        )
    });
    response
}

#[derive(Default)]
pub(super) struct SelectionState {
    source: Option<Arc<str>>,
    selection: Option<SourceSelection>,
    active_line: usize,
    caret_fragment: Option<usize>,
    preferred_column: Option<usize>,
    dragging: bool,
    scroll_target: Option<usize>,
    reveal_caret: bool,
    visible: Range<usize>,
}

impl SelectionState {
    fn bind(&mut self, ctx: &egui::Context, document: &SourceDocument) {
        let source = document.source_arc();
        if self
            .source
            .as_ref()
            .is_none_or(|current| !Arc::ptr_eq(current, &source))
        {
            if self.source.is_some() {
                release_keyboard_focus(ctx);
            }
            *self = Self {
                source: Some(source),
                scroll_target: Some(0),
                ..Default::default()
            };
        }
    }

    #[cfg(test)]
    pub(super) fn selection(&self) -> Option<SourceSelection> {
        self.selection
    }

    #[cfg(test)]
    fn set_selection(
        &mut self,
        document: &SourceDocument,
        selection: SourceSelection,
    ) -> Result<(), OffsetError> {
        document.selection_text(selection)?;
        self.source = Some(document.source_arc());
        self.selection = Some(selection);
        self.caret_fragment = fragment_for_offset(document, selection.caret);
        self.active_line = line_for_offset(document, selection.caret).unwrap_or(0);
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) struct ViewerStyle {
    pub(super) background: Color32,
    pub(super) foreground: Color32,
    pub(super) gutter_background: Color32,
    pub(super) gutter_foreground: Color32,
    pub(super) selection: Color32,
}

impl Default for ViewerStyle {
    fn default() -> Self {
        let background = super::code_highlight::theme_background();
        let foreground = super::code_highlight::theme_foreground();
        Self {
            background,
            foreground,
            gutter_background: Color32::from_rgb(
                (background.r() as f32 * 0.82) as u8,
                (background.g() as f32 * 0.82) as u8,
                (background.b() as f32 * 0.82) as u8,
            ),
            gutter_foreground: foreground.gamma_multiply(0.45),
            selection: Color32::from_rgb(58, 78, 106),
        }
    }
}

#[cfg(test)]
pub(super) struct WidgetOutput {
    rendered: Vec<RenderedFragment>,
    pub(super) viewport: Rect,
    copy_selection_button: Rect,
    copy_line_button: Rect,
    copy_file_button: Rect,
    menu_buttons: Vec<(&'static str, Rect)>,
}

#[cfg(test)]
impl Default for WidgetOutput {
    fn default() -> Self {
        Self {
            rendered: Vec::new(),
            viewport: Rect::NOTHING,
            copy_selection_button: Rect::NOTHING,
            copy_line_button: Rect::NOTHING,
            copy_file_button: Rect::NOTHING,
            menu_buttons: Vec::new(),
        }
    }
}

#[cfg(not(test))]
pub(super) struct WidgetOutput;

struct HitRow {
    index: usize,
    rect: Rect,
    position: Pos2,
    galley: Arc<Galley>,
}

#[cfg(test)]
struct RenderedFragment {
    index: usize,
    source: Range<usize>,
    row_rect: Rect,
    text_position: Pos2,
    galley: Arc<Galley>,
    input_bytes: usize,
    glyphs: usize,
    vertices: usize,
    indices: usize,
}

fn line_for_offset(document: &SourceDocument, offset: usize) -> Option<usize> {
    let lines = document.logical_lines();
    (!lines.is_empty()).then(|| {
        lines
            .partition_point(|line| line.content.start <= offset)
            .saturating_sub(1)
    })
}

fn fragment_for_offset(document: &SourceDocument, offset: usize) -> Option<usize> {
    let fragments = document.fragments();
    (!fragments.is_empty()).then(|| {
        fragments
            .partition_point(|fragment| fragment.source.end < offset)
            .min(fragments.len() - 1)
    })
}

fn caret_fragment(
    document: &SourceDocument,
    state: &SelectionState,
    offset: usize,
) -> Option<usize> {
    state
        .caret_fragment
        .filter(|index| {
            document.fragments().get(*index).is_some_and(|fragment| {
                fragment.source.start <= offset && offset <= fragment.source.end
            })
        })
        .or_else(|| fragment_for_offset(document, offset))
}

fn complete_line_text(document: &SourceDocument, index: usize) -> Option<&str> {
    let line = document.logical_lines().get(index)?;
    Some(&document.source()[line.content.start..line.ending.end])
}

fn gutter_width(document: &SourceDocument) -> f32 {
    12.0 + document.logical_lines().len().max(1).to_string().len() as f32 * 7.5
}

fn copy_selection(ctx: &egui::Context, document: &SourceDocument, state: &SelectionState) {
    if let Some(selection) = state.selection {
        if let Ok(text) = document.selection_text(selection) {
            if !text.is_empty() {
                ctx.copy_text(text.to_owned());
            }
        }
    }
}

fn copy_line(ctx: &egui::Context, document: &SourceDocument, state: &SelectionState) {
    if let Some(text) = complete_line_text(document, state.active_line) {
        ctx.copy_text(text.to_owned());
    }
}

fn copy_file(ctx: &egui::Context, document: &SourceDocument) {
    ctx.copy_text(document.source().to_owned());
}

/// Append original slices, splitting only at source offsets. Highlight output
/// is advisory: stale/malformed spans never change the displayed/copied text.
fn fragment_job(
    document: &SourceDocument,
    index: usize,
    state: &SelectionState,
    highlighted: &[HighlightedLine],
    font: &FontId,
    style: ViewerStyle,
) -> egui::text::LayoutJob {
    let fragment = &document.fragments()[index];
    let text = document.fragment_text(index).expect("prepared fragment");
    assert!(
        text.len() <= FRAGMENT_BYTE_CAP,
        "bounded viewer layout input"
    );
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    job.break_on_newline = false;
    let selected = state
        .selection
        .filter(|selection| document.fragment_selection_text(index, *selection).is_ok())
        .map(SourceSelection::normalized);
    let mut append = |range: Range<usize>, color: Color32| {
        let start = selected.as_ref().map_or(range.end, |selected| {
            selected.start.max(range.start).min(range.end)
        });
        let end = selected
            .as_ref()
            .map_or(range.end, |selected| selected.end.max(start).min(range.end));
        for (part, selected) in [
            (range.start..start, false),
            (start..end, true),
            (end..range.end, false),
        ] {
            if !part.is_empty() {
                job.append(
                    &document.source()[part],
                    0.0,
                    egui::TextFormat {
                        font_id: font.clone(),
                        color,
                        background: if selected {
                            style.selection
                        } else {
                            Color32::TRANSPARENT
                        },
                        ..Default::default()
                    },
                );
            }
        }
    };
    let spans = (!document.has_long_lines())
        .then(|| highlighted.get(fragment.logical_line))
        .flatten()
        .filter(|spans| {
            let mut offset = 0;
            for (_, piece) in *spans {
                if !text
                    .get(offset..)
                    .is_some_and(|remaining| remaining.starts_with(piece.as_str()))
                {
                    return false;
                }
                offset += piece.len();
            }
            offset == text.len()
        });
    if let Some(spans) = spans {
        let mut offset = fragment.source.start;
        for (color, piece) in spans {
            let end = offset + piece.len();
            append(offset..end, *color);
            offset = end;
        }
    } else {
        append(fragment.source.clone(), style.foreground);
    }
    if job.text.is_empty() {
        job.append(
            "",
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: style.foreground,
                ..Default::default()
            },
        );
    }
    debug_assert_eq!(job.text, text);
    job
}

/// Grapheme navigation scans at most one prepared fragment. CRLF is crossed
/// as one real line boundary; pathological >cap graphemes use prepared cuts.
fn horizontal_offset(document: &SourceDocument, offset: usize, right: bool) -> usize {
    let Some(line_index) = line_for_offset(document, offset) else {
        return 0;
    };
    let line = &document.logical_lines()[line_index];
    if right && offset >= line.content.end {
        return document
            .logical_lines()
            .get(line_index + 1)
            .map_or(document.source().len(), |line| line.content.start);
    }
    if !right && offset > line.content.end {
        return line.content.end;
    }
    if !right && offset == line.content.start {
        return line_index
            .checked_sub(1)
            .map_or(0, |index| document.logical_lines()[index].content.end);
    }
    let mut index = fragment_for_offset(document, offset).expect("nonempty line has fragment");
    if right && offset == document.fragments()[index].source.end && index + 1 < line.fragments.end {
        index += 1;
    }
    if !right && offset == document.fragments()[index].source.start && index > line.fragments.start
    {
        index -= 1;
    }
    let fragment = &document.fragments()[index];
    let relative = offset
        .saturating_sub(fragment.source.start)
        .min(fragment.source.len());
    let text = document.fragment_text(index).expect("prepared fragment");
    if right {
        offset + text[relative..].graphemes(true).next().map_or(0, str::len)
    } else {
        fragment.source.start
            + text[..relative]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(start, _)| start)
    }
}

fn navigate(
    document: &SourceDocument,
    state: &mut SelectionState,
    key: Key,
    shift: bool,
    command: bool,
) {
    let old = state.selection.unwrap_or(SourceSelection {
        anchor: 0,
        caret: 0,
    });
    let range = old.normalized();
    let mut target_fragment = None;
    let mut column = None;
    let target = match key {
        Key::ArrowLeft | Key::ArrowRight if !shift && range.start != range.end => {
            if key == Key::ArrowLeft {
                range.start
            } else {
                range.end
            }
        }
        Key::ArrowLeft | Key::ArrowRight if !command => {
            horizontal_offset(document, old.caret, key == Key::ArrowRight)
        }
        Key::Home | Key::End | Key::ArrowLeft | Key::ArrowRight => {
            let end = matches!(key, Key::End | Key::ArrowRight);
            if command && matches!(key, Key::Home | Key::End) {
                if end {
                    document.source().len()
                } else {
                    0
                }
            } else {
                line_for_offset(document, old.caret).map_or(0, |index| {
                    let line = &document.logical_lines()[index];
                    if end {
                        line.content.end
                    } else {
                        line.content.start
                    }
                })
            }
        }
        Key::ArrowUp | Key::ArrowDown => {
            if let Some(index) = caret_fragment(document, state, old.caret) {
                let current = &document.fragments()[index];
                let within = old.caret.clamp(current.source.start, current.source.end);
                let preferred = state.preferred_column.unwrap_or_else(|| {
                    document.source()[current.source.start..within]
                        .chars()
                        .count()
                });
                let next = if key == Key::ArrowUp {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(document.fragments().len() - 1)
                };
                let count = document
                    .fragment_text(next)
                    .expect("prepared fragment")
                    .chars()
                    .count();
                target_fragment = Some(next);
                column = Some(preferred);
                let offset = document
                    .fragment_cursor_to_source(next, preferred.min(count))
                    .expect("bounded scalar cursor");
                let fragment = &document.fragments()[next];
                let relative = offset - fragment.source.start;
                let text = document.fragment_text(next).expect("prepared fragment");
                fragment.source.start
                    + text
                        .grapheme_indices(true)
                        .map(|(start, _)| start)
                        .chain(std::iter::once(text.len()))
                        .find(|start| *start >= relative)
                        .expect("fragment has end boundary")
            } else {
                0
            }
        }
        _ => return,
    };
    state.selection = Some(SourceSelection {
        anchor: if shift { old.anchor } else { target },
        caret: target,
    });
    state.caret_fragment = target_fragment.or_else(|| fragment_for_offset(document, target));
    state.preferred_column = column;
    state.active_line = line_for_offset(document, target).unwrap_or(0);
    if let Some(index) = state.caret_fragment {
        if !state.visible.contains(&index) {
            state.scroll_target = Some(index);
        }
    }
    state.reveal_caret = true;
}

fn keyboard_commands(ctx: &egui::Context, document: &SourceDocument, state: &mut SelectionState) {
    if !viewer_has_keyboard_focus(ctx) {
        return;
    }
    let mut actions = Vec::new();
    let mut copy = false;
    ctx.input_mut(|input| {
        input.events.retain(|event| match event {
            Event::Copy => {
                copy = true;
                false
            }
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } if !modifiers.alt => {
                let command = modifiers.command || modifiers.ctrl || modifiers.mac_cmd;
                if command && *key == Key::A {
                    actions.push((*key, false, true));
                    false
                } else if command && *key == Key::C {
                    copy = true;
                    false
                } else if matches!(
                    key,
                    Key::ArrowLeft
                        | Key::ArrowRight
                        | Key::ArrowUp
                        | Key::ArrowDown
                        | Key::Home
                        | Key::End
                ) {
                    actions.push((*key, modifiers.shift, command));
                    false
                } else {
                    true
                }
            }
            _ => true,
        })
    });
    for (key, shift, command) in actions {
        if key == Key::A {
            state.selection = Some(document.select_all());
            state.caret_fragment = fragment_for_offset(document, document.source().len());
            state.preferred_column = None;
        } else {
            navigate(document, state, key, shift, command);
        }
        ctx.request_repaint();
    }
    if copy {
        copy_selection(ctx, document, state);
    }
}

fn pointer_caret(
    document: &SourceDocument,
    state: &mut SelectionState,
    row: &HitRow,
    pointer: Pos2,
    start: bool,
    shift: bool,
) {
    let cursor = row.galley.cursor_from_pos(pointer - row.position);
    let offset = document
        .fragment_cursor_to_source(row.index, cursor.index.0)
        .expect("galley cursor belongs to prepared fragment");
    let anchor = if start && !shift {
        offset
    } else {
        state.selection.map_or(offset, |selection| selection.anchor)
    };
    state.selection = Some(SourceSelection {
        anchor,
        caret: offset,
    });
    state.caret_fragment = Some(row.index);
    state.active_line = document.fragments()[row.index].logical_line;
    state.preferred_column = None;
}

pub(super) fn draw_widget(
    ui: &mut egui::Ui,
    document: &SourceDocument,
    state: &mut SelectionState,
    highlighted: &[HighlightedLine],
    style: ViewerStyle,
) -> WidgetOutput {
    let ctx = ui.ctx().clone();
    let allow_input = ui.is_enabled() && ctx.input(|input| input.raw.focused);
    state.bind(&ctx, document);
    if !allow_input {
        release_keyboard_focus(&ctx);
        state.dragging = false;
    }
    #[cfg(test)]
    let mut output = WidgetOutput::default();
    #[cfg(not(test))]
    let output = WidgetOutput;
    ui.scope(|ui| {
        ui.style_mut().interaction.selectable_labels = false;
        ui.spacing_mut().item_spacing.y = ROW_SPACING;
        ui.horizontal_wrapped(|ui| {
            let selected = state
                .selection
                .is_some_and(|selection| selection.anchor != selection.caret);
            let selection_button = ui.add_enabled(selected, egui::Button::new("Copiar selección"));
            let line_button = ui.add_enabled(
                !document.logical_lines().is_empty(),
                egui::Button::new("Copiar línea"),
            );
            let file_button = ui.add_enabled(
                !document.source().is_empty(),
                egui::Button::new("Copiar archivo"),
            );
            #[cfg(test)]
            {
                output.copy_selection_button = selection_button.rect;
                output.copy_line_button = line_button.rect;
                output.copy_file_button = file_button.rect;
            }
            if allow_input && selection_button.clicked() {
                copy_selection(&ctx, document, state);
                request_keyboard_focus(&ctx);
            }
            if allow_input && line_button.clicked() {
                copy_line(&ctx, document, state);
                request_keyboard_focus(&ctx);
            }
            if allow_input && file_button.clicked() {
                copy_file(&ctx, document);
                request_keyboard_focus(&ctx);
            }
        });
        if document.has_long_lines() {
            ui.label("Líneas extensas: vista en continuaciones, texto íntegro y sin resaltado.");
        }
        let owner = ui.interact(
            ui.available_rect_before_wrap(),
            owner_id(),
            Sense::click_and_drag(),
        );
        owner.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::TextEdit,
                ui.is_enabled(),
                "Visor de código de sólo lectura",
            )
        });
        ctx.accesskit_node_builder(owner_id(), |node| {
            node.set_role(egui::accesskit::Role::MultilineTextInput);
            node.set_read_only();
        });
        ctx.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                owner_id(),
                egui::EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                    ..Default::default()
                },
            )
        });
        if allow_input {
            keyboard_commands(&ctx, document, state);
        }
        let font = FontId::monospace(FONT_SIZE);
        let row_height = ui
            .fonts_mut(|fonts| fonts.row_height(&font))
            .max(ui.spacing().interact_size.y);
        let gutter_width = gutter_width(document);
        let mut scroll = egui::ScrollArea::both()
            .id_salt("code-viewer-source-scroll")
            .auto_shrink([false, false])
            .animated(false)
            .scroll_source(if allow_input { egui::scroll_area::ScrollSource::SCROLL_BAR | egui::scroll_area::ScrollSource::MOUSE_WHEEL } else { egui::scroll_area::ScrollSource::NONE });
        if let Some(index) = state.scroll_target.take() {
            scroll = scroll.vertical_scroll_offset(
                index.min(document.fragments().len().saturating_sub(1)) as f32
                    * (row_height + ROW_SPACING),
            );
        }
        let reveal = std::mem::take(&mut state.reveal_caret);
        let _scrolling =
            scroll.show_rows(ui, row_height, document.fragments().len(), |ui, range| {
                let (pointer, pressed, down, released, shift) = ui.input(|input| {
                    (
                        input.pointer.interact_pos(),
                        input.pointer.primary_pressed(),
                        input.pointer.primary_down(),
                        input.pointer.primary_released(),
                        input.modifiers.shift,
                    )
                });
                let clip = ui.clip_rect();
                let mut rows = Vec::with_capacity(range.len());
                state.visible = 0..0;
                for index in range {
                    let fragment = &document.fragments()[index];
                    let job = fragment_job(document, index, state, highlighted, &font, style);
                    #[cfg(test)]
                    let input_bytes = job.text.len();
                    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                    let width =
                        (gutter_width + CODE_INSET + galley.size().x).max(ui.available_width());
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(width, row_height), Sense::hover());
                    // Accessible visible text is bounded too. Rows describe
                    // text without becoming keyboard owners or copy labels.
                    let row_response = ui.interact(
                        rect,
                        owner_id().with("fragment").with(index),
                        Sense::hover(),
                    );
                    row_response.widget_info(|| {
                        let mut info = egui::WidgetInfo::labeled(
                            egui::WidgetType::Label,
                            ui.is_enabled(),
                            document.fragment_text(index).expect("prepared fragment"),
                        );
                        if fragment.starts_inside_grapheme || fragment.ends_inside_grapheme {
                            info.hint_text = Some("Una secuencia Unicode extensa continúa en otra fila; la copia conserva el original.".to_owned());
                        }
                        info
                    });
                    if fragment.starts_inside_grapheme || fragment.ends_inside_grapheme {
                        row_response.on_hover_text("Una secuencia Unicode extensa continúa en otra fila; la copia conserva el original.");
                    }
                    let position = egui::pos2(
                        rect.left() + gutter_width + CODE_INSET,
                        rect.center().y - galley.size().y / 2.0,
                    );
                    if clip.y_range().contains(rect.center().y) {
                        if state.visible.is_empty() {
                            state.visible = index..index + 1;
                        } else {
                            state.visible.end = index + 1;
                        }
                    }
                    let row = HitRow {
                        index,
                        rect,
                        position,
                        galley: Arc::clone(&galley),
                    };
                    if let Some(pointer) =
                        pointer.filter(|pointer| rect.intersect(clip).contains(*pointer))
                    {
                        if allow_input && owner.secondary_clicked() {
                            state.active_line = fragment.logical_line;
                        }
                        if allow_input && pressed && owner.is_pointer_button_down_on() {
                            pointer_caret(document, state, &row, pointer, true, shift);
                            state.dragging = true;
                            request_keyboard_focus(&ctx);
                            ctx.request_repaint();
                        } else if allow_input && state.dragging && (down || released) {
                            pointer_caret(document, state, &row, pointer, false, shift);
                            ctx.request_repaint();
                        }
                    }
                    ui.painter().rect_filled(rect, 0.0, style.background);
                    ui.painter().rect_filled(
                        Rect::from_min_size(
                            rect.min,
                            Vec2::new(gutter_width, row_height + ROW_SPACING),
                        ),
                        0.0,
                        style.gutter_background,
                    );
                    let first = document.logical_lines()[fragment.logical_line]
                        .fragments
                        .start
                        == index;
                    let gutter = if first {
                        (fragment.logical_line + 1).to_string()
                    } else {
                        "↳".to_owned()
                    };
                    ui.painter().text(
                        egui::pos2(rect.left() + gutter_width - 8.0, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        gutter,
                        font.clone(),
                        style.gutter_foreground,
                    );
                    ui.painter()
                        .galley(position, Arc::clone(&galley), style.foreground);
                    if viewer_has_keyboard_focus(&ctx) {
                        if let Some(selection) = state.selection.filter(|selection| {
                            caret_fragment(document, state, selection.caret) == Some(index)
                        }) {
                            let offset = selection
                                .caret
                                .clamp(fragment.source.start, fragment.source.end);
                            let scalar = document
                                .source_to_fragment_cursor(index, offset)
                                .expect("valid caret offset");
                            let caret = galley
                                .pos_from_cursor(egui::text::CCursor::new(scalar))
                                .translate(position.to_vec2());
                            ui.painter().line_segment(
                                [caret.left_top(), caret.left_bottom()],
                                egui::Stroke::new(1.0, style.foreground),
                            );
                            if reveal {
                                ui.scroll_to_rect(caret, None);
                            }
                        }
                    }
                    #[cfg(test)]
                    output.rendered.push(RenderedFragment {
                        index,
                        source: fragment.source.clone(),
                        row_rect: rect,
                        text_position: position,
                        input_bytes,
                        glyphs: galley.rows.iter().map(|row| row.glyphs.len()).sum(),
                        vertices: galley
                            .rows
                            .iter()
                            .map(|row| row.visuals.mesh.vertices.len())
                            .sum(),
                        indices: galley
                            .rows
                            .iter()
                            .map(|row| row.visuals.mesh.indices.len())
                            .sum(),
                        galley: Arc::clone(&galley),
                    });
                    rows.push(row);
                }
                if allow_input && state.dragging && (down || released) {
                    if let Some(pointer) = pointer.filter(|pointer| !clip.contains(*pointer)) {
                        if let Some(row) = rows.iter().min_by(|left, right| {
                            (left.rect.center().y - pointer.y)
                                .abs()
                                .total_cmp(&(right.rect.center().y - pointer.y).abs())
                        }) {
                            pointer_caret(document, state, row, pointer, false, shift);
                        }
                        if down {
                            let x = if pointer.x < clip.left() {
                                row_height * 4.0
                            } else if pointer.x > clip.right() {
                                -row_height * 4.0
                            } else {
                                0.0
                            };
                            let y = if pointer.y < clip.top() {
                                row_height
                            } else if pointer.y > clip.bottom() {
                                -row_height
                            } else {
                                0.0
                            };
                            ui.scroll_with_delta(Vec2::new(x, y));
                            ctx.request_repaint_after(Duration::from_millis(16));
                        }
                    }
                }
            });
        #[cfg(test)]
        {
            output.viewport = _scrolling.inner_rect;
        }
        if owner.hovered() || state.dragging {
            ctx.set_cursor_icon(egui::CursorIcon::Text);
        }
        owner.context_menu(|ui| {
            let selected = state
                .selection
                .is_some_and(|selection| selection.anchor != selection.caret);
            for (label, enabled) in [
                ("Copiar selección", selected),
                ("Copiar línea", !document.logical_lines().is_empty()),
                ("Copiar archivo", !document.source().is_empty()),
                ("Seleccionar todo", !document.source().is_empty()),
            ] {
                let button = ui.add_enabled(allow_input && enabled, egui::Button::new(label));
                #[cfg(test)]
                output.menu_buttons.push((label, button.rect));
                if allow_input && button.clicked() {
                    match label {
                        "Copiar selección" => copy_selection(&ctx, document, state),
                        "Copiar línea" => copy_line(&ctx, document, state),
                        "Copiar archivo" => copy_file(&ctx, document),
                        _ => {
                            state.selection = Some(document.select_all());
                            ctx.request_repaint();
                        }
                    }
                    request_keyboard_focus(&ctx);
                    ui.close();
                }
            }
        });
        if ctx.input(|input| input.pointer.primary_released()) {
            state.dragging = false;
        }
        if !viewer_has_keyboard_focus(&ctx) && !ctx.input(|input| input.pointer.primary_down()) {
            state.dragging = false;
        }
    });
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{FullOutput, Modifiers, PointerButton, RawInput};

    struct Frame {
        report: WidgetOutput,
        output: FullOutput,
    }

    impl Frame {
        fn copied(&self) -> Vec<&str> {
            self.output
                .platform_output
                .commands
                .iter()
                .filter_map(|command| match command {
                    egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        }

        fn discard(self) {
            self.output.drop_without_applying_deltas();
        }

        fn row(&self, index: usize) -> &RenderedFragment {
            self.report
                .rendered
                .iter()
                .find(|row| {
                    row.index == index
                        && self
                            .report
                            .viewport
                            .y_range()
                            .contains(row.row_rect.center().y)
                        && row.row_rect.intersect(self.report.viewport).width() > 0.0
                })
                .expect("fragment visible")
        }
    }

    struct Harness {
        document: SourceDocument,
        state: SelectionState,
        ctx: egui::Context,
        time: f64,
        enabled: bool,
        focused: bool,
        size: Vec2,
        field: Option<String>,
        take_field_focus: bool,
    }

    impl Harness {
        fn new(source: &str) -> Self {
            Self {
                document: SourceDocument::prepare(Arc::from(source)),
                state: SelectionState::default(),
                ctx: egui::Context::default(),
                time: 0.0,
                enabled: true,
                focused: true,
                size: Vec2::new(620.0, 400.0),
                field: None,
                take_field_focus: false,
            }
        }

        fn frame(&mut self, mut events: Vec<Event>) -> Frame {
            self.time += 0.1;
            let modifiers = events
                .iter()
                .rev()
                .find_map(|event| match event {
                    Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => {
                        Some(*modifiers)
                    }
                    _ => None,
                })
                .unwrap_or(Modifiers::NONE);
            events.insert(0, Event::ModifiersChanged(modifiers));
            let mut report = None;
            let mut output = self.ctx.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
                    events,
                    focused: self.focused,
                    time: Some(self.time),
                    ..Default::default()
                },
                |ui| {
                    if let Some(field) = &mut self.field {
                        let response = ui.add(
                            egui::TextEdit::singleline(field)
                                .id(Id::new("test-viewer-palette-field")),
                        );
                        if self.take_field_focus {
                            response.request_focus();
                        }
                    }
                    report = Some(
                        ui.add_enabled_ui(self.enabled, |ui| {
                            draw_widget(
                                ui,
                                &self.document,
                                &mut self.state,
                                &[],
                                ViewerStyle::default(),
                            )
                        })
                        .inner,
                    );
                },
            );
            self.take_field_focus = false;
            output.textures_delta.clear();
            Frame {
                report: report.expect("widget ran"),
                output,
            }
        }

        fn settled(&mut self) -> Frame {
            self.frame(Vec::new()).discard();
            self.frame(Vec::new())
        }

        fn focus(&mut self) {
            request_keyboard_focus(&self.ctx);
            self.settled().discard();
            assert!(viewer_has_keyboard_focus(&self.ctx));
        }

        fn button(
            &mut self,
            point: Pos2,
            button: PointerButton,
            pressed: bool,
            modifiers: Modifiers,
        ) -> Frame {
            self.frame(vec![
                Event::PointerMoved(point),
                Event::PointerButton {
                    pos: point,
                    button,
                    pressed,
                    modifiers,
                },
            ])
        }

        fn click(&mut self, point: Pos2) -> Frame {
            self.button(point, PointerButton::Primary, true, Modifiers::NONE)
                .discard();
            self.button(point, PointerButton::Primary, false, Modifiers::NONE)
        }

        fn drag(&mut self, start: Pos2, end: Pos2) {
            self.button(start, PointerButton::Primary, true, Modifiers::NONE)
                .discard();
            self.frame(vec![Event::PointerMoved(end)]).discard();
            self.button(end, PointerButton::Primary, false, Modifiers::NONE)
                .discard();
        }
    }

    fn caret_position(row: &RenderedFragment, scalar: usize) -> Pos2 {
        row.text_position
            + row
                .galley
                .pos_from_cursor(egui::text::CCursor::new(scalar))
                .center()
                .to_vec2()
    }

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn drag_across_continuations_copies_original_unicode_without_synthetic_lf() {
        let source = format!("{}cafe\u{301}漢字🙂TAIL", "x".repeat(FRAGMENT_BYTE_CAP - 8));
        let mut harness = Harness::new(&source);
        let frame = harness.settled();
        let start = caret_position(frame.row(0), 2);
        let end_row = frame.row(1);
        let end = caret_position(end_row, end_row.galley.end().index.0);
        let expected_end = end_row.source.end;
        frame.discard();
        harness.drag(start, end);
        assert_eq!(
            harness.state.selection().unwrap().normalized(),
            2..expected_end
        );
        let frame = harness.frame(vec![Event::Copy]);
        assert_eq!(frame.copied(), vec![&source[2..expected_end]]);
        assert!(!frame.copied()[0].contains('\n'));
        frame.discard();
    }

    #[test]
    fn forward_and_reversed_drag_keep_real_crlf_and_exclude_gutter() {
        let source = "cafe\u{301}\r\n漢字🙂tail";
        for reversed in [false, true] {
            let mut harness = Harness::new(source);
            let frame = harness.settled();
            let mut start = caret_position(frame.row(0), 0);
            start.x += 0.25;
            let end_row = frame.row(1);
            let mut end = caret_position(end_row, end_row.galley.end().index.0);
            end.x -= 0.25;
            frame.discard();
            if reversed {
                harness.drag(end, start);
            } else {
                harness.drag(start, end);
            }
            let copied = harness.frame(vec![Event::Copy]);
            assert_eq!(copied.copied(), vec![source]);
            copied.discard();
        }
    }

    #[test]
    fn shift_click_extends_the_original_anchor_across_fragments() {
        let source = format!("{}TAIL", "s".repeat(FRAGMENT_BYTE_CAP));
        let mut harness = Harness::new(&source);
        let frame = harness.settled();
        let start = caret_position(frame.row(0), 3);
        let end_row = frame.row(1);
        let end = caret_position(end_row, end_row.galley.end().index.0);
        frame.discard();
        harness.click(start).discard();
        harness
            .button(end, PointerButton::Primary, true, Modifiers::SHIFT)
            .discard();
        harness
            .button(end, PointerButton::Primary, false, Modifiers::SHIFT)
            .discard();
        let copied = harness.frame(vec![Event::Copy]);
        assert_eq!(copied.copied(), vec![&source[3..]]);
        copied.discard();
    }

    #[test]
    fn dragging_below_viewport_scrolls_without_losing_original_anchor() {
        let source = "cafe\u{301} 漢字🙂\r\n".repeat(1000);
        let mut harness = Harness::new(&source);
        let frame = harness.settled();
        let start = caret_position(frame.row(0), 2);
        let below = egui::pos2(start.x, frame.report.viewport.bottom() + 40.0);
        frame.discard();
        harness
            .button(start, PointerButton::Primary, true, Modifiers::NONE)
            .discard();
        for _ in 0..12 {
            harness.frame(vec![Event::PointerMoved(below)]).discard();
        }
        harness
            .button(below, PointerButton::Primary, false, Modifiers::NONE)
            .discard();
        let selection = harness.state.selection().expect("drag selection retained");
        assert_eq!(selection.anchor, 2);
        assert!(selection.caret > 2);
        let frame = harness.settled();
        assert!(frame.report.rendered.first().unwrap().index > 0);
        assert!(frame.report.rendered.len() < 64);
        frame.discard();
        let copied = harness.frame(vec![Event::Copy]);
        assert_eq!(copied.copied(), vec![&source[selection.normalized()]]);
        copied.discard();
    }

    #[test]
    fn command_a_and_copy_include_offscreen_source_and_final_terminator_once() {
        let source = format!("{}\r\nlast\n", "a".repeat(2 * 1024 * 1024 - 7));
        let mut harness = Harness::new(&source);
        harness.settled().discard();
        harness.focus();
        let frame = harness.frame(vec![
            key(Key::A, Modifiers::COMMAND),
            key(Key::C, Modifiers::COMMAND),
            Event::Copy,
        ]);
        assert_eq!(frame.copied(), vec![source.as_str()]);
        assert_eq!(
            harness.state.selection(),
            Some(harness.document.select_all())
        );
        assert!(frame.report.rendered.len() < harness.document.fragments().len());
        assert!(frame
            .report
            .rendered
            .iter()
            .all(|row| row.input_bytes <= FRAGMENT_BYTE_CAP
                && row.galley.job.text.len() <= FRAGMENT_BYTE_CAP));
        frame.discard();
    }

    #[test]
    fn copy_toolbar_keeps_complete_line_file_and_current_selection() {
        let source = format!("{}\r\nFINAL", "q".repeat(3 * FRAGMENT_BYTE_CAP));
        let mut harness = Harness::new(&source);
        let frame = harness.settled();
        let line = frame.report.copy_line_button.center();
        let file = frame.report.copy_file_button.center();
        frame.discard();
        let copied = harness.click(line);
        assert_eq!(
            copied.copied(),
            vec![&source[..source.len() - "FINAL".len()]]
        );
        copied.discard();
        let copied = harness.click(file);
        assert_eq!(copied.copied(), vec![source.as_str()]);
        copied.discard();
        harness
            .state
            .set_selection(
                &harness.document,
                SourceSelection {
                    anchor: 2,
                    caret: 8,
                },
            )
            .unwrap();
        let frame = harness.settled();
        let selection = frame.report.copy_selection_button.center();
        frame.discard();
        let copied = harness.click(selection);
        assert_eq!(copied.copied(), vec![&source[2..8]]);
        copied.discard();
    }

    #[test]
    fn context_menu_copies_the_pointed_logical_line_and_full_file() {
        let source = "first\r\n漢字🙂last\n";
        let mut harness = Harness::new(source);
        let frame = harness.settled();
        let point = caret_position(frame.row(1), 1);
        frame.discard();
        for (label, expected) in [("Copiar línea", "漢字🙂last\n"), ("Copiar archivo", source)]
        {
            harness
                .button(point, PointerButton::Secondary, true, Modifiers::NONE)
                .discard();
            harness
                .button(point, PointerButton::Secondary, false, Modifiers::NONE)
                .discard();
            let frame = harness.settled();
            let button = frame
                .report
                .menu_buttons
                .iter()
                .find(|(text, _)| *text == label)
                .expect("menu opened")
                .1
                .center();
            frame.discard();
            let copied = harness.click(button);
            assert_eq!(copied.copied(), vec![expected]);
            copied.discard();
        }
    }

    #[test]
    fn two_mib_layout_is_virtualized_at_eof_and_selection_survives_scroll() {
        let source = "k".repeat(2 * 1024 * 1024);
        let mut harness = Harness::new(&source);
        harness.settled().discard();
        let selection = SourceSelection {
            anchor: 10,
            caret: source.len(),
        };
        harness
            .state
            .set_selection(&harness.document, selection)
            .unwrap();
        harness.focus();
        harness.state.scroll_target = Some(harness.document.fragments().len() - 1);
        let frame = harness.settled();
        assert!(frame.report.rendered.len() < 64);
        assert!(frame
            .report
            .rendered
            .iter()
            .any(|row| row.source.end == source.len()));
        assert_eq!(harness.state.selection(), Some(selection));
        for row in &frame.report.rendered {
            assert!(row.input_bytes <= FRAGMENT_BYTE_CAP);
            assert!(row.galley.job.text.len() <= FRAGMENT_BYTE_CAP);
            assert!(row.glyphs <= FRAGMENT_BYTE_CAP);
            assert!(row.vertices < 16 * FRAGMENT_BYTE_CAP);
            assert!(row.indices < 24 * FRAGMENT_BYTE_CAP);
        }
        assert!(frame.output.shapes.iter().all(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => text.galley.job.text.len() <= FRAGMENT_BYTE_CAP,
            _ => true,
        }));
        frame.discard();
        let copied = harness.frame(vec![Event::Copy]);
        assert_eq!(copied.copied(), vec![&source[10..]]);
        copied.discard();
    }

    #[test]
    fn pathological_grapheme_layout_is_bounded_and_copy_is_lossless() {
        let source = format!("a{}\r\n", "\u{301}".repeat(4 * FRAGMENT_BYTE_CAP));
        let mut harness = Harness::new(&source);
        let frame = harness.settled();
        assert!(harness
            .document
            .fragments()
            .iter()
            .any(|fragment| fragment.starts_inside_grapheme));
        assert!(frame
            .report
            .rendered
            .iter()
            .all(|row| row.input_bytes <= FRAGMENT_BYTE_CAP));
        frame.discard();
        harness.focus();
        let copied = harness.frame(vec![key(Key::A, Modifiers::COMMAND), Event::Copy]);
        assert_eq!(copied.copied(), vec![source.as_str()]);
        copied.discard();
    }

    #[test]
    fn shift_arrows_use_graphemes_and_home_end_preserve_crlf() {
        let source = "cafe\u{301}🙂\r\n漢字TAIL";
        let mut harness = Harness::new(source);
        harness.settled().discard();
        harness
            .state
            .set_selection(
                &harness.document,
                SourceSelection {
                    anchor: 3,
                    caret: 3,
                },
            )
            .unwrap();
        harness.focus();
        harness
            .frame(vec![key(Key::ArrowRight, Modifiers::SHIFT)])
            .discard();
        assert_eq!(harness.state.selection().unwrap().normalized(), 3..6);
        harness
            .frame(vec![key(Key::ArrowRight, Modifiers::SHIFT)])
            .discard();
        assert_eq!(harness.state.selection().unwrap().normalized(), 3..10);
        harness
            .frame(vec![key(Key::ArrowRight, Modifiers::SHIFT)])
            .discard();
        assert_eq!(harness.state.selection().unwrap().normalized(), 3..12);
        harness
            .frame(vec![key(Key::End, Modifiers::SHIFT)])
            .discard();
        let copied = harness.frame(vec![Event::Copy]);
        assert_eq!(copied.copied(), vec![&source[3..]]);
        copied.discard();
        harness
            .frame(vec![key(Key::Home, Modifiers::NONE)])
            .discard();
        assert_eq!(harness.state.selection().unwrap().caret, 12);
        harness
            .frame(vec![key(Key::Home, Modifiers::COMMAND)])
            .discard();
        assert_eq!(harness.state.selection().unwrap().caret, 0);
        harness
            .frame(vec![key(Key::End, Modifiers::COMMAND)])
            .discard();
        assert_eq!(harness.state.selection().unwrap().caret, source.len());
    }

    #[test]
    fn vertical_shift_navigation_keeps_anchor_column_and_source_across_empty_line() {
        let source = "abcdef\r\n\r\n漢字🙂abcd\n";
        let mut harness = Harness::new(source);
        harness.settled().discard();
        harness
            .state
            .set_selection(
                &harness.document,
                SourceSelection {
                    anchor: 2,
                    caret: 2,
                },
            )
            .unwrap();
        harness.focus();
        harness
            .frame(vec![key(Key::ArrowDown, Modifiers::SHIFT)])
            .discard();
        assert_eq!(
            harness.state.selection().unwrap(),
            SourceSelection {
                anchor: 2,
                caret: 8
            }
        );
        harness
            .frame(vec![key(Key::ArrowDown, Modifiers::SHIFT)])
            .discard();
        assert_eq!(
            harness.state.selection().unwrap(),
            SourceSelection {
                anchor: 2,
                caret: 16
            }
        );
        let copied = harness.frame(vec![Event::Copy]);
        assert_eq!(copied.copied(), vec![&source[2..16]]);
        copied.discard();
        harness
            .frame(vec![key(Key::ArrowUp, Modifiers::SHIFT)])
            .discard();
        harness
            .frame(vec![key(Key::ArrowUp, Modifiers::SHIFT)])
            .discard();
        assert_eq!(
            harness.state.selection().unwrap(),
            SourceSelection {
                anchor: 2,
                caret: 2
            }
        );
    }

    #[test]
    fn vertical_navigation_does_not_stop_inside_a_normal_combining_cluster() {
        let mut harness = Harness::new("abcd\naé\u{301}z");
        harness.settled().discard();
        harness
            .state
            .set_selection(
                &harness.document,
                SourceSelection {
                    anchor: 2,
                    caret: 2,
                },
            )
            .unwrap();
        harness.focus();
        harness
            .frame(vec![key(Key::ArrowDown, Modifiers::SHIFT)])
            .discard();
        // Scalar column 2 falls between é and its combining acute: snap to
        // the complete grapheme boundary, not an interior source byte offset.
        assert_eq!(
            harness.state.selection().unwrap().caret,
            "abcd\naé\u{301}".len()
        );
    }

    #[test]
    fn keyboard_takeover_by_textedit_does_not_copy_viewer_or_steal_command_a() {
        let mut harness = Harness::new("viewer source\r\n");
        harness.settled().discard();
        harness
            .state
            .set_selection(&harness.document, harness.document.select_all())
            .unwrap();
        harness.focus();
        harness.field = Some("palette text".to_owned());
        harness.take_field_focus = true;
        harness.settled().discard();
        assert_eq!(
            harness.ctx.memory(|memory| memory.focused()),
            Some(Id::new("test-viewer-palette-field"))
        );
        assert!(!viewer_has_keyboard_focus(&harness.ctx));
        let frame = harness.frame(vec![key(Key::A, Modifiers::COMMAND), Event::Copy]);
        assert_eq!(frame.copied(), vec!["palette text"]);
        assert_eq!(
            harness.state.selection(),
            Some(harness.document.select_all())
        );
        frame.discard();
        release_keyboard_focus(&harness.ctx);
        assert_eq!(
            harness.ctx.memory(|memory| memory.focused()),
            Some(Id::new("test-viewer-palette-field"))
        );
    }

    #[test]
    fn palette_release_and_window_blur_do_not_reacquire_viewer_focus() {
        let mut harness = Harness::new("source\n");
        harness.settled().discard();
        harness.focus();
        release_keyboard_focus(&harness.ctx);
        let frame = harness.frame(vec![key(Key::A, Modifiers::COMMAND), Event::Copy]);
        assert!(frame.copied().is_empty());
        assert!(harness.state.selection().is_none());
        assert!(harness.ctx.input(|input| input
            .events
            .iter()
            .any(|event| matches!(event, Event::Copy))));
        frame.discard();
        harness.focus();
        harness.focused = false;
        let frame = harness.frame(vec![key(Key::A, Modifiers::COMMAND), Event::Copy]);
        assert!(frame.copied().is_empty());
        assert!(!viewer_has_keyboard_focus(&harness.ctx));
        assert_ne!(
            harness.ctx.memory(|memory| memory.focused()),
            Some(owner_id())
        );
        frame.discard();
        harness.focused = true;
        harness.settled().discard();
        assert!(!viewer_has_keyboard_focus(&harness.ctx));
    }

    #[test]
    fn disabled_widget_ignores_manual_keys_pointer_and_copy_toolbar() {
        let mut harness = Harness::new("source\r\n");
        harness.settled().discard();
        harness.focus();
        harness.enabled = false;
        let frame = harness.frame(vec![key(Key::A, Modifiers::COMMAND), Event::Copy]);
        assert!(frame.copied().is_empty());
        assert!(harness.state.selection().is_none());
        assert!(!viewer_has_keyboard_focus(&harness.ctx));
        let point = frame.report.copy_file_button.center();
        frame.discard();
        let copied = harness.click(point);
        assert!(copied.copied().is_empty());
        copied.discard();
    }

    #[test]
    fn replacement_document_resets_selection_focus_and_scroll() {
        let mut harness = Harness::new(&"s".repeat(32 * FRAGMENT_BYTE_CAP));
        harness.settled().discard();
        harness
            .state
            .set_selection(&harness.document, harness.document.select_all())
            .unwrap();
        harness.focus();
        harness.state.scroll_target = Some(31);
        harness.settled().discard();
        harness.document = SourceDocument::prepare(Arc::from("new\r\n"));
        let frame = harness.settled();
        assert!(harness.state.selection().is_none());
        assert!(!viewer_has_keyboard_focus(&harness.ctx));
        assert_eq!(frame.row(0).source, 0..3);
        frame.discard();
    }

    #[test]
    fn resizing_keeps_viewport_inside_bounds_and_selection_unchanged() {
        let mut harness = Harness::new(&format!("{}\nnext", "a".repeat(8 * FRAGMENT_BYTE_CAP)));
        harness.settled().discard();
        let selection = SourceSelection {
            anchor: 1,
            caret: 2000,
        };
        harness
            .state
            .set_selection(&harness.document, selection)
            .unwrap();
        for width in [320.0, 1200.0, 620.0] {
            harness.size = Vec2::new(width, 400.0);
            let frame = harness.settled();
            assert!(
                Rect::from_min_size(Pos2::ZERO, harness.size).contains_rect(frame.report.viewport)
            );
            assert_eq!(harness.state.selection(), Some(selection));
            assert!(frame
                .report
                .rendered
                .iter()
                .all(|row| row.input_bytes <= FRAGMENT_BYTE_CAP));
            frame.discard();
        }
    }

    #[test]
    fn visible_rows_have_bounded_accessible_text_without_taking_focus() {
        let source = "a".repeat(2 * 1024 * 1024);
        let mut harness = Harness::new(&source);
        harness.ctx.enable_accesskit();
        let frame = harness.settled();
        let tree = frame
            .output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("accesskit enabled");
        let owner = tree
            .nodes
            .iter()
            .find(|(id, _)| *id == owner_id().accesskit_id())
            .expect("stable owner node");
        assert_eq!(owner.1.role(), egui::accesskit::Role::MultilineTextInput);
        assert!(owner.1.is_read_only());
        for row in &frame.report.rendered {
            let id = owner_id().with("fragment").with(row.index).accesskit_id();
            let node = &tree
                .nodes
                .iter()
                .find(|(node_id, _)| *node_id == id)
                .expect("visible row metadata")
                .1;
            assert_eq!(node.role(), egui::accesskit::Role::Label);
            assert_eq!(node.value(), harness.document.fragment_text(row.index));
            assert!(node.value().unwrap().len() <= FRAGMENT_BYTE_CAP);
            assert!(!node.supports_action(egui::accesskit::Action::Focus));
        }
        assert!(!viewer_has_keyboard_focus(&harness.ctx));
        frame.discard();
    }

    #[test]
    fn layout_keeps_nowrap_indentation_and_exact_highlight_colors() {
        let document = SourceDocument::prepare(Arc::from("    let café = 42;\r\n"));
        let colors = [Color32::RED, Color32::GREEN, Color32::BLUE];
        let highlighted = vec![vec![
            (colors[0], "    let ".to_owned()),
            (colors[1], "café".to_owned()),
            (colors[2], " = 42;".to_owned()),
        ]];
        let font = FontId::monospace(FONT_SIZE);
        let job = fragment_job(
            &document,
            0,
            &SelectionState::default(),
            &highlighted,
            &font,
            ViewerStyle::default(),
        );
        assert_eq!(job.text, "    let café = 42;");
        assert_eq!(job.wrap.max_width, f32::INFINITY);
        assert!(!job.break_on_newline);
        assert_eq!(
            job.sections
                .iter()
                .map(|section| section.format.color)
                .collect::<Vec<_>>(),
            colors
        );
        assert_eq!(
            job.sections
                .iter()
                .map(|section| &job.text[section.byte_range.start.0..section.byte_range.end.0])
                .collect::<Vec<_>>(),
            ["    let ", "café", " = 42;"]
        );
    }

    #[test]
    fn empty_missing_or_stale_highlights_fall_back_to_original_plain_text() {
        let document = SourceDocument::prepare(Arc::from("  source\nsecond\r\n"));
        let style = ViewerStyle::default();
        let font = FontId::monospace(FONT_SIZE);
        for highlighted in [
            vec![],
            vec![vec![]],
            vec![vec![(Color32::RED, "wrong source".to_owned())]],
        ] {
            for index in 0..document.fragments().len() {
                let job = fragment_job(
                    &document,
                    index,
                    &SelectionState::default(),
                    &highlighted,
                    &font,
                    style,
                );
                assert_eq!(job.text, document.fragment_text(index).unwrap());
                assert!(job
                    .sections
                    .iter()
                    .all(|section| section.format.color == style.foreground));
            }
        }
        // Source rows past the highlighter's cap remain visible and copyable.
        let capped = SourceDocument::prepare(Arc::from(format!(
            "{}TAIL",
            "line\n".repeat(super::super::code_highlight::MAX_HIGHLIGHT_LINES)
        )));
        let job = fragment_job(
            &capped,
            super::super::code_highlight::MAX_HIGHLIGHT_LINES,
            &SelectionState::default(),
            &[],
            &font,
            style,
        );
        assert_eq!(job.text, "TAIL");
    }

    #[test]
    fn long_line_document_is_plain_even_if_short_line_highlights_exist() {
        let document = SourceDocument::prepare(Arc::from(format!(
            "short\n{}",
            "x".repeat(FRAGMENT_BYTE_CAP + 1)
        )));
        let style = ViewerStyle::default();
        let job = fragment_job(
            &document,
            0,
            &SelectionState::default(),
            &[vec![(Color32::RED, "short".to_owned())]],
            &FontId::monospace(FONT_SIZE),
            style,
        );
        assert!(job
            .sections
            .iter()
            .all(|section| section.format.color == style.foreground));
        assert_eq!(job.text, "short");
    }

    #[test]
    fn gutter_grows_with_five_digit_logical_line_count() {
        let small = SourceDocument::prepare(Arc::from("line"));
        let large = SourceDocument::prepare(Arc::from("line\n".repeat(10_000)));
        assert!(gutter_width(&large) > gutter_width(&small));
        assert_eq!(gutter_width(&large) - gutter_width(&small), 4.0 * 7.5);
    }
}
