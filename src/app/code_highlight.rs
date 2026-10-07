//! Resaltado de sintaxis para el visor de código, con `syntect` (las mismas
//! gramáticas TextMate que usan VS Code y Sublime).
//!
//! El resaltado corre en un **worker thread**: medido en release tarda ~90 ms
//! cada 2000 líneas, y en debug bastante más, así que hacerlo en el hilo de UI
//! congelaría el frame al abrir un archivo. El visor muestra el texto plano al
//! instante y cambia a la versión coloreada cuando el worker la entrega.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use egui::Color32;
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::{SyntaxReference, SyntaxSet};
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
static THEME: OnceLock<Theme> = OnceLock::new();

fn theme() -> &'static Theme {
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
    THEME
        .get()
        .and_then(|theme| theme.settings.background)
        .map(syntect_color)
        .unwrap_or(Color32::from_rgb(30, 30, 46))
}

/// Color del texto sin token asignado (y del fallback mientras no llegó el
/// resaltado).
pub fn theme_foreground() -> Color32 {
    THEME
        .get()
        .and_then(|theme| theme.settings.foreground)
        .map(syntect_color)
        .unwrap_or(Color32::from_rgb(205, 214, 244))
}

pub fn syntect_color(color: syntect::highlighting::Color) -> Color32 {
    Color32::from_rgb(color.r, color.g, color.b)
}

/// Elige la gramática por extensión y, si no hay, por la primera línea
/// (shebangs tipo `#!/bin/bash`). Devuelve el nombre del lenguaje detectado.
pub fn detect_language(file_name: &str, first_line: &str) -> Option<String> {
    select_syntax(syntax_set(), file_name, first_line).map(|syntax| {
        if file_name
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("jsx"))
            && syntax.name == "TypeScriptReact"
        {
            "JSX".to_owned()
        } else {
            syntax.name.clone()
        }
    })
}

/// Keep the viewer's language indicator and its parser on the same grammar.
pub(crate) fn select_syntax<'a>(
    set: &'a SyntaxSet,
    file_name: &str,
    first_line: &str,
) -> Option<&'a SyntaxReference> {
    let extension = file_name.rsplit('.').next().unwrap_or_default();
    // Un archivo sin punto (`Makefile`) no tiene extensión real: `rsplit` en ese
    // caso devuelve el nombre entero, que igual sirve para buscar por token.
    if !extension.is_empty() {
        if let Some(syntax) = set.find_syntax_by_extension(extension) {
            return Some(syntax);
        }
    }
    // The fancy-regex assets exclude Babel's extra extensions. JSX uses the
    // embedded React grammar; module extensions use the same grammar as .js.
    // Native extension definitions remain authoritative when assets change.
    if file_name.contains('.') {
        let alias = if extension.eq_ignore_ascii_case("jsx") {
            Some("TypeScriptReact")
        } else if extension.eq_ignore_ascii_case("mjs") || extension.eq_ignore_ascii_case("cjs") {
            Some("JavaScript")
        } else {
            None
        };
        if let Some(syntax) = alias.and_then(|name| set.find_syntax_by_name(name)) {
            return Some(syntax);
        }
    }
    if let Some(syntax) = set.find_syntax_by_token(file_name) {
        return Some(syntax);
    }
    set.find_syntax_by_first_line(first_line)
}

/// Resalta el texto completo. Pensado para correr fuera del hilo de UI.
pub fn highlight_text(file_name: &str, text: &str) -> Vec<HighlightedLine> {
    highlight_text_cancelable(file_name, text, &|| false).unwrap_or_default()
}

/// Cancelar entre líneas conserva el estado multilínea de la gramática. No
/// interrumpe una regex que ya está ejecutándose sobre la línea actual.
fn highlight_text_cancelable(
    file_name: &str,
    text: &str,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<HighlightedLine>> {
    if cancelled() {
        return None;
    }
    let set = syntax_set();
    let first_line = text.lines().next().unwrap_or_default();
    let syntax =
        select_syntax(set, file_name, first_line).unwrap_or_else(|| set.find_syntax_plain_text());

    let mut highlighter = HighlightLines::new(syntax, theme());
    let mut out = Vec::new();
    // Las gramáticas de extra_newlines consumen los terminadores para cerrar
    // comentarios y strings de una sola línea. El parser recibe el texto real;
    // sólo la representación visible omite LF/CRLF, igual que str::lines().
    for line in text.split_inclusive('\n').take(MAX_HIGHLIGHT_LINES) {
        if cancelled() {
            return None;
        }
        let visible_line = line
            .strip_suffix('\n')
            .map(|without_lf| without_lf.strip_suffix('\r').unwrap_or(without_lf))
            .unwrap_or(line);
        match highlighter.highlight_line(line, set) {
            Ok(ranges) => {
                let mut spans = Vec::new();
                let mut offset = 0;
                for (style, piece) in ranges {
                    // El terminador puede estar en otro tramo, o compartirlo
                    // con código. Recortar por posición evita quitar caracteres
                    // de contenido, como un CR sin LF al final del archivo.
                    let visible_len = visible_line.len().saturating_sub(offset).min(piece.len());
                    if visible_len > 0 {
                        spans.push((
                            syntect_color(style.foreground),
                            piece[..visible_len].to_owned(),
                        ));
                    }
                    offset += piece.len();
                }
                out.push(spans);
            }
            // Si una línea falla (regex patológica), sigue en texto plano en
            // vez de tirar abajo el resaltado del archivo entero.
            Err(_) => out.push(vec![(theme_foreground(), visible_line.to_owned())]),
        }
    }
    (!cancelled()).then_some(out)
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

struct HighlightJob {
    request: HighlightRequest,
    notify: Box<dyn FnOnce() + Send>,
}

#[derive(Default)]
struct HighlightState {
    /// Como máximo un pedido pendiente además del trabajo en curso.
    pending: Option<HighlightJob>,
    /// Como máximo un resultado, siempre del último token solicitado.
    completed: Option<HighlightResult>,
}

struct HighlightShared {
    state: Mutex<HighlightState>,
    ready: Condvar,
    latest_token: AtomicU64,
    closed: AtomicBool,
}

impl HighlightShared {
    fn cancelled(&self, token: u64) -> bool {
        self.closed.load(Ordering::Acquire) || self.latest_token.load(Ordering::Acquire) != token
    }
}

/// También marca el worker como no disponible si una operación entra en
/// pánico: los siguientes pedidos pueden seguir mostrando texto plano.
struct WorkerExitGuard(Arc<HighlightShared>);

impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.0.closed.store(true, Ordering::Release);
        let pending = state.pending.take();
        drop(state);
        self.0.ready.notify_all();
        drop(pending);
    }
}

/// Un único worker y dos slots reemplazables evitan acumular archivos o
/// resultados obsoletos cuando el usuario cambia de archivo rápidamente.
pub struct Highlighter {
    shared: Arc<HighlightShared>,
    next_token: u64,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    pub fn new() -> Self {
        Self::with_processor(|request, cancelled| {
            highlight_text_cancelable(&request.file_name, &request.text, cancelled)
        })
    }

    fn with_processor<F>(process: F) -> Self
    where
        F: Fn(&HighlightRequest, &dyn Fn() -> bool) -> Option<Vec<HighlightedLine>>
            + Send
            + 'static,
    {
        let shared = Arc::new(HighlightShared {
            state: Mutex::new(HighlightState::default()),
            ready: Condvar::new(),
            latest_token: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("code-highlighter".to_owned())
            .spawn(move || {
                let _exit = WorkerExitGuard(Arc::clone(&worker));
                loop {
                    let mut state = worker
                        .state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    while state.pending.is_none() && !worker.closed.load(Ordering::Acquire) {
                        state = worker
                            .ready
                            .wait(state)
                            .unwrap_or_else(|error| error.into_inner());
                    }
                    if worker.closed.load(Ordering::Acquire) {
                        return;
                    }
                    let job = state.pending.take().expect("pending highlight");
                    drop(state);

                    let cancelled = || worker.cancelled(job.request.token);
                    if cancelled() {
                        continue;
                    }
                    let Some(lines) = process(&job.request, &cancelled) else {
                        continue;
                    };
                    let mut state = worker
                        .state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if cancelled() {
                        continue;
                    }
                    let replaced = state.completed.replace(HighlightResult {
                        token: job.request.token,
                        lines,
                    });
                    drop(state);
                    drop(replaced);
                    // El resultado se publica antes de despertar a la UI.
                    // Una apertura nueva puede reemplazarlo sin bloquearse.
                    (job.notify)();
                }
            })
            .is_ok();
        if !spawned {
            shared.closed.store(true, Ordering::Release);
        }
        Self {
            shared,
            next_token: 0,
        }
    }

    /// Encola un archivo y devuelve el token con el que reconocer su resultado.
    pub fn request(&mut self, file_name: String, text: String) -> u64 {
        self.request_with_notify(file_name, text, || {})
    }

    /// El callback despierta al consumidor cuando el resultado actual está
    /// listo. No hay una cola de callbacks ni un envío que pueda bloquearse.
    pub fn request_with_notify(
        &mut self,
        file_name: String,
        text: String,
        notify: impl FnOnce() + Send + 'static,
    ) -> u64 {
        self.next_token = self.next_token.wrapping_add(1);
        let token = self.next_token;
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.shared.latest_token.store(token, Ordering::Release);
        let completed = state.completed.take();
        let pending = state.pending.take();
        if !self.shared.closed.load(Ordering::Acquire) {
            state.pending = Some(HighlightJob {
                request: HighlightRequest {
                    token,
                    file_name,
                    text,
                },
                notify: Box::new(notify),
            });
        }
        drop(state);
        self.shared.ready.notify_one();
        // Desalocar archivos grandes fuera del mutex mantiene breve el acceso
        // al slot compartido desde el hilo de UI.
        drop(pending);
        drop(completed);
        token
    }

    pub fn is_available(&self) -> bool {
        !self.shared.closed.load(Ordering::Acquire)
    }

    /// Una apertura nueva o cerrar el visor invalida también el trabajo que
    /// ya estaba en curso, además de liberar el pedido y resultado pendientes.
    pub fn cancel(&mut self) {
        self.next_token = self.next_token.wrapping_add(1);
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.shared
            .latest_token
            .store(self.next_token, Ordering::Release);
        let pending = state.pending.take();
        let completed = state.completed.take();
        drop(state);
        drop(pending);
        drop(completed);
    }

    pub fn poll(&mut self) -> Option<HighlightResult> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state
            .completed
            .take()
            .filter(|result| result.token == self.shared.latest_token.load(Ordering::Acquire))
    }
}

impl Drop for Highlighter {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.shared.closed.store(true, Ordering::Release);
        let pending = state.pending.take();
        let completed = state.completed.take();
        drop(state);
        self.shared.ready.notify_all();
        // No esperar una regex activa en el hilo de UI. El único worker sale
        // cooperativamente al terminar la línea que estaba procesando.
        drop(pending);
        drop(completed);
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_language, highlight_text, Highlighter, MAX_HIGHLIGHT_LINES};

    const WORKER_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

    fn plain_result(text: &str) -> Vec<super::HighlightedLine> {
        vec![vec![(egui::Color32::WHITE, text.to_owned())]]
    }

    fn joined(line: &[(egui::Color32, String)]) -> String {
        line.iter().map(|(_, text)| text.as_str()).collect()
    }

    fn coloured_characters(lines: &[super::HighlightedLine]) -> Vec<Vec<([u8; 4], char)>> {
        lines
            .iter()
            .map(|line| {
                line.iter()
                    .flat_map(|(color, text)| {
                        text.chars().map(|character| (color.to_array(), character))
                    })
                    .collect()
            })
            .collect()
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
    fn highlighting_preserves_visible_lines_for_lf_crlf_unicode_and_eof() {
        for source in [
            "",
            "\n",
            "\r\n",
            "\n\n",
            "const título = \"café 🐈\";\r\n\r\nconst final = 3;",
            "const título = \"café 🐈\";\n\nconst final = 3;\n",
            "sin terminador",
            "un CR de contenido al final\r",
            "un CR\ren medio\n",
        ] {
            let highlighted = highlight_text("example.js", source);
            let rebuilt: Vec<String> = highlighted.iter().map(|line| joined(line)).collect();
            let expected: Vec<String> = source.lines().map(str::to_owned).collect();
            assert_eq!(rebuilt, expected, "source: {source:?}");
        }
    }

    #[test]
    fn a_javascript_line_comment_does_not_colour_the_next_line() {
        let code = "const value = 42;";
        for ending in ["\n", "\r\n"] {
            let source = format!("// comentario{ending}{code}{ending}");
            let lines = highlight_text("example.js", &source);
            let independent = highlight_text("example.js", &format!("{code}{ending}"));
            assert_eq!(
                lines[1], independent[0],
                "comment state leaked past {ending:?}"
            );
        }
    }

    #[test]
    fn an_unclosed_javascript_string_ends_at_the_line_ending() {
        let code = "const value = 42;";
        for quote in ["'", "\""] {
            for ending in ["\n", "\r\n"] {
                let source = format!("{quote}sin cerrar{ending}{code}");
                let lines = highlight_text("example.js", &source);
                let independent = highlight_text("example.js", code);
                assert_eq!(
                    lines[1], independent[0],
                    "string state leaked past {ending:?} with quote {quote:?}"
                );
            }
        }
    }

    #[test]
    fn a_javascript_template_string_keeps_its_multiline_context() {
        let code = "const value = 42;";
        for ending in ["\n", "\r\n"] {
            let source = format!("const message = `uno{ending}{code}{ending}`;{ending}{code}");
            let lines = highlight_text("example.js", &source);
            let independent = highlight_text("example.js", code);
            assert_ne!(
                lines[1], independent[0],
                "a real multiline string lost its context at {ending:?}"
            );
            assert_eq!(
                lines[3], independent[0],
                "template string state leaked past the closing backtick"
            );
        }
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
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let token = highlighter.request_with_notify(
            "main.rs".to_owned(),
            "fn main() {}\n".to_owned(),
            move || {
                let _ = ready_tx.send(());
            },
        );
        ready_rx
            .recv_timeout(WORKER_DEADLINE)
            .expect("worker ready");
        let result = highlighter.poll().expect("published before notification");

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
    fn the_latest_pending_request_replaces_older_jobs_without_notifying_them() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let processed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let processed_worker = std::sync::Arc::clone(&processed);
        let obsolete_notifications = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut highlighter = Highlighter::with_processor(move |request, cancelled| {
            processed_worker.lock().unwrap().push(request.token);
            if request.file_name == "blocked" {
                let _ = entered_tx.send(());
                release_rx.recv().expect("release first job");
                assert!(
                    cancelled(),
                    "the superseded active job must see cancellation"
                );
                return None;
            }
            Some(plain_result(&request.text))
        });
        let first_notifications = std::sync::Arc::clone(&obsolete_notifications);
        let first =
            highlighter.request_with_notify("blocked".to_owned(), "first".to_owned(), move || {
                first_notifications.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            });
        entered_rx
            .recv_timeout(WORKER_DEADLINE)
            .expect("active job");
        for index in 0..128 {
            let notifications = std::sync::Arc::clone(&obsolete_notifications);
            highlighter.request_with_notify("old".to_owned(), index.to_string(), move || {
                notifications.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            });
        }
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let latest =
            highlighter.request_with_notify("latest".to_owned(), "final".to_owned(), move || {
                let _ = ready_tx.send(());
            });
        {
            let state = highlighter.shared.state.lock().unwrap();
            assert_eq!(state.pending.as_ref().unwrap().request.token, latest);
            assert!(state.completed.is_none());
        }
        assert_eq!(*processed.lock().unwrap(), vec![first]);
        release_tx.send(()).expect("release active job");
        ready_rx
            .recv_timeout(WORKER_DEADLINE)
            .expect("latest ready");
        let result = highlighter.poll().expect("latest result");
        assert_eq!(result.token, latest);
        assert_eq!(joined(&result.lines[0]), "final");
        assert_eq!(*processed.lock().unwrap(), vec![first, latest]);
        assert_eq!(
            obsolete_notifications.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert!(highlighter.poll().is_none());
    }

    #[test]
    fn a_new_request_discards_an_unpolled_result_before_starting() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut highlighter = Highlighter::with_processor(move |request, _| {
            if request.file_name == "blocked" {
                let _ = entered_tx.send(());
                release_rx.recv().expect("release new job");
            }
            Some(plain_result(&request.text))
        });
        let (old_tx, old_rx) = std::sync::mpsc::channel();
        let old = highlighter.request_with_notify("old".to_owned(), "old".to_owned(), move || {
            let _ = old_tx.send(());
        });
        old_rx
            .recv_timeout(WORKER_DEADLINE)
            .expect("old result ready");
        assert_eq!(
            highlighter
                .shared
                .state
                .lock()
                .unwrap()
                .completed
                .as_ref()
                .unwrap()
                .token,
            old
        );

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let latest =
            highlighter.request_with_notify("blocked".to_owned(), "latest".to_owned(), move || {
                let _ = ready_tx.send(());
            });
        entered_rx
            .recv_timeout(WORKER_DEADLINE)
            .expect("new job active");
        assert!(
            highlighter.poll().is_none(),
            "the old result must already be gone"
        );
        release_tx.send(()).expect("release new job");
        ready_rx
            .recv_timeout(WORKER_DEADLINE)
            .expect("latest result ready");
        assert_eq!(highlighter.poll().unwrap().token, latest);
        assert!(highlighter.poll().is_none());
    }

    #[test]
    fn cancelling_clears_queued_work_and_allows_a_later_request() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        let mut highlighter = Highlighter::with_processor(move |request, cancelled| {
            if request.file_name == "blocked" {
                let _ = entered_tx.send(());
                release_rx.recv().expect("release cancelled job");
                let _ = cancelled_tx.send(cancelled());
                return None;
            }
            Some(plain_result(&request.text))
        });
        highlighter.request("blocked".to_owned(), "first".to_owned());
        entered_rx
            .recv_timeout(WORKER_DEADLINE)
            .expect("first active");
        highlighter.request("queued".to_owned(), "obsolete".to_owned());
        highlighter.cancel();
        {
            let state = highlighter.shared.state.lock().unwrap();
            assert!(state.pending.is_none());
            assert!(state.completed.is_none());
        }
        release_tx.send(()).expect("release cancelled job");
        assert!(cancelled_rx
            .recv_timeout(WORKER_DEADLINE)
            .expect("observed cancellation"));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let latest =
            highlighter.request_with_notify("new".to_owned(), "usable".to_owned(), move || {
                let _ = ready_tx.send(());
            });
        ready_rx
            .recv_timeout(WORKER_DEADLINE)
            .expect("new result ready");
        let result = highlighter.poll().expect("new result");
        assert_eq!(result.token, latest);
        assert_eq!(joined(&result.lines[0]), "usable");
    }

    #[test]
    fn dropping_the_highlighter_cancels_without_joining_an_active_operation() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        let mut highlighter = Highlighter::with_processor(move |_, cancelled| {
            let _ = entered_tx.send(());
            release_rx.recv().expect("release active operation");
            let _ = cancelled_tx.send(cancelled());
            None
        });
        highlighter.request("blocked".to_owned(), "text".to_owned());
        entered_rx
            .recv_timeout(WORKER_DEADLINE)
            .expect("active operation");
        let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(highlighter);
            let _ = dropped_tx.send(());
        });
        let dropped_without_release = dropped_rx.recv_timeout(WORKER_DEADLINE);
        // Liberar aun si falla la comprobación evita dejar el fixture trabado.
        let _ = release_tx.send(());
        dropper.join().expect("drop thread");
        dropped_without_release.expect("Drop must return before the operation is released");
        assert!(cancelled_rx
            .recv_timeout(WORKER_DEADLINE)
            .expect("Drop cancellation"));
    }

    #[test]
    fn syntax_work_checks_for_cancellation_between_lines() {
        let checks = std::cell::Cell::new(0);
        let cancelled = || {
            let count = checks.get();
            checks.set(count + 1);
            count >= 2
        };
        let result = super::highlight_text_cancelable(
            "example.js",
            "// first\nconst second = 42;\nconst third = 3;\n",
            &cancelled,
        );
        assert!(result.is_none());
        assert_eq!(checks.get(), 3, "cancelled before parsing the second line");
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
    fn jsx_keeps_markup_expressions_and_following_code_colours() {
        let lf = "// café 🐈 界 e\u{301}\nconst Card = ({ title }) => (\n  <section data-count={42}>\n    <strong>{title || \"hola 👋\"}</strong>\n  </section>\n);\nconst after = 3; // comentario";
        for source in [
            lf.to_owned(),
            lf.replace('\n', "\r\n"),
            lf.replacen('\n', "\r\n", 2),
        ] {
            let reference = highlight_text("Card.tsx", &source);
            let colors: std::collections::BTreeSet<_> = reference
                .iter()
                .flatten()
                .map(|(color, _)| color.to_array())
                .collect();
            assert!(
                colors.len() >= 4,
                "JSX markup must have distinct syntax colours"
            );
            for file in ["Card.jsx", "Card.JSX"] {
                assert_eq!(detect_language(file, "").as_deref(), Some("JSX"));
                let actual = highlight_text(file, &source);
                assert_eq!(
                    coloured_characters(&actual),
                    coloured_characters(&reference),
                    "{file}"
                );
                assert_eq!(
                    actual.iter().map(|line| joined(line)).collect::<Vec<_>>(),
                    source.lines().collect::<Vec<_>>(),
                    "{file}"
                );
            }
        }
    }

    #[test]
    fn javascript_modules_keep_javascript_colours_and_text() {
        for (file, source) in [
            ("app.mjs", "import { answer } from \"./dep.mjs\";\nexport const message = `primera\ncafé 🐈 界 e\u{301} ${answer + 42}`;\n"),
            ("app.cjs", "const { answer } = require(\"./dep.cjs\");\n/* café 🐈 界 e\u{301}\n   comentario */\nmodule.exports = { answer: answer + 42 };"),
        ] {
            for source in [source.to_owned(), source.replace('\n', "\r\n"), source.replacen('\n', "\r\n", 1)] {
                let reference = highlight_text("app.js", &source);
                for file in [file.to_owned(), file.to_uppercase()] {
                    assert_eq!(detect_language(&file, "").as_deref(), Some("JavaScript"));
                    let actual = highlight_text(&file, &source);
                    assert_eq!(coloured_characters(&actual), coloured_characters(&reference), "{file}");
                    assert_eq!(actual.iter().map(|line| joined(line)).collect::<Vec<_>>(), source.lines().collect::<Vec<_>>(), "{file}");
                }
            }
        }
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
