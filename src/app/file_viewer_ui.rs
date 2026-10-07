//! Visor de código dockeado a la derecha del canvas (como el editor de Orca):
//! panel redimensionable, con gutter de números de línea y resaltado de
//! sintaxis real vía `syntect`.
//!
//! Dos decisiones de fluidez:
//! - La lectura y el coloreado corren en workers separados, así ni un volumen
//!   lento ni syntect pueden trabar el frame de egui.
//! - La lista se dibuja virtualizada (`show_rows`): sólo se pintan las líneas
//!   visibles, así un archivo de 20k líneas cuesta lo mismo que uno de 50.

#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use egui::{vec2, Color32, RichText};

use crate::theme::colors as palette;

use super::code_highlight::HighlightedLine;
use super::file_viewer_document::SourceDocument;
use super::file_viewer_reader::{FileContents, FileViewerReader};
use super::file_viewer_selection::{self, SelectionState, ViewerStyle};
use super::TerminalApp;

#[cfg(test)]
use super::file_viewer_reader::{MAX_VIEW_BYTES, MAX_VIEW_LINES};
const DEFAULT_WIDTH: f32 = 620.0;
const MIN_WIDTH: f32 = 320.0;
const MAX_WIDTH: f32 = 1200.0;

/// El fondo y el color base salen del tema de syntect, no de la paleta de la
/// app: los colores de los tokens están elegidos para ese fondo.
fn code_bg() -> Color32 {
    super::code_highlight::theme_background()
}

fn plain_fg() -> Color32 {
    super::code_highlight::theme_foreground()
}

/// Gutter: mismo tono que el código pero apenas más oscuro, y número atenuado.
fn gutter_bg() -> Color32 {
    let bg = code_bg();
    Color32::from_rgb(
        (bg.r() as f32 * 0.82) as u8,
        (bg.g() as f32 * 0.82) as u8,
        (bg.b() as f32 * 0.82) as u8,
    )
}

fn gutter_fg() -> Color32 {
    plain_fg().gamma_multiply(0.45)
}

pub(super) struct FileViewerState {
    pub(super) path: PathBuf,
    pub(super) document: Option<Arc<SourceDocument>>,
    pub(super) selection: SelectionState,
    pub(super) read_error: Option<String>,
    pub(super) truncated: bool,
    pub(super) binary: bool,
    /// Líneas ya coloreadas; vacío mientras el worker trabaja.
    pub(super) highlighted: Vec<HighlightedLine>,
    /// Token del pedido en curso, para descartar resultados viejos.
    pub(super) highlight_token: Option<u64>,
    pub(super) language: Option<String>,
    pub(super) loading: bool,
}

impl FileViewerState {
    fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

impl TerminalApp {
    pub(super) fn open_file_viewer(&mut self, path: PathBuf) {
        self.highlighter.cancel();
        if let Some(ctx) = &self.ctx {
            file_viewer_selection::release_keyboard_focus(ctx);
        }
        let reader = self
            .file_viewer_reader
            .get_or_insert_with(|| FileViewerReader::new(super::code_highlight::detect_language));
        let repaint = self.ctx.clone();
        reader.request(path.clone(), move || {
            if let Some(ctx) = repaint {
                ctx.request_repaint();
            }
        });
        let available = reader.is_available();
        self.file_viewer = Some(loading_file_state(path));
        if !available {
            if let Some(viewer) = self.file_viewer.as_mut() {
                viewer.loading = false;
                viewer.read_error = Some("(no se pudo iniciar la lectura del archivo)".to_owned());
            }
        }
    }

    fn activate_loaded_file(&mut self, mut state: FileViewerState, source: Option<Arc<str>>) {
        if let Some(source) = source.filter(|_| {
            !state.binary
                && state.document.as_ref().is_some_and(|document| {
                    !document.logical_lines().is_empty() && !document.has_long_lines()
                })
                && self.highlighter.is_available()
        }) {
            let name = state.file_name();
            // Share the prepared source; no full-file clone or line scan on UI.
            let repaint = self.ctx.clone();
            state.highlight_token =
                Some(self.highlighter.request_with_notify(name, source, move || {
                    if let Some(ctx) = repaint {
                        ctx.request_repaint();
                    }
                }));
        }
        self.file_viewer = Some(state);
    }

    fn poll_file_viewer_load(&mut self, ctx: &egui::Context) {
        let result = self
            .file_viewer_reader
            .as_mut()
            .and_then(FileViewerReader::poll);
        match result {
            Some(result) => {
                let current_generation = self
                    .file_viewer_reader
                    .as_ref()
                    .is_some_and(|reader| reader.is_current(result.token));
                let still_expected = current_generation
                    && self
                        .file_viewer
                        .as_ref()
                        .is_some_and(|viewer| viewer.loading && viewer.path == result.path);
                if still_expected {
                    let (state, source) = loaded_file_state(result.path, result.contents);
                    self.activate_loaded_file(state, source);
                }
            }
            None => {
                if let Some(viewer) = self.file_viewer.as_mut().filter(|viewer| viewer.loading) {
                    if self
                        .file_viewer_reader
                        .as_ref()
                        .is_some_and(FileViewerReader::is_available)
                    {
                        ctx.request_repaint_after(std::time::Duration::from_millis(50));
                    } else {
                        viewer.loading = false;
                        viewer.read_error = Some("(no se pudo leer el archivo)".to_owned());
                    }
                }
            }
        }
    }

    /// Recoge el resultado del worker de resaltado, si llegó.
    pub(super) fn poll_highlighter(&mut self) {
        while let Some(result) = self.highlighter.poll() {
            let Some(viewer) = self.file_viewer.as_mut() else {
                continue;
            };
            // Descartamos lo que corresponda a un archivo ya cerrado o cambiado.
            if viewer.highlight_token == Some(result.token) {
                viewer.highlighted = result.lines;
                viewer.highlight_token = None;
            }
        }
        if !self.highlighter.is_available() {
            if let Some(viewer) = self.file_viewer.as_mut() {
                viewer.highlight_token = None;
            }
        }
    }

    pub(super) fn poll_file_viewer_updates(&mut self, ctx: &egui::Context) {
        self.poll_file_viewer_load(ctx);
        self.poll_highlighter();
    }

    pub(super) fn show_file_viewer(&mut self, root_ui: &mut egui::Ui) {
        let ctx = root_ui.ctx().clone();
        self.ctx = Some(ctx.clone());
        if self.file_viewer.is_none() {
            return;
        }
        let can_focus = !self.modal_input_is_active() && ctx.input(|input| input.raw.focused);
        if !can_focus {
            file_viewer_selection::release_keyboard_focus(&ctx);
        }
        if file_viewer_selection::viewer_has_keyboard_focus(&ctx)
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.highlighter.cancel();
            if let Some(reader) = self.file_viewer_reader.as_mut() {
                reader.cancel();
            }
            self.file_viewer = None;
            file_viewer_selection::release_keyboard_focus(&ctx);
            return;
        }

        let mut close = false;
        let mut open_external: Option<PathBuf> = None;
        let mut open_dropped: Option<PathBuf> = None;

        egui::Panel::right("code-viewer")
            .resizable(true)
            .show_separator_line(false)
            .default_size(DEFAULT_WIDTH)
            .size_range(MIN_WIDTH..=MAX_WIDTH)
            .frame(
                egui::Frame::NONE
                    .fill(code_bg())
                    .inner_margin(egui::Margin::same(0)),
            )
            .show(root_ui, |ui| {
                let Some(viewer) = self.file_viewer.as_mut() else {
                    return;
                };
                ui.style_mut().interaction.selectable_labels = false;
                // Borde izquierdo que separa el código del canvas.
                let panel_rect = ui.max_rect();
                if let Some(pointer) = ctx.input(|input| {
                    input
                        .pointer
                        .primary_pressed()
                        .then(|| input.pointer.interact_pos())
                        .flatten()
                }) {
                    if can_focus && panel_rect.contains(pointer) {
                        file_viewer_selection::request_keyboard_focus(&ctx);
                    } else {
                        file_viewer_selection::release_keyboard_focus(&ctx);
                    }
                }

                // Soltar un archivo sobre el visor lo abre (si el puntero
                // está dentro de la región del panel).
                let (dropped, pointer) = ui.ctx().input(|input| {
                    (
                        input
                            .raw
                            .dropped_files
                            .first()
                            .map(|file| file.path().to_path_buf())
                            .filter(|path| !path.as_os_str().is_empty()),
                        input.pointer.hover_pos(),
                    )
                });
                if let (Some(path), Some(pointer)) = (dropped, pointer) {
                    if panel_rect.contains(pointer) {
                        open_dropped = Some(path);
                    }
                }
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(panel_rect.min, vec2(1.0, panel_rect.height())),
                    0.0,
                    palette::LINE,
                );

                draw_header(ui, viewer, &mut close, &mut open_external);
                ui.separator();

                if viewer.binary
                    || viewer.loading
                    || viewer.read_error.is_some()
                    || viewer.document.is_none()
                {
                    ui.scope(|ui| {
                        if !can_focus {
                            ui.disable();
                        }
                        let rect = ui.available_rect_before_wrap();
                        file_viewer_selection::register_placeholder_keyboard_owner(ui, rect);
                    });
                }

                if viewer.binary {
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new("Archivo binario: no se puede mostrar como texto")
                            .size(11.0)
                            .color(palette::DIM),
                    );
                    return;
                }

                if viewer.loading {
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new("Leyendo archivo…")
                            .size(11.0)
                            .color(palette::DIM),
                    );
                    return;
                }

                if let Some(message) = &viewer.read_error {
                    ui.add_space(16.0);
                    ui.label(RichText::new(message).size(11.0).color(palette::DIM));
                    return;
                }

                if !can_focus {
                    ui.disable();
                }
                draw_code(ui, viewer);
            });

        if close {
            self.highlighter.cancel();
            if let Some(reader) = self.file_viewer_reader.as_mut() {
                reader.cancel();
            }
            self.file_viewer = None;
            file_viewer_selection::release_keyboard_focus(&ctx);
        }
        if let Some(path) = open_dropped {
            self.open_file_viewer(path);
        }
        if let Some(path) = open_external {
            if let Err(err) = crate::utils::platform::open_path_external(&path) {
                self.toast_error(format!("No se pudo abrir en el editor: {err}"));
            }
        }
    }
}

fn loading_file_state(path: PathBuf) -> FileViewerState {
    FileViewerState {
        path,
        document: None,
        selection: SelectionState::default(),
        read_error: None,
        truncated: false,
        binary: false,
        highlighted: Vec::new(),
        highlight_token: None,
        language: None,
        loading: true,
    }
}

fn draw_header(
    ui: &mut egui::Ui,
    viewer: &FileViewerState,
    close: &mut bool,
    open_external: &mut Option<PathBuf>,
) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(
            RichText::new(viewer.file_name())
                .size(12.5)
                .color(palette::TEXT_STRONG),
        );
        if let Some(language) = &viewer.language {
            ui.label(RichText::new(language).size(10.0).color(palette::DIM));
        }
        // Los botones van pegados a la derecha.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            if ui.small_button("✕").on_hover_text("Cerrar (Esc)").clicked() {
                *close = true;
            }
            if ui
                .small_button("↗")
                .on_hover_text("Abrir en el editor externo")
                .clicked()
            {
                *open_external = Some(viewer.path.clone());
            }
        });
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        let line_count = viewer
            .document
            .as_ref()
            .map_or(0, |document| document.logical_lines().len());
        let mut status = format!("{line_count} líneas");
        if viewer.truncated {
            status.push_str(" · truncado por límite seguro");
        }
        if viewer.highlight_token.is_some() {
            status.push_str(" · coloreando…");
        }
        if viewer
            .document
            .as_ref()
            .is_some_and(|document| document.has_long_lines())
        {
            status.push_str(" · líneas extensas en texto plano");
        }
        ui.label(RichText::new(status).size(10.0).color(palette::DIM));
    });
    ui.add_space(6.0);
}

fn draw_code(ui: &mut egui::Ui, viewer: &mut FileViewerState) {
    let Some(document) = &viewer.document else {
        return;
    };
    let style = ViewerStyle {
        foreground: plain_fg(),
        background: code_bg(),
        gutter_background: gutter_bg(),
        gutter_foreground: gutter_fg(),
        selection: ui.visuals().selection.bg_fill,
    };
    file_viewer_selection::draw_widget(
        ui,
        document,
        &mut viewer.selection,
        &viewer.highlighted,
        style,
    );
}

fn loaded_file_state(path: PathBuf, contents: FileContents) -> (FileViewerState, Option<Arc<str>>) {
    let mut state = loading_file_state(path);
    state.loading = false;
    let source = match contents {
        FileContents::Text {
            document,
            truncated,
            language,
        } => {
            let source = document.source_arc();
            state.document = Some(document);
            state.truncated = truncated;
            state.language = language;
            Some(source)
        }
        FileContents::Binary => {
            state.binary = true;
            None
        }
        FileContents::Unreadable => {
            state.read_error = Some("(no se pudo leer el archivo)".to_owned());
            None
        }
    };
    (state, source)
}

#[cfg(test)]
fn load_file_for_view(path: &Path) -> FileViewerState {
    let contents = super::file_viewer_reader::load_file_for_view(
        path,
        &|| false,
        &super::code_highlight::detect_language,
    )
    .expect("test file read is not cancelled");
    loaded_file_state(path.to_path_buf(), contents).0
}

#[cfg(test)]
mod tests {
    use super::{load_file_for_view, FileViewerState, MAX_VIEW_BYTES, MAX_VIEW_LINES};

    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("file-viewer-{tag}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn missing_file_reports_error_line_instead_of_panicking() {
        let state = load_file_for_view(&temp_path("missing"));
        assert!(!state.binary);
        assert!(!state.truncated);
        assert!(state.document.is_none());
        assert!(state
            .read_error
            .as_deref()
            .unwrap()
            .contains("no se pudo leer"));
    }

    #[test]
    fn text_file_splits_into_lines_without_trailing_empty() {
        let path = temp_path("text");
        std::fs::write(&path, b"alpha\nbeta\ngamma\n").expect("write");
        let state = load_file_for_view(&path);
        let _ = std::fs::remove_file(&path);

        assert!(!state.binary);
        assert!(!state.truncated);
        let document = state.document.unwrap();
        assert_eq!(document.logical_lines().len(), 3);
        assert_eq!(
            (0..3)
                .map(|index| document.logical_line_text(index).unwrap())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"]
        );
        assert_eq!(document.source(), "alpha\nbeta\ngamma\n");
    }

    #[test]
    fn nul_byte_marks_file_as_binary_and_skips_lines() {
        let path = temp_path("binary");
        std::fs::write(&path, b"ELF\0\x01\x02rest").expect("write");
        let state = load_file_for_view(&path);
        let _ = std::fs::remove_file(&path);

        assert!(state.binary);
        assert!(state.document.is_none());
    }

    #[test]
    fn file_at_exactly_the_cap_is_not_reported_as_truncated() {
        let path = temp_path("exact");
        let body = vec![b'a'; MAX_VIEW_BYTES as usize];
        std::fs::write(&path, &body).expect("write");
        let state = load_file_for_view(&path);
        let _ = std::fs::remove_file(&path);

        assert!(!state.truncated, "cap-sized file must not be truncated");
        let document = state.document.unwrap();
        assert_eq!(document.logical_lines().len(), 1);
        assert_eq!(document.source().len(), MAX_VIEW_BYTES as usize);
        assert!(document.has_long_lines());
        assert!(document
            .fragments()
            .iter()
            .all(|fragment| fragment.source.len()
                <= super::super::file_viewer_document::FRAGMENT_BYTE_CAP));
    }

    #[test]
    fn oversized_file_is_truncated_to_the_cap() {
        let path = temp_path("oversized");
        let body = vec![b'b'; MAX_VIEW_BYTES as usize + 4096];
        std::fs::write(&path, &body).expect("write");
        let state = load_file_for_view(&path);
        let _ = std::fs::remove_file(&path);

        assert!(state.truncated, "oversized file must be flagged truncated");
        assert_eq!(
            state.document.unwrap().source().len(),
            MAX_VIEW_BYTES as usize
        );
    }

    #[test]
    fn many_tiny_lines_are_capped_to_avoid_memory_amplification() {
        let path = temp_path("many-lines");
        let body = "x\n".repeat(MAX_VIEW_LINES + 10);
        std::fs::write(&path, body).expect("write");
        let state = load_file_for_view(&path);
        let _ = std::fs::remove_file(&path);

        assert!(state.truncated);
        assert_eq!(
            state.document.unwrap().logical_lines().len(),
            MAX_VIEW_LINES
        );
    }

    fn viewer_with(lines: &[&str], highlighted: Vec<super::HighlightedLine>) -> FileViewerState {
        FileViewerState {
            path: std::path::PathBuf::from("/tmp/demo.rs"),
            document: Some(std::sync::Arc::new(super::SourceDocument::prepare(
                std::sync::Arc::from(lines.join("\n")),
            ))),
            selection: super::SelectionState::default(),
            read_error: None,
            truncated: false,
            binary: false,
            highlighted,
            highlight_token: None,
            language: Some("Rust".to_owned()),
            loading: false,
        }
    }

    fn detached_app(ctx: &egui::Context) -> super::TerminalApp {
        let workspace = crate::state::Workspace::new("Viewer regression", None);
        let state = crate::state::AppState {
            schema_version: crate::state::persistence::APP_STATE_SCHEMA_VERSION,
            workspaces: vec![workspace.to_saved()],
            active_ws: 0,
            sidebar_visible: true,
            legacy_canvas_ui: Default::default(),
            local_device_id: uuid::Uuid::new_v4().to_string(),
            trusted_devices: Vec::new(),
            orchestration: Default::default(),
        };
        super::TerminalApp::build(ctx, None, Some(state), None, false, false)
    }

    #[test]
    fn loading_shares_the_original_buffer_with_the_highlighter() {
        let source: std::sync::Arc<str> = std::sync::Arc::from("fn main() {}\r\n");
        let document = std::sync::Arc::new(super::SourceDocument::prepare(source.clone()));
        let (state, highlight_source) = super::loaded_file_state(
            "main.rs".into(),
            super::FileContents::Text {
                document: document.clone(),
                truncated: false,
                language: Some("Rust".to_owned()),
            },
        );
        assert!(std::sync::Arc::ptr_eq(&highlight_source.unwrap(), &source));
        assert!(std::sync::Arc::ptr_eq(
            state.document.as_ref().unwrap(),
            &document
        ));
        assert_eq!(document.source(), "fn main() {}\r\n");
    }

    #[test]
    fn huge_line_activation_keeps_all_source_and_skips_the_highlight_request() {
        let ctx = egui::Context::default();
        let mut app = detached_app(&ctx);
        let mut state = viewer_with(&["x".repeat(MAX_VIEW_BYTES as usize).as_str()], Vec::new());
        let source = state.document.as_ref().unwrap().source_arc();
        state.truncated = false;
        app.activate_loaded_file(state, Some(source.clone()));
        let viewer = app.file_viewer.as_ref().unwrap();
        assert!(viewer.highlight_token.is_none());
        assert!(viewer.highlighted.is_empty());
        assert!(!viewer.truncated);
        assert!(std::sync::Arc::ptr_eq(
            &viewer.document.as_ref().unwrap().source_arc(),
            &source
        ));
        assert_eq!(
            viewer.document.as_ref().unwrap().source().len(),
            MAX_VIEW_BYTES as usize
        );
    }

    #[test]
    fn short_file_activation_still_requests_async_syntax_highlighting() {
        let ctx = egui::Context::default();
        let mut app = detached_app(&ctx);
        let state = viewer_with(&["fn main() {}"], Vec::new());
        let source = state.document.as_ref().unwrap().source_arc();
        app.activate_loaded_file(state, Some(source));
        assert!(app.file_viewer.as_ref().unwrap().highlight_token.is_some());
    }

    #[test]
    fn loading_binary_and_error_headers_keep_focus_until_escape() {
        use egui::{Event, Modifiers, PointerButton, RawInput};
        for kind in ["loading", "binary", "error"] {
            let ctx = egui::Context::default();
            let mut app = detached_app(&ctx);
            let mut viewer = super::loading_file_state("placeholder.txt".into());
            viewer.loading = kind == "loading";
            viewer.binary = kind == "binary";
            viewer.read_error = (kind == "error").then(|| "controlled read failure".to_owned());
            app.file_viewer = Some(viewer);
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 600.0));
            let mut frame = |events| {
                let mut output = ctx.run_ui(
                    RawInput {
                        screen_rect: Some(screen),
                        focused: true,
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        app.show_file_viewer(ui);
                        egui::CentralPanel::default().show(ui, |ui| {
                            ui.label("Terminal area");
                        });
                    },
                );
                output.textures_delta.clear();
                output
            };
            frame(Vec::new()).drop_without_applying_deltas();
            let output = frame(Vec::new());
            let point = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.text() == "placeholder.txt" => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .expect("viewer header");
            output.drop_without_applying_deltas();
            for pressed in [true, false] {
                frame(vec![
                    Event::PointerMoved(point),
                    Event::PointerButton {
                        pos: point,
                        button: PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    },
                ])
                .drop_without_applying_deltas();
            }
            frame(Vec::new()).drop_without_applying_deltas();
            frame(Vec::new()).drop_without_applying_deltas();
            assert!(
                super::file_viewer_selection::viewer_has_keyboard_focus(&ctx),
                "{kind}"
            );
            frame(vec![Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }])
            .drop_without_applying_deltas();
            assert!(
                app.file_viewer.is_none(),
                "Escape closes the focused {kind} viewer"
            );
            assert!(app.terminal_input_is_routable());
            assert!(!ctx.input(|input| input.events.iter().any(|event| matches!(
                event,
                Event::Key {
                    key: egui::Key::Escape,
                    pressed: true,
                    ..
                }
            ))));
        }
    }

    #[test]
    fn copying_across_code_lines_keeps_unicode_and_excludes_gutter_numbers() {
        use egui::{Event, Modifiers, PointerButton, RawInput};

        let ctx = egui::Context::default();
        let lines = ["let palabra = \"cafe\u{301}\";", "let 漢字 = 2;"];
        let mut viewer = viewer_with(&lines, Vec::new());
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(620.0, 300.0));
        let mut frame = |events: Vec<Event>, time: f64| {
            let mut output = ctx.run_ui(
                RawInput {
                    screen_rect: Some(screen),
                    events,
                    time: Some(time),
                    focused: true,
                    ..Default::default()
                },
                |ui| super::draw_code(ui, &mut viewer),
            );
            // This headless test inspects UI output rather than uploading fonts.
            output.textures_delta.clear();
            output
        };
        frame(Vec::new(), 0.0).drop_without_applying_deltas();
        let output = frame(Vec::new(), 0.1);
        let cursor_position = |line: &str, at_end: bool| {
            let shape = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.text() == line => Some(text),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing code line {line:?}"));
            let cursor = if at_end {
                shape.galley.end()
            } else {
                shape.galley.begin()
            };
            let mut position = shape.pos + shape.galley.pos_from_cursor(cursor).center().to_vec2();
            // Keep each endpoint inside its label while choosing its first/last
            // caret. No assumptions about Unicode glyph width or DPI are needed.
            position.x += if at_end { -0.25 } else { 0.25 };
            position
        };
        let start = cursor_position(lines[0], false);
        let end = cursor_position(lines[1], true);
        output.drop_without_applying_deltas();

        frame(
            vec![
                Event::PointerMoved(start),
                Event::PointerButton {
                    pos: start,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
            0.2,
        )
        .drop_without_applying_deltas();
        frame(vec![Event::PointerMoved(end)], 0.3).drop_without_applying_deltas();
        frame(
            vec![Event::PointerButton {
                pos: end,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
            0.4,
        )
        .drop_without_applying_deltas();
        let output = frame(vec![Event::Copy], 0.5);
        let copied: Vec<_> = output
            .platform_output
            .commands
            .iter()
            .filter_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(copied, vec![lines.join("\n").as_str()]);
        output.drop_without_applying_deltas();
    }
}
