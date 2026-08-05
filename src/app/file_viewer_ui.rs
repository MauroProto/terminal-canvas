//! Visor de código dockeado a la derecha del canvas (como el editor de Orca):
//! panel redimensionable, con gutter de números de línea y resaltado de
//! sintaxis real vía `syntect`.
//!
//! Dos decisiones de fluidez:
//! - El archivo se muestra en texto plano al instante y el coloreado llega
//!   después desde el worker (`code_highlight`), así abrir nunca traba el frame.
//! - La lista se dibuja virtualizada (`show_rows`): sólo se pintan las líneas
//!   visibles, así un archivo de 20k líneas cuesta lo mismo que uno de 50.

use std::io::Read;
use std::path::{Path, PathBuf};

use egui::{vec2, Color32, FontId, RichText, ScrollArea};

use crate::theme::colors as palette;

use super::code_highlight::HighlightedLine;
use super::TerminalApp;

/// Tope de bytes a leer para no bloquear la UI con archivos enormes.
const MAX_VIEW_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_WIDTH: f32 = 620.0;
const MIN_WIDTH: f32 = 320.0;
const MAX_WIDTH: f32 = 1200.0;
const CODE_FONT_SIZE: f32 = 12.5;
/// Interlineado extra: pegadas, las líneas de código se leen mal. Un editor
/// decente deja aire entre renglones.
const LINE_SPACING: f32 = 3.0;

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
    pub(super) lines: Vec<String>,
    pub(super) truncated: bool,
    pub(super) binary: bool,
    /// Líneas ya coloreadas; vacío mientras el worker trabaja.
    pub(super) highlighted: Vec<HighlightedLine>,
    /// Token del pedido en curso, para descartar resultados viejos.
    pub(super) highlight_token: Option<u64>,
    pub(super) language: Option<String>,
}

impl FileViewerState {
    fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// Ancho del gutter según la cantidad de dígitos del número más alto.
    fn gutter_width(&self) -> f32 {
        let digits = self.lines.len().max(1).to_string().len();
        12.0 + digits as f32 * 7.5
    }
}

impl TerminalApp {
    pub(super) fn open_file_viewer(&mut self, path: PathBuf) {
        let mut state = load_file_for_view(&path);
        if !state.binary && !state.lines.is_empty() {
            let name = state.file_name();
            state.language = super::code_highlight::detect_language(&name, state.lines[0].as_str());
            // El texto se reensambla para el worker; el visor ya puede pintar
            // el plano mientras tanto.
            let text = state.lines.join("\n");
            state.highlight_token = Some(self.highlighter.request(name, text));
        }
        self.file_viewer = Some(state);
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
            }
        }
    }

    pub(super) fn show_file_viewer(&mut self, ctx: &egui::Context) {
        self.poll_highlighter();
        if self.file_viewer.is_none() {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.file_viewer = None;
            return;
        }

        let mut close = false;
        let mut open_external: Option<PathBuf> = None;

        egui::SidePanel::right("code-viewer")
            .resizable(true)
            .default_width(DEFAULT_WIDTH)
            .width_range(MIN_WIDTH..=MAX_WIDTH)
            .frame(
                egui::Frame::none()
                    .fill(code_bg())
                    .inner_margin(egui::Margin::same(0.0)),
            )
            .show(ctx, |ui| {
                let Some(viewer) = self.file_viewer.as_ref() else {
                    return;
                };
                // Borde izquierdo que separa el código del canvas.
                let panel_rect = ui.max_rect();
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(panel_rect.min, vec2(1.0, panel_rect.height())),
                    0.0,
                    palette::LINE,
                );

                draw_header(ui, viewer, &mut close, &mut open_external);
                ui.separator();

                if viewer.binary {
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new("Archivo binario: no se puede mostrar como texto")
                            .size(11.0)
                            .color(palette::DIM),
                    );
                    return;
                }

                draw_code(ui, viewer);
            });

        if close {
            self.file_viewer = None;
        }
        if let Some(path) = open_external {
            if let Err(err) = crate::utils::platform::open_path_external(&path) {
                self.toast_error(format!("No se pudo abrir en el editor: {err}"));
            }
        }
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
        let mut status = format!("{} líneas", viewer.lines.len());
        if viewer.truncated {
            status.push_str(" · truncado a 2 MB");
        }
        if viewer.highlighted.is_empty() && !viewer.lines.is_empty() {
            status.push_str(" · coloreando…");
        }
        ui.label(RichText::new(status).size(10.0).color(palette::DIM));
    });
    ui.add_space(6.0);
}

fn draw_code(ui: &mut egui::Ui, viewer: &FileViewerState) {
    let font = FontId::monospace(CODE_FONT_SIZE);
    // La fila que reserva `show_rows` tiene que coincidir exactamente con la
    // que ocupa cada renglón, si no el gutter se desalinea del código.
    let row_height = ui.fonts(|fonts| fonts.row_height(&font)) + LINE_SPACING;
    let gutter_width = viewer.gutter_width();
    let total_rows = viewer.lines.len();

    ScrollArea::both()
        .id_salt("code-viewer-scroll")
        .auto_shrink([false, false])
        .show_rows(ui, row_height, total_rows, |ui, range| {
            ui.spacing_mut().item_spacing.y = LINE_SPACING;
            // El gutter se pinta como una banda continua a lo alto de todo lo
            // que se está dibujando, no por fila: si no, se ve rayado.
            let area = ui.max_rect();
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    area.min,
                    vec2(gutter_width, area.height().max(ui.available_height())),
                ),
                0.0,
                gutter_bg(),
            );

            for index in range {
                let Some(plain) = viewer.lines.get(index) else {
                    continue;
                };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    // Número de línea alineado a la derecha del gutter.
                    ui.allocate_ui_with_layout(
                        vec2(gutter_width, row_height),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new((index + 1).to_string())
                                    .font(font.clone())
                                    .color(gutter_fg()),
                            );
                        },
                    );
                    ui.add_space(10.0);
                    let job = line_layout_job(viewer, index, plain, &font);
                    // `selectable`: un visor de código del que no podés copiar
                    // no sirve para nada.
                    ui.add(
                        egui::Label::new(job)
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .selectable(true),
                    );
                });
            }
        });
}

/// Dibuja una línea: coloreada si el worker ya la entregó, plana si no.
///
/// Se arma un único `LayoutJob` por línea en vez de un label por tramo. Dos
/// razones: un label por tramo serían ~400 widgets por frame (50 líneas × 8
/// tramos), y además cada label se posiciona por separado, lo que rompe la
/// alineación monoespaciada. Con un solo job el texto es una sola tirada.
fn line_layout_job(
    viewer: &FileViewerState,
    index: usize,
    plain: &str,
    font: &FontId,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    // Sin wrap: una línea de código es una fila. Si envolviera, la fila
    // ocuparía más alto del que `show_rows` reservó y todo se desalinearía.
    job.wrap.max_width = f32::INFINITY;
    job.break_on_newline = false;

    let format_for = |color: Color32| egui::TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };

    match viewer.highlighted.get(index) {
        Some(spans) if !spans.is_empty() => {
            for (color, piece) in spans {
                job.append(piece, 0.0, format_for(*color));
            }
        }
        _ => job.append(plain, 0.0, format_for(plain_fg())),
    }
    job
}
fn load_file_for_view(path: &Path) -> FileViewerState {
    let unreadable = || FileViewerState {
        path: path.to_path_buf(),
        lines: vec!["(no se pudo leer el archivo)".to_owned()],
        truncated: false,
        binary: false,
        highlighted: Vec::new(),
        highlight_token: None,
        language: None,
    };

    // Lectura acotada: leemos como máximo un byte más que el tope para poder
    // distinguir "justo del tamaño del tope" de "más grande que el tope", sin
    // reservar memoria proporcional al archivo real.
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return unreadable(),
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_VIEW_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return unreadable();
    }
    let truncated = bytes.len() as u64 > MAX_VIEW_BYTES;
    if truncated {
        bytes.truncate(MAX_VIEW_BYTES as usize);
    }

    if bytes.contains(&0) {
        return FileViewerState {
            path: path.to_path_buf(),
            lines: Vec::new(),
            truncated: false,
            binary: true,
            highlighted: Vec::new(),
            highlight_token: None,
            language: None,
        };
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<String> = text.lines().map(|line| line.to_owned()).collect();
    FileViewerState {
        path: path.to_path_buf(),
        lines,
        truncated,
        binary: false,
        highlighted: Vec::new(),
        highlight_token: None,
        language: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{load_file_for_view, FileViewerState, MAX_VIEW_BYTES};

    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("file-viewer-{tag}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn missing_file_reports_error_line_instead_of_panicking() {
        let state = load_file_for_view(&temp_path("missing"));
        assert!(!state.binary);
        assert!(!state.truncated);
        assert_eq!(state.lines.len(), 1);
        assert!(state.lines[0].contains("no se pudo leer"));
    }

    #[test]
    fn text_file_splits_into_lines_without_trailing_empty() {
        let path = temp_path("text");
        std::fs::write(&path, b"alpha\nbeta\ngamma\n").expect("write");
        let state = load_file_for_view(&path);
        let _ = std::fs::remove_file(&path);

        assert!(!state.binary);
        assert!(!state.truncated);
        assert_eq!(state.lines, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn nul_byte_marks_file_as_binary_and_skips_lines() {
        let path = temp_path("binary");
        std::fs::write(&path, b"ELF\0\x01\x02rest").expect("write");
        let state = load_file_for_view(&path);
        let _ = std::fs::remove_file(&path);

        assert!(state.binary);
        assert!(state.lines.is_empty());
    }

    #[test]
    fn file_at_exactly_the_cap_is_not_reported_as_truncated() {
        let path = temp_path("exact");
        let body = vec![b'a'; MAX_VIEW_BYTES as usize];
        std::fs::write(&path, &body).expect("write");
        let state = load_file_for_view(&path);
        let _ = std::fs::remove_file(&path);

        assert!(!state.truncated, "cap-sized file must not be truncated");
        assert_eq!(state.lines.len(), 1);
        assert_eq!(state.lines[0].len(), MAX_VIEW_BYTES as usize);
    }

    #[test]
    fn oversized_file_is_truncated_to_the_cap() {
        let path = temp_path("oversized");
        let body = vec![b'b'; MAX_VIEW_BYTES as usize + 4096];
        std::fs::write(&path, &body).expect("write");
        let state = load_file_for_view(&path);
        let _ = std::fs::remove_file(&path);

        assert!(state.truncated, "oversized file must be flagged truncated");
        let total: usize = state.lines.iter().map(String::len).sum();
        assert_eq!(total, MAX_VIEW_BYTES as usize);
    }
    fn viewer_with(lines: &[&str], highlighted: Vec<super::HighlightedLine>) -> FileViewerState {
        FileViewerState {
            path: std::path::PathBuf::from("/tmp/demo.rs"),
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            truncated: false,
            binary: false,
            highlighted,
            highlight_token: None,
            language: Some("Rust".to_owned()),
        }
    }

    fn job_text(job: &egui::text::LayoutJob) -> String {
        job.sections
            .iter()
            .map(|section| &job.text[section.byte_range.clone()])
            .collect()
    }

    #[test]
    fn a_code_line_never_wraps() {
        // Si envolviera, la fila ocuparía más alto del reservado por show_rows
        // y el gutter dejaría de alinear con el código.
        let viewer = viewer_with(&["x".repeat(4000).as_str()], Vec::new());
        let font = egui::FontId::monospace(12.0);
        let job = super::line_layout_job(&viewer, 0, &viewer.lines[0], &font);
        assert_eq!(job.wrap.max_width, f32::INFINITY);
        assert!(!job.break_on_newline);
    }

    #[test]
    fn highlighted_line_keeps_the_exact_source_text() {
        let spans = vec![vec![
            (egui::Color32::RED, "fn ".to_owned()),
            (egui::Color32::GREEN, "main".to_owned()),
            (egui::Color32::WHITE, "() {}".to_owned()),
        ]];
        let viewer = viewer_with(&["fn main() {}"], spans);
        let font = egui::FontId::monospace(12.0);
        let job = super::line_layout_job(&viewer, 0, &viewer.lines[0], &font);
        assert_eq!(job_text(&job), "fn main() {}");
        assert_eq!(job.sections.len(), 3, "one section per span");
    }

    #[test]
    fn indentation_is_preserved_verbatim() {
        let viewer = viewer_with(&["        anidado()"], Vec::new());
        let font = egui::FontId::monospace(12.0);
        let job = super::line_layout_job(&viewer, 0, &viewer.lines[0], &font);
        assert!(job_text(&job).starts_with("        "), "lost the indent");
    }

    #[test]
    fn falls_back_to_plain_text_while_the_worker_is_still_running() {
        let viewer = viewer_with(&["let x = 1;"], Vec::new());
        let font = egui::FontId::monospace(12.0);
        let job = super::line_layout_job(&viewer, 0, &viewer.lines[0], &font);
        assert_eq!(job_text(&job), "let x = 1;");
        assert_eq!(job.sections.len(), 1);
    }

    #[test]
    fn a_line_past_the_highlight_cap_still_renders_plain() {
        // El worker recorta a MAX_HIGHLIGHT_LINES; las siguientes no tienen
        // spans y no deben quedar en blanco.
        let viewer = viewer_with(
            &["uno", "dos"],
            vec![vec![(egui::Color32::RED, "uno".to_owned())]],
        );
        let font = egui::FontId::monospace(12.0);
        let job = super::line_layout_job(&viewer, 1, &viewer.lines[1], &font);
        assert_eq!(job_text(&job), "dos");
    }

    #[test]
    fn gutter_widens_with_the_line_count() {
        let narrow = viewer_with(&["a"], Vec::new());
        let wide = viewer_with(&["a"; 10_000], Vec::new());
        assert!(
            wide.gutter_width() > narrow.gutter_width(),
            "a 5-digit gutter must be wider than a 1-digit one"
        );
    }
}
