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
use super::file_viewer_reader::{FileContents, FileReadError, FileReadOperation, FileViewerReader};
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
    pub(super) read_error: Option<FileReadError>,
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
        self.start_file_viewer_load(path, false);
    }

    fn start_file_viewer_load(&mut self, path: PathBuf, preserve_keyboard_focus: bool) {
        self.highlighter.cancel();
        let restore_focus = preserve_keyboard_focus
            && self
                .ctx
                .as_ref()
                .is_some_and(file_viewer_selection::viewer_keyboard_input_is_active);
        if !preserve_keyboard_focus {
            if let Some(ctx) = &self.ctx {
                file_viewer_selection::release_keyboard_focus(ctx);
            }
        }
        // Un lector cerrado ya no ejecuta lecturas. Un lector ocupado sigue
        // disponible y se reutiliza: reintentar no crea otro worker para una
        // llamada al filesystem que todavía no retornó.
        if self
            .file_viewer_reader
            .as_ref()
            .is_some_and(|reader| !reader.is_available())
        {
            self.file_viewer_reader = None;
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
                viewer.read_error = Some(FileReadError::WorkerUnavailable);
            }
        }
        if restore_focus {
            if let Some(ctx) = &self.ctx {
                file_viewer_selection::request_keyboard_focus(ctx);
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
                        viewer.read_error = Some(FileReadError::WorkerUnavailable);
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
        let can_focus = root_ui.is_enabled()
            && !self.modal_input_is_active()
            && ctx.input(|input| input.raw.focused);
        if !can_focus {
            file_viewer_selection::release_keyboard_focus(&ctx);
        }
        if can_focus
            && file_viewer_selection::viewer_handles_escape(&ctx)
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
        let mut retry: Option<PathBuf> = None;

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
                let already_focused = file_viewer_selection::viewer_has_keyboard_focus(&ctx);
                let focus_requested = ctx.input(|input| {
                    (input.pointer.primary_pressed()
                        || (input.pointer.primary_released() && already_focused))
                        .then(|| input.pointer.interact_pos())
                        .flatten()
                });
                let focus_requested = if let Some(pointer) = focus_requested {
                    if !panel_rect.contains(pointer)
                        && ctx.input(|input| input.pointer.primary_pressed())
                    {
                        file_viewer_selection::release_keyboard_focus(&ctx);
                    }
                    can_focus && panel_rect.contains(pointer)
                } else {
                    false
                };
                file_viewer_selection::begin_viewer_controls(&ctx);

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
                    if can_focus && panel_rect.contains(pointer) {
                        open_dropped = Some(path);
                    }
                }
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(panel_rect.min, vec2(1.0, panel_rect.height())),
                    0.0,
                    palette::LINE,
                );

                ui.scope(|ui| {
                    if !can_focus {
                        ui.disable();
                    }
                    draw_header(ui, viewer, &mut close, &mut open_external);
                });
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
                        file_viewer_selection::register_placeholder_keyboard_owner(
                            ui,
                            rect,
                            focus_requested,
                        );
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

                if let Some(error) = viewer.read_error {
                    ui.add_space(16.0);
                    let (message, help) = read_error_message(error);
                    ui.label(RichText::new(message).size(11.0).color(palette::DIM));
                    ui.label(RichText::new(help).size(11.0).color(palette::DIM));
                    ui.label(RichText::new(viewer.path.to_string_lossy()).size(11.0));
                    let button = ui.add_enabled(can_focus, egui::Button::new("Reintentar"));
                    file_viewer_selection::register_viewer_control(&button);
                    if button.clicked() {
                        retry = Some(viewer.path.clone());
                    }
                    return;
                }

                if !can_focus {
                    ui.disable();
                }
                draw_code(ui, viewer, focus_requested);
            });

        if close {
            self.highlighter.cancel();
            if let Some(reader) = self.file_viewer_reader.as_mut() {
                reader.cancel();
            }
            self.file_viewer = None;
            file_viewer_selection::release_keyboard_focus(&ctx);
        } else if let Some(path) = open_dropped {
            self.open_file_viewer(path);
        } else if let Some(path) = retry {
            // El botón pertenece al visor actual: conservar el owner y su
            // filtro de eventos permite cerrar con Esc durante el reintento.
            self.start_file_viewer_load(path, true);
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
    // Reservar primero los botones mantiene las acciones dentro del panel,
    // incluso con nombres muy largos y el ancho mínimo del visor.
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            let close_button = ui.small_button("✕");
            file_viewer_selection::register_viewer_control(&close_button);
            if close_button.on_hover_text("Cerrar (Esc)").clicked() {
                *close = true;
            }
            let external_button = ui.small_button("↗");
            file_viewer_selection::register_viewer_control(&external_button);
            if external_button
                .on_hover_text("Abrir en el editor externo")
                .clicked()
            {
                *open_external = Some(viewer.path.clone());
            }
            let path_button = ui.small_button("Ruta");
            file_viewer_selection::register_viewer_control(&path_button);
            if path_button.on_hover_text("Copiar ruta completa").clicked() {
                ui.ctx()
                    .copy_text(viewer.path.to_string_lossy().into_owned());
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add_space(10.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(viewer.file_name())
                            .size(12.5)
                            .color(palette::TEXT_STRONG),
                    )
                    .truncate()
                    .selectable(false)
                    .halign(egui::Align::LEFT)
                    .show_tooltip_when_elided(false),
                )
                .on_hover_text(viewer.path.to_string_lossy());
            });
        });
    });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.add_space(10.0);
        let line_count = viewer
            .document
            .as_ref()
            .map_or(0, |document| document.logical_lines().len());
        let mut status = if viewer.loading {
            "Leyendo archivo…".to_owned()
        } else if viewer.read_error.is_some() {
            "No se pudo leer el archivo".to_owned()
        } else if viewer.binary {
            "Archivo binario".to_owned()
        } else {
            format!("{line_count} líneas")
        };
        if let Some(language) = &viewer.language {
            status.push_str(" · ");
            status.push_str(language);
        }
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

fn draw_code(ui: &mut egui::Ui, viewer: &mut FileViewerState, focus_requested: bool) {
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
        focus_requested,
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
        FileContents::Unreadable(error) => {
            state.read_error = Some(error);
            None
        }
    };
    (state, source)
}

fn read_error_message(error: FileReadError) -> (&'static str, &'static str) {
    use std::io::ErrorKind;
    match error {
        FileReadError::NotRegularFile => (
            "Esta ruta no es un archivo regular.",
            "Las carpetas, dispositivos, pipes y sockets no se muestran. Abrí un archivo o soltalo sobre el visor.",
        ),
        FileReadError::Io {
            kind: ErrorKind::NotFound,
            ..
        } => (
            "El archivo no existe o su ruta ya no está disponible.",
            "Comprobá la ruta. Si restaurás el archivo o el volumen, podés reintentar.",
        ),
        FileReadError::Io {
            kind: ErrorKind::PermissionDenied,
            ..
        } => (
            "No se pudo leer el archivo: acceso denegado.",
            "Comprobá los permisos del archivo y de sus carpetas antes de reintentar.",
        ),
        FileReadError::Io {
            operation: FileReadOperation::Read,
            ..
        } => (
            "La lectura del archivo se interrumpió.",
            "Comprobá que el disco o volumen esté disponible y reintentá.",
        ),
        FileReadError::Io { .. } => (
            "No se pudo abrir el archivo.",
            "Comprobá la ruta, los permisos y la disponibilidad del volumen.",
        ),
        FileReadError::WorkerUnavailable => (
            "El lector de archivos dejó de estar disponible.",
            "Reintentá para iniciar un lector nuevo.",
        ),
    }
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
        assert!(matches!(
            state.read_error,
            Some(super::FileReadError::Io {
                kind: std::io::ErrorKind::NotFound,
                ..
            })
        ));
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

    fn viewer_frame(
        app: &mut super::TerminalApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        focused: bool,
        width: f32,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 600.0),
                )),
                focused,
                events,
                ..Default::default()
            },
            |ui| app.show_file_viewer(ui),
        );
        output.textures_delta.clear();
        output
    }

    fn painted_text_center(output: &egui::FullOutput, text: &str) -> egui::Pos2 {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(value) if value.galley.text() == text => {
                    Some(value.pos + value.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing painted text {text:?}"))
    }

    fn click_viewer(
        app: &mut super::TerminalApp,
        ctx: &egui::Context,
        point: egui::Pos2,
        focused: bool,
        width: f32,
    ) -> egui::FullOutput {
        let mut last = None;
        for pressed in [true, false] {
            let output = viewer_frame(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                focused,
                width,
            );
            if let Some(previous) = last.replace(output) {
                previous.drop_without_applying_deltas();
            }
        }
        last.expect("pointer release frame")
    }

    fn wait_for_viewer(app: &mut super::TerminalApp, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app
            .file_viewer
            .as_ref()
            .is_some_and(|viewer| viewer.loading)
        {
            app.poll_file_viewer_updates(ctx);
            assert!(std::time::Instant::now() < deadline, "bounded viewer load");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn retry_button_recovers_a_missing_file_without_changing_its_path() {
        let ctx = egui::Context::default();
        let mut app = detached_app(&ctx);
        let path = temp_path("retry-café");
        app.file_viewer = Some(load_file_for_view(&path));
        viewer_frame(&mut app, &ctx, Vec::new(), true, 900.0).drop_without_applying_deltas();
        let output = viewer_frame(&mut app, &ctx, Vec::new(), true, 900.0);
        let retry = painted_text_center(&output, "Reintentar");
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::epaint::Shape::Text(value) if value.galley.text() == path.to_string_lossy())));
        output.drop_without_applying_deltas();
        let source = "const título = \"café 🐈\";\r\n";
        std::fs::write(&path, source).expect("restore missing file");
        click_viewer(&mut app, &ctx, retry, true, 900.0).drop_without_applying_deltas();
        let loading = app.file_viewer.as_ref().unwrap();
        assert_eq!(loading.path, path);
        assert!(loading.loading);
        assert!(loading.read_error.is_none());
        assert!(loading.document.is_none());
        wait_for_viewer(&mut app, &ctx);
        std::fs::remove_file(&path).expect("remove restored fixture");
        let loaded = app.file_viewer.as_ref().unwrap();
        assert_eq!(loaded.path, path);
        assert!(loaded.read_error.is_none());
        assert_eq!(loaded.document.as_ref().unwrap().source(), source);
    }

    #[test]
    fn retry_keeps_escape_working_while_the_reader_is_busy() {
        let ctx = egui::Context::default();
        let mut app = detached_app(&ctx);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        app.file_viewer_reader = Some(super::FileViewerReader::with_test_loader(
            move |_, cancelled| {
                entered_tx.send(()).expect("announce active read");
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("release active read");
                cancelled_tx
                    .send(cancelled())
                    .expect("observe Escape cancellation");
                None
            },
        ));
        let mut state = super::loading_file_state("retry.rs".into());
        state.loading = false;
        state.read_error = Some(super::FileReadError::WorkerUnavailable);
        app.file_viewer = Some(state);
        viewer_frame(&mut app, &ctx, Vec::new(), true, 900.0).drop_without_applying_deltas();
        let output = viewer_frame(&mut app, &ctx, Vec::new(), true, 900.0);
        let retry = painted_text_center(&output, "Reintentar");
        output.drop_without_applying_deltas();
        click_viewer(&mut app, &ctx, retry, true, 900.0).drop_without_applying_deltas();
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("retry started");
        assert!(super::file_viewer_selection::viewer_has_keyboard_focus(
            &ctx
        ));
        viewer_frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            true,
            900.0,
        )
        .drop_without_applying_deltas();
        release_tx.send(()).expect("release cancelled worker");
        assert!(cancelled_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker observes Escape before app Drop"));
        assert!(app.file_viewer.is_none());
        assert!(app.terminal_input_is_routable());
        assert!(!ctx.input(|input| input.events.iter().any(|event| matches!(
            event,
            egui::Event::Key {
                key: egui::Key::Escape,
                pressed: true,
                ..
            }
        ))));
    }

    #[test]
    fn tab_activates_retry_without_routing_enter_or_immediate_escape_to_the_terminal() {
        let ctx = egui::Context::default();
        let mut app = detached_app(&ctx);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        app.file_viewer_reader = Some(super::FileViewerReader::with_test_loader(
            move |_, cancelled| {
                entered_tx.send(()).expect("announce keyboard retry");
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("release keyboard retry");
                cancelled_tx
                    .send(cancelled())
                    .expect("observe keyboard Escape cancellation");
                None
            },
        ));
        let mut state = super::loading_file_state("keyboard-retry.rs".into());
        state.loading = false;
        state.read_error = Some(super::FileReadError::WorkerUnavailable);
        app.file_viewer = Some(state);
        viewer_frame(&mut app, &ctx, Vec::new(), true, 900.0).drop_without_applying_deltas();
        let output = viewer_frame(&mut app, &ctx, Vec::new(), true, 900.0);
        let retry = painted_text_center(&output, "Reintentar");
        output.drop_without_applying_deltas();
        let key = |key, pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let mut reached_retry = false;
        for _ in 0..8 {
            viewer_frame(&mut app, &ctx, vec![key(egui::Key::Tab, true)], true, 900.0)
                .drop_without_applying_deltas();
            reached_retry = !super::file_viewer_selection::viewer_has_keyboard_focus(&ctx)
                && super::file_viewer_selection::viewer_keyboard_input_is_active(&ctx)
                && ctx
                    .memory(|memory| memory.focused())
                    .and_then(|id| ctx.read_response(id))
                    .is_some_and(|response| response.rect.contains(retry));
            if reached_retry {
                break;
            }
            viewer_frame(
                &mut app,
                &ctx,
                vec![key(egui::Key::Tab, false)],
                true,
                900.0,
            )
            .drop_without_applying_deltas();
        }
        assert!(reached_retry, "Tab reaches the actual retry button");
        assert!(
            !app.terminal_input_is_routable(),
            "focused viewer control owns input"
        );
        viewer_frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::Tab, false), key(egui::Key::Enter, true)],
            true,
            900.0,
        )
        .drop_without_applying_deltas();
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("keyboard retry started");
        assert!(app.file_viewer.as_ref().unwrap().loading);
        assert!(super::file_viewer_selection::viewer_has_keyboard_focus(
            &ctx
        ));
        assert!(!app.terminal_input_is_routable());
        viewer_frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::Enter, false), key(egui::Key::Escape, true)],
            true,
            900.0,
        )
        .drop_without_applying_deltas();
        release_tx
            .send(())
            .expect("release keyboard-cancelled generation");
        assert!(cancelled_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker observes keyboard Escape before app Drop"));
        assert!(app.file_viewer.is_none());
        assert!(app.terminal_input_is_routable());
        assert!(!ctx.input(|input| input.events.iter().any(|event| matches!(
            event,
            egui::Event::Key {
                key: egui::Key::Escape,
                pressed: true,
                ..
            }
        ))));
    }

    #[test]
    fn reopening_a_busy_reader_reuses_it_and_loads_only_the_latest_generation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let ctx = egui::Context::default();
        let mut app = detached_app(&ctx);
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let worker_calls = calls.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        app.file_viewer_reader = Some(super::FileViewerReader::with_test_loader(
            move |path, cancelled| {
                assert_eq!(path, std::path::Path::new("busy.txt"));
                if worker_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    entered_tx.send(()).expect("first read active");
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .expect("release stale read");
                    assert!(cancelled());
                    return None;
                }
                Some(super::FileContents::Text {
                    document: std::sync::Arc::new(super::SourceDocument::prepare("latest".into())),
                    truncated: false,
                    language: None,
                })
            },
        ));
        app.open_file_viewer("busy.txt".into());
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("busy reader started");
        for _ in 0..32 {
            app.start_file_viewer_load("busy.txt".into(), true);
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(!app.file_viewer_reader.as_ref().unwrap().is_current(1));
        release_tx.send(()).expect("release stale generation");
        wait_for_viewer(&mut app, &ctx);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            app.file_viewer
                .as_ref()
                .unwrap()
                .document
                .as_ref()
                .unwrap()
                .source(),
            "latest"
        );
    }

    #[test]
    fn retry_replaces_a_reader_that_has_exited() {
        let ctx = egui::Context::default();
        let mut app = detached_app(&ctx);
        let path = temp_path("reader-exit");
        std::fs::write(&path, "recovered\r\n").expect("recovery source");
        app.file_viewer_reader = Some(super::FileViewerReader::with_test_loader(|_, _| {
            panic!("synthetic reader exit");
        }));
        app.open_file_viewer(path.clone());
        wait_for_viewer(&mut app, &ctx);
        assert_eq!(
            app.file_viewer.as_ref().unwrap().read_error,
            Some(super::FileReadError::WorkerUnavailable)
        );
        assert!(!app.file_viewer_reader.as_ref().unwrap().is_available());
        app.start_file_viewer_load(path.clone(), true);
        wait_for_viewer(&mut app, &ctx);
        std::fs::remove_file(&path).expect("remove recovery source");
        assert!(app.file_viewer_reader.as_ref().unwrap().is_available());
        let viewer = app.file_viewer.as_ref().unwrap();
        assert_eq!(viewer.path, path);
        assert!(viewer.read_error.is_none());
        assert_eq!(viewer.document.as_ref().unwrap().source(), "recovered\r\n");
    }

    #[test]
    fn modal_and_window_blur_block_retry_and_path_copy() {
        for (modal, focused) in [(true, true), (false, false)] {
            let ctx = egui::Context::default();
            let mut app = detached_app(&ctx);
            let mut state = super::loading_file_state("disabled.txt".into());
            state.loading = false;
            state.read_error = Some(super::FileReadError::WorkerUnavailable);
            app.file_viewer = Some(state);
            viewer_frame(&mut app, &ctx, Vec::new(), true, 900.0).drop_without_applying_deltas();
            let output = viewer_frame(&mut app, &ctx, Vec::new(), true, 900.0);
            let retry = painted_text_center(&output, "Reintentar");
            let copy = painted_text_center(&output, "Ruta");
            let header = painted_text_center(&output, "disabled.txt");
            output.drop_without_applying_deltas();
            click_viewer(&mut app, &ctx, header, true, 900.0).drop_without_applying_deltas();
            assert!(super::file_viewer_selection::viewer_has_keyboard_focus(
                &ctx
            ));
            app.command_palette.open = modal;
            for point in [retry, copy] {
                let output = click_viewer(&mut app, &ctx, point, focused, 900.0);
                assert!(!output
                    .platform_output
                    .commands
                    .iter()
                    .any(|command| matches!(command, egui::OutputCommand::CopyText(_))));
                output.drop_without_applying_deltas();
                assert!(app.file_viewer_reader.is_none());
                assert!(!app.file_viewer.as_ref().unwrap().loading);
                assert!(!super::file_viewer_selection::viewer_has_keyboard_focus(
                    &ctx
                ));
            }
            viewer_frame(
                &mut app,
                &ctx,
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                focused,
                900.0,
            )
            .drop_without_applying_deltas();
            assert!(
                app.file_viewer.is_some(),
                "modal or blurred Escape cannot close the viewer"
            );
            assert!(ctx.input(|input| input.events.iter().any(|event| matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Escape,
                    pressed: true,
                    ..
                }
            ))));
        }
    }

    #[test]
    fn minimum_width_header_keeps_actions_visible_and_copies_the_complete_unicode_path() {
        let ctx = egui::Context::default();
        let mut app = detached_app(&ctx);
        let path = std::path::PathBuf::from(format!(
            "/parent folder/{}/{}.rs",
            "café".repeat(40),
            "漢字🙂".repeat(80)
        ));
        let mut state = super::loading_file_state(path.clone());
        state.loading = false;
        state.read_error = Some(super::FileReadError::WorkerUnavailable);
        app.file_viewer = Some(state);
        viewer_frame(&mut app, &ctx, Vec::new(), true, 320.0).drop_without_applying_deltas();
        let output = viewer_frame(&mut app, &ctx, Vec::new(), true, 320.0);
        for label in ["✕", "↗", "Ruta"] {
            let center = painted_text_center(&output, label);
            assert!((0.0..=320.0).contains(&center.x), "{label}");
            assert!((0.0..=60.0).contains(&center.y), "{label}");
        }
        let point = painted_text_center(&output, "Ruta");
        output.drop_without_applying_deltas();
        let output = click_viewer(&mut app, &ctx, point, true, 320.0);
        let copied: Vec<_> = output
            .platform_output
            .commands
            .iter()
            .filter_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(copied, vec![path.to_string_lossy().as_ref()]);
        output.drop_without_applying_deltas();
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
        for (kind, idle_frames) in ["loading", "binary", "error"]
            .into_iter()
            .flat_map(|kind| [0, 2].map(|idle_frames| (kind, idle_frames)))
        {
            let ctx = egui::Context::default();
            let mut app = detached_app(&ctx);
            let mut viewer = super::loading_file_state("placeholder.txt".into());
            viewer.loading = kind == "loading";
            viewer.binary = kind == "binary";
            viewer.read_error =
                (kind == "error").then_some(super::FileReadError::WorkerUnavailable);
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
            let terminal_point = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.text() == "Terminal area" => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .expect("terminal area");
            output.drop_without_applying_deltas();
            for (interaction, start, end, expected_focus) in [
                ("header click", point, point, true),
                ("drag from terminal", terminal_point, point, false),
                ("header reactivation", point, point, true),
            ] {
                for (point, pressed) in [(start, true), (end, false)] {
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
                    assert_eq!(
                        super::file_viewer_selection::viewer_has_keyboard_focus(&ctx),
                        expected_focus,
                        "{kind}: {interaction} (pressed={pressed})"
                    );
                }
            }
            for _ in 0..idle_frames {
                frame(Vec::new()).drop_without_applying_deltas();
            }
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
                |ui| super::draw_code(ui, &mut viewer, false),
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
