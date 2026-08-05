//! Búsqueda en el scrollback (regex vía `alacritty_terminal`). El flujo lo
//! maneja el panel: compilar la consulta, encontrar el próximo match desde el
//! match actual (con wrap-around al inicio del historial) y revelar la línea.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::term::search::{Match, RegexSearch};
use alacritty_terminal::term::Term;

use crate::terminal::pty::EventProxy;

/// Máximo de líneas que cubre el resaltado de un match: los regex tipo `.*`
/// pueden abarcar el scrollback entero y no tiene sentido dibujar miles de
/// rects.
pub const MAX_HIGHLIGHT_LINES: usize = 200;

pub struct SearchQuery {
    regex: RegexSearch,
    pub raw: String,
}

impl SearchQuery {
    /// Compila la consulta como regex; si no es un regex válido la escapa y
    /// la trata como literal (comportamiento de búsqueda de texto común).
    pub fn compile(query: &str) -> Option<SearchQuery> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return None;
        }
        let regex = RegexSearch::new(trimmed)
            .or_else(|_| RegexSearch::new(&regex_escape(trimmed)))
            .ok()?;
        Some(SearchQuery {
            regex,
            raw: trimmed.to_owned(),
        })
    }
}

fn regex_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() * 2);
    for ch in text.chars() {
        if matches!(
            ch,
            '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|'
        ) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// Primer punto del buffer completo (fondo del historial).
fn history_start(term: &Term<EventProxy>) -> Point {
    Point::new(Line(-(term.grid().history_size() as i32)), Column(0))
}

/// El punto está dentro del grid (historial + pantalla activa). Los puntos
/// guardados pueden quedar fuera tras resizes; usarlos de origen de búsqueda
/// sin validar puede paniquear al indexar el grid.
fn point_in_grid(term: &Term<EventProxy>, point: Point) -> bool {
    let top = -(term.grid().history_size() as i32);
    let bottom = term.screen_lines() as i32;
    point.line.0 >= top && point.line.0 < bottom && point.column.0 < term.columns()
}

/// Celda siguiente al final de un match (para buscar "el próximo"); `None`
/// si el match termina en la última celda del grid.
fn point_after(term: &Term<EventProxy>, point: Point) -> Option<Point> {
    let last_col = term.columns() - 1;
    let next = if point.column.0 < last_col {
        Point::new(point.line, Column(point.column.0 + 1))
    } else {
        Point::new(Line(point.line.0 + 1), Column(0))
    };
    point_in_grid(term, next).then_some(next)
}

/// Próximo match hacia adelante desde `after` (o desde el fondo del
/// historial), con wrap-around: si no hay nada después del cursor, vuelve a
/// buscar desde el inicio del scrollback.
pub fn find_next(
    term: &Term<EventProxy>,
    query: &mut SearchQuery,
    after: Option<Point>,
) -> Option<Match> {
    let mut from_origin = |term: &Term<EventProxy>, origin: Point| {
        term.search_next(&mut query.regex, origin, Direction::Right, Side::Left, None)
    };
    match after.filter(|point| point_in_grid(term, *point)) {
        Some(point) => match point_after(term, point) {
            Some(origin) => {
                from_origin(term, origin).or_else(|| from_origin(term, history_start(term)))
            }
            None => from_origin(term, history_start(term)),
        },
        None => from_origin(term, history_start(term)),
    }
}

/// Display offset que revela la línea del match, anclada al tercio inferior
/// de la pantalla. Devuelve `None` si ya está visible.
///
/// Coordenadas de grid: la pantalla activa ocupa `Line(0)..Line(rows)` y el
/// historial las líneas negativas; `viewport_row = line + display_offset`.
pub fn display_offset_for_match(
    match_start: Point,
    current_offset: usize,
    visible_rows: usize,
    history_size: usize,
) -> Option<usize> {
    let line = match_start.line.0;
    let rows = visible_rows as i32;
    let row_now = line + current_offset as i32;
    if (0..rows).contains(&row_now) {
        return None;
    }
    let target_row = (rows * 2) / 3;
    let target = (target_row - line).max(0) as usize;
    Some(target.min(history_size))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config as TermConfig, Term};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use super::{display_offset_for_match, find_next, regex_escape, SearchQuery};
    use crate::terminal::pty::EventProxy;

    fn sample_term(text: &str) -> Term<EventProxy> {
        let (event_tx, _event_rx) = mpsc::channel();
        let mut term = Term::new(
            TermConfig::default(),
            &TermSize::new(40, 10),
            EventProxy::new(event_tx),
        );
        let mut processor = Processor::<StdSyncHandler>::new();
        processor.advance(&mut term, text.as_bytes());
        term
    }

    #[test]
    fn compile_rejects_empty_query() {
        assert!(SearchQuery::compile("").is_none());
        assert!(SearchQuery::compile("   ").is_none());
    }

    #[test]
    fn compile_falls_back_to_literal_for_invalid_regex() {
        let query = SearchQuery::compile("file[0").expect("literal fallback");
        assert_eq!(query.raw, "file[0");
    }

    #[test]
    fn regex_escape_neutralizes_metacharacters() {
        assert_eq!(regex_escape("a.b[c]"), "a\\.b\\[c\\]");
        assert_eq!(regex_escape("plain"), "plain");
    }

    #[test]
    fn find_next_locates_matches_and_wraps_around() {
        let term = sample_term("alpha beta\nalpha gamma");
        let mut query = SearchQuery::compile("alpha").unwrap();

        let first = find_next(&term, &mut query, None).expect("first match");
        assert_eq!(first.start().column.0, 0);

        let second = find_next(&term, &mut query, Some(*first.end())).expect("second match");
        assert_ne!(first.start(), second.start());

        // Después del último match vuelve al primero (wrap-around).
        let wrapped = find_next(&term, &mut query, Some(*second.end())).expect("wrap");
        assert_eq!(wrapped.start(), first.start());
    }

    #[test]
    fn find_next_returns_none_without_matches() {
        let term = sample_term("hello world");
        let mut query = SearchQuery::compile("zzz").unwrap();
        assert!(find_next(&term, &mut query, None).is_none());
    }

    #[test]
    fn find_next_ignores_stale_out_of_bounds_origin() {
        use alacritty_terminal::index::{Column, Line, Point};
        let term = sample_term("alpha beta");
        let mut query = SearchQuery::compile("alpha").unwrap();
        // Un origen fuera del grid (p. ej. tras un resize) no debe paniquear:
        // se trata como búsqueda desde el inicio.
        let stale = Point::new(Line(-9999), Column(0));
        let found = find_next(&term, &mut query, Some(stale)).expect("no panic");
        assert_eq!(found.start().column.0, 0);
    }

    #[test]
    fn display_offset_keeps_visible_lines_in_place() {
        // Línea ya visible: no scroll.
        assert_eq!(display_offset_for_point_line(0, 0, 20, 100), None);
        assert_eq!(display_offset_for_point_line(-5, 5, 20, 100), None);
        assert_eq!(display_offset_for_point_line(-5, 12, 20, 100), None);
    }

    #[test]
    fn display_offset_scrolls_to_history_matches() {
        // Línea 30 en el historial, pantalla de 20 filas: el target la deja
        // en el tercio inferior (fila 13): 30 + 13 = 43.
        let target = display_offset_for_point_line(-30, 0, 20, 100).unwrap();
        assert_eq!(target, 43);
    }

    #[test]
    fn display_offset_returns_to_live_screen_when_scrolled_back() {
        // Con scroll profundo, una línea de la pantalla activa queda debajo
        // del viewport: el target la devuelve al tercio inferior.
        let target = display_offset_for_point_line(5, 50, 20, 100).unwrap();
        assert_eq!(target, 8);
    }

    #[test]
    fn display_offset_clamps_to_history_size() {
        let target = display_offset_for_point_line(-500, 0, 20, 100).unwrap();
        assert_eq!(target, 100);
    }

    fn display_offset_for_point_line(
        line: i32,
        current_offset: usize,
        visible_rows: usize,
        history_size: usize,
    ) -> Option<usize> {
        let point = alacritty_terminal::index::Point::new(
            alacritty_terminal::index::Line(line),
            alacritty_terminal::index::Column(0),
        );
        display_offset_for_match(point, current_offset, visible_rows, history_size)
    }
}
