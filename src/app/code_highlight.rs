//! Resaltado de sintaxis para el visor de código, con `syntect` (las mismas
//! gramáticas TextMate que usan VS Code y Sublime).
//!
//! El resaltado corre en un **worker thread**: medido en release tarda ~90 ms
//! cada 2000 líneas, y en debug bastante más, así que hacerlo en el hilo de UI
//! congelaría el frame al abrir un archivo. El visor muestra el texto plano al
//! instante y cambia a la versión coloreada cuando el worker la entrega.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::OnceLock;

use egui::Color32;
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;
use two_face::theme::EmbeddedThemeName;

/// Tope de líneas a resaltar. Más que esto no aporta (nadie lee 40k líneas
/// coloreadas) y sí cuesta memoria: el resto queda en texto plano.
pub const MAX_HIGHLIGHT_LINES: usize = 20_000;

/// Un tramo coloreado dentro de una línea.
pub type Span = (Color32, String);
/// Línea ya resaltada, partida en tramos.
pub type HighlightedLine = Vec<Span>;

/// Gramáticas. Se usa el set extendido de `two-face` (el mismo que empaqueta
/// `bat`) y no el de syntect, porque el de syntect **no trae TypeScript, TSX,
/// JSX ni TOML**: con él un `.ts` caía a texto plano y salía todo gris.
///
/// Va con `fancy-regex` en vez de `onig` para no arrastrar una dependencia C.
/// Medido en release sobre un archivo de 2380 líneas: 145 ms con fancy contra
/// 56 ms con onig. Como el resaltado corre en un worker, esa diferencia no se
/// percibe y a cambio el build queda sin toolchain de C.
fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// Catppuccin Mocha: de los temas disponibles es el que más colores distintos
/// produce sobre código real y el que mejor combina con el gris neutro de la
/// app.
fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        two_face::theme::extra()
            .get(EmbeddedThemeName::CatppuccinMocha)
            .clone()
    })
}

/// Fondo que el tema espera debajo del texto. Usarlo (en vez de un gris
/// propio) es lo que hace que los colores se vean como en un editor de verdad,
/// porque están elegidos para ese fondo.
pub fn theme_background() -> Color32 {
    theme()
        .settings
        .background
        .map(syntect_color)
        .unwrap_or(Color32::from_rgb(30, 30, 46))
}

/// Color del texto sin token asignado (y del fallback mientras no llegó el
/// resaltado).
pub fn theme_foreground() -> Color32 {
    theme()
        .settings
        .foreground
        .map(syntect_color)
        .unwrap_or(Color32::from_rgb(205, 214, 244))
}

pub fn syntect_color(color: syntect::highlighting::Color) -> Color32 {
    Color32::from_rgb(color.r, color.g, color.b)
}

/// Elige la gramática por extensión y, si no hay, por la primera línea
/// (shebangs tipo `#!/bin/bash`). Devuelve el nombre del lenguaje detectado.
pub fn detect_language(file_name: &str, first_line: &str) -> Option<String> {
    let set = syntax_set();
    let extension = file_name.rsplit('.').next().unwrap_or_default();
    // Un archivo sin punto (`Makefile`) no tiene extensión real: `rsplit` en ese
    // caso devuelve el nombre entero, que igual sirve para buscar por token.
    if !extension.is_empty() {
        if let Some(syntax) = set.find_syntax_by_extension(extension) {
            return Some(syntax.name.clone());
        }
    }
    if let Some(syntax) = set.find_syntax_by_token(file_name) {
        return Some(syntax.name.clone());
    }
    set.find_syntax_by_first_line(first_line)
        .map(|syntax| syntax.name.clone())
}

/// Resalta el texto completo. Pensado para correr fuera del hilo de UI.
pub fn highlight_text(file_name: &str, text: &str) -> Vec<HighlightedLine> {
    let set = syntax_set();
    let first_line = text.lines().next().unwrap_or_default();
    let extension = file_name.rsplit('.').next().unwrap_or_default();
    let syntax = set
        .find_syntax_by_extension(extension)
        .or_else(|| set.find_syntax_by_token(file_name))
        .or_else(|| set.find_syntax_by_first_line(first_line))
        .unwrap_or_else(|| set.find_syntax_plain_text());

    let mut highlighter = HighlightLines::new(syntax, theme());
    let mut out = Vec::new();
    for line in text.lines().take(MAX_HIGHLIGHT_LINES) {
        match highlighter.highlight_line(line, set) {
            Ok(ranges) => out.push(
                ranges
                    .into_iter()
                    .map(|(style, piece)| (syntect_color(style.foreground), piece.to_owned()))
                    .collect(),
            ),
            // Si una línea falla (regex patológica), sigue en texto plano en
            // vez de tirar abajo el resaltado del archivo entero.
            Err(_) => out.push(vec![(theme_foreground(), line.to_owned())]),
        }
    }
    out
}

pub struct HighlightRequest {
    pub token: u64,
    pub file_name: String,
    pub text: String,
}

pub struct HighlightResult {
    /// Identifica a qué apertura corresponde: si el usuario abrió otro archivo
    /// mientras se resaltaba, el resultado viejo se descarta.
    pub token: u64,
    pub lines: Vec<HighlightedLine>,
}

/// Worker de resaltado. Un único hilo con cola: alcanza porque sólo hay un
/// visor abierto a la vez.
pub struct Highlighter {
    tx: Sender<HighlightRequest>,
    rx: Receiver<HighlightResult>,
    next_token: u64,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    pub fn new() -> Self {
        let (tx, job_rx) = std::sync::mpsc::channel::<HighlightRequest>();
        let (result_tx, rx) = std::sync::mpsc::channel::<HighlightResult>();
        std::thread::Builder::new()
            .name("code-highlighter".to_owned())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let lines = highlight_text(&job.file_name, &job.text);
                    if result_tx
                        .send(HighlightResult {
                            token: job.token,
                            lines,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .ok();
        Self {
            tx,
            rx,
            next_token: 0,
        }
    }

    /// Encola un archivo y devuelve el token con el que reconocer su resultado.
    pub fn request(&mut self, file_name: String, text: String) -> u64 {
        self.next_token = self.next_token.wrapping_add(1);
        let token = self.next_token;
        let _ = self.tx.send(HighlightRequest {
            token,
            file_name,
            text,
        });
        token
    }

    pub fn poll(&mut self) -> Option<HighlightResult> {
        self.rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_language, highlight_text, Highlighter, MAX_HIGHLIGHT_LINES};

    fn joined(line: &[(egui::Color32, String)]) -> String {
        line.iter().map(|(_, text)| text.as_str()).collect()
    }

    #[test]
    fn detects_language_by_extension() {
        assert_eq!(detect_language("main.rs", ""), Some("Rust".to_owned()));
        assert_eq!(detect_language("app.py", ""), Some("Python".to_owned()));
        assert_eq!(detect_language("index.json", ""), Some("JSON".to_owned()));
    }

    #[test]
    fn detects_language_by_shebang_when_there_is_no_extension() {
        let detected = detect_language("deploy", "#!/bin/bash\necho hi\n");
        assert!(
            detected
                .as_deref()
                .is_some_and(|name| name.contains("Bash") || name.contains("Shell")),
            "got {detected:?}"
        );
    }

    #[test]
    fn unknown_extension_is_not_a_hard_error() {
        // No debe entrar en pánico ni inventar un lenguaje raro.
        let _ = detect_language("thing.zzzz", "contenido cualquiera");
    }

    #[test]
    fn highlighting_preserves_the_text_exactly() {
        // Lo más importante: colorear no puede alterar ni perder caracteres.
        let source = "fn main() {\n    let x = 42; // nota\n}\n";
        let lines = highlight_text("main.rs", source);
        let rebuilt: Vec<String> = lines.iter().map(|line| joined(line)).collect();
        assert_eq!(rebuilt, vec!["fn main() {", "    let x = 42; // nota", "}"]);
    }

    #[test]
    fn a_comment_marker_inside_a_string_is_not_treated_as_a_comment() {
        let lines = highlight_text("main.rs", "let s = \"// no es comentario\";\n");
        let line = &lines[0];
        let comment_text = "// no es comentario";
        let span = line
            .iter()
            .find(|(_, text)| text.contains(comment_text))
            .expect("the string body must be present");
        // El color del cuerpo del string tiene que diferir del de un comentario
        // real en la misma gramática.
        let comment_lines = highlight_text("main.rs", "// comentario\n");
        let comment_color = comment_lines[0]
            .iter()
            .find(|(_, text)| text.contains("comentario"))
            .expect("comment span")
            .0;
        assert_ne!(
            span.0, comment_color,
            "a string body was coloured like a comment"
        );
    }

    #[test]
    fn keywords_and_plain_text_get_different_colours() {
        let lines = highlight_text("main.rs", "fn nombre() {}\n");
        let colors: Vec<_> = lines[0].iter().map(|(color, _)| *color).collect();
        assert!(
            colors.iter().any(|color| *color != colors[0]),
            "everything came out the same colour: {colors:?}"
        );
    }

    #[test]
    fn plain_text_files_still_produce_one_span_per_line() {
        let lines = highlight_text("notas.txt", "primera\nsegunda\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(joined(&lines[0]), "primera");
        assert_eq!(joined(&lines[1]), "segunda");
    }

    #[test]
    fn empty_input_yields_no_lines() {
        assert!(highlight_text("main.rs", "").is_empty());
    }

    #[test]
    fn line_count_is_capped() {
        let source = "let x = 1;\n".repeat(MAX_HIGHLIGHT_LINES + 500);
        assert_eq!(
            highlight_text("main.rs", &source).len(),
            MAX_HIGHLIGHT_LINES
        );
    }

    #[test]
    fn worker_returns_the_highlighted_file_with_its_token() {
        let mut highlighter = Highlighter::new();
        let token = highlighter.request("main.rs".to_owned(), "fn main() {}\n".to_owned());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let result = loop {
            if let Some(result) = highlighter.poll() {
                break result;
            }
            assert!(std::time::Instant::now() < deadline, "worker timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };

        assert_eq!(result.token, token);
        assert_eq!(joined(&result.lines[0]), "fn main() {}");
    }

    #[test]
    fn tokens_increase_so_a_stale_result_can_be_discarded() {
        let mut highlighter = Highlighter::new();
        let first = highlighter.request("a.rs".to_owned(), "fn a() {}\n".to_owned());
        let second = highlighter.request("b.rs".to_owned(), "fn b() {}\n".to_owned());
        assert_ne!(first, second);
    }
    #[test]
    fn the_languages_this_app_is_used_with_are_all_covered() {
        // Regresión: el set por defecto de syntect no trae TypeScript, TSX,
        // JSX ni TOML, y por eso un .ts salía enteramente gris.
        for (file, expected) in [
            ("tailwind.config.ts", "TypeScript"),
            ("page.tsx", "TypeScriptReact"),
            ("Cargo.toml", "TOML"),
            ("main.rs", "Rust"),
            ("app.py", "Python"),
            ("index.js", "JavaScript"),
            ("data.json", "JSON"),
            ("README.md", "Markdown"),
            ("styles.css", "CSS"),
            ("main.go", "Go"),
            ("deploy.yaml", "YAML"),
        ] {
            let detected = detect_language(file, "");
            assert_eq!(
                detected.as_deref(),
                Some(expected),
                "{file} should be detected as {expected}"
            );
        }
    }

    #[test]
    fn a_typescript_file_actually_gets_several_colours() {
        // El síntoma reportado era "no tiene colores": este test lo cubre.
        let source = "import type { Config } from \"tailwindcss\";\nconst config: Config = { plugins: [] };\n";
        let lines = highlight_text("tailwind.config.ts", source);
        let colors: std::collections::BTreeSet<[u8; 4]> = lines
            .iter()
            .flatten()
            .map(|(color, _)| color.to_array())
            .collect();
        assert!(
            colors.len() >= 4,
            "a TS file should use several colours, got {}",
            colors.len()
        );
    }

    #[test]
    fn theme_background_and_foreground_differ_enough_to_read() {
        let bg = super::theme_background();
        let fg = super::theme_foreground();
        let delta = (bg.r() as i32 - fg.r() as i32).abs()
            + (bg.g() as i32 - fg.g() as i32).abs()
            + (bg.b() as i32 - fg.b() as i32).abs();
        assert!(delta > 150, "insufficient contrast: bg={bg:?} fg={fg:?}");
    }
}
