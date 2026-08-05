//! Exportación del scrollback a texto plano: pasa el historial más la pantalla
//! activa a un `String` listo para escribir a disco.
//!
//! El recorrido del grid es fino (una pasada, sin copiar celdas) y el armado
//! del texto está separado en funciones puras para poder testearlo sin montar
//! un terminal completo.

use alacritty_terminal::grid::{Dimensions, Row};
use alacritty_terminal::index::Line;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::Term;

use super::pty::EventProxy;

/// Convierte historial + pantalla activa en texto plano. Cada fila del grid es
/// una línea; se recortan los espacios de relleno a la derecha y las líneas
/// vacías del final (el área de pantalla que el shell nunca usó).
pub fn scrollback_to_text(term: &Term<EventProxy>) -> String {
    let grid = term.grid();
    let history = grid.history_size();
    let rows = grid.screen_lines();
    let mut lines = Vec::with_capacity(history + rows);
    // El historial vive en líneas negativas y la pantalla activa en 0..rows.
    for line in -(history as i32)..rows as i32 {
        lines.push(row_to_string(&grid[Line(line)]));
    }
    join_document(lines)
}

/// Texto de una fila del grid, sin el relleno de la derecha.
fn row_to_string(row: &Row<Cell>) -> String {
    let mut text = String::with_capacity(row.len());
    for cell in row {
        // Los spacers de caracteres anchos no aportan texto (el glifo ya se
        // emitió en la celda anterior) y las celdas ocultas tampoco.
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER | Flags::HIDDEN)
        {
            continue;
        }
        text.push(cell.c);
    }
    while text.ends_with(' ') {
        text.pop();
    }
    text
}

/// Une las líneas descartando las vacías del final y cierra con un salto de
/// línea (convención POSIX). Un documento sin contenido queda vacío, no en un
/// "\n" solitario.
fn join_document(mut lines: Vec<String>) -> String {
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// Nombre de archivo seguro para el export de un panel: el título del terminal
/// puede traer barras, dos puntos o espacios (viene del OSC 0/2 del shell), y
/// nada de eso puede terminar en un path.
pub fn export_file_name(title: &str, timestamp: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            // Cualquier otra cosa (espacios, separadores de path, unicode)
            // colapsa en un solo guión.
            slug.push('-');
        }
    }
    let slug = slug.trim_matches(['-', '.']).to_owned();
    // Tope de largo para no chocar con el límite de nombre del filesystem.
    let slug: String = slug.chars().take(48).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        format!("terminal-{timestamp}.txt")
    } else {
        format!("{slug}-{timestamp}.txt")
    }
}

/// Marca temporal ordenable para nombres de archivo (`AAAAMMDD-HHMMSS` local).
pub fn export_timestamp(now: chrono::DateTime<chrono::Local>) -> String {
    now.format("%Y%m%d-%H%M%S").to_string()
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::grid::Row;
    use alacritty_terminal::term::cell::{Cell, Flags};
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config as TermConfig, Term};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
    use std::sync::mpsc;

    use super::{join_document, row_to_string, scrollback_to_text};
    use crate::terminal::pty::EventProxy;

    fn cell(c: char, flags: Flags) -> Cell {
        Cell {
            c,
            flags,
            ..Cell::default()
        }
    }

    fn row_from(text: &str, width: usize) -> Row<Cell> {
        let mut cells: Vec<Cell> = text
            .chars()
            .map(|ch| cell(ch, Flags::empty()))
            .collect::<Vec<_>>();
        while cells.len() < width {
            cells.push(Cell::default());
        }
        Row::from_vec(cells, width)
    }

    #[test]
    fn row_drops_the_padding_on_the_right() {
        assert_eq!(row_to_string(&row_from("hola", 20)), "hola");
    }

    #[test]
    fn row_keeps_interior_spaces() {
        assert_eq!(row_to_string(&row_from("a  b", 10)), "a  b");
    }

    #[test]
    fn row_skips_wide_char_spacers_so_glyphs_are_not_duplicated() {
        let cells = vec![
            cell('漢', Flags::WIDE_CHAR),
            cell(' ', Flags::WIDE_CHAR_SPACER),
            cell('x', Flags::empty()),
        ];
        let row = Row::from_vec(cells, 3);
        assert_eq!(row_to_string(&row), "漢x");
    }

    #[test]
    fn document_drops_trailing_blank_lines_and_ends_with_newline() {
        let lines = vec![
            "one".to_owned(),
            "two".to_owned(),
            String::new(),
            String::new(),
        ];
        assert_eq!(join_document(lines), "one\ntwo\n");
    }

    #[test]
    fn document_keeps_interior_blank_lines() {
        let lines = vec!["one".to_owned(), String::new(), "two".to_owned()];
        assert_eq!(join_document(lines), "one\n\ntwo\n");
    }

    #[test]
    fn empty_document_is_empty_not_a_lone_newline() {
        assert!(join_document(Vec::new()).is_empty());
        assert!(join_document(vec![String::new(), String::new()]).is_empty());
    }

    fn term_with(input: &str, rows: usize, cols: usize) -> Term<EventProxy> {
        let (tx, _rx) = mpsc::channel();
        let mut term = Term::new(
            TermConfig::default(),
            &TermSize::new(cols, rows),
            EventProxy::new(tx),
        );
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        for byte in input.as_bytes() {
            parser.advance(&mut term, &[*byte]);
        }
        term
    }

    #[test]
    fn exports_the_active_screen() {
        let term = term_with("alpha\r\nbeta\r\n", 6, 20);
        assert_eq!(scrollback_to_text(&term), "alpha\nbeta\n");
    }

    #[test]
    fn exports_history_scrolled_out_of_the_screen() {
        // Más líneas que filas: las primeras caen al historial y deben salir
        // igual, en orden.
        let rows = 4;
        let mut input = String::new();
        for index in 0..10 {
            input.push_str(&format!("line{index}\r\n"));
        }
        let term = term_with(&input, rows, 20);
        let text = scrollback_to_text(&term);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 10, "got {lines:?}");
        for (index, line) in lines.iter().enumerate() {
            assert_eq!(*line, format!("line{index}"));
        }
    }

    #[test]
    fn empty_terminal_exports_nothing() {
        let term = term_with("", 6, 20);
        assert!(scrollback_to_text(&term).is_empty());
    }
    #[test]
    fn file_name_slugifies_the_terminal_title() {
        assert_eq!(
            super::export_file_name("My Project", "20260805-120000"),
            "my-project-20260805-120000.txt"
        );
    }

    #[test]
    fn file_name_never_contains_path_separators() {
        let name = super::export_file_name("../../etc/passwd", "ts");
        assert!(!name.contains('/'), "got {name}");
        assert!(!name.contains(".."), "got {name}");
        assert_eq!(name, "etc-passwd-ts.txt");
    }

    #[test]
    fn file_name_collapses_runs_of_separators() {
        assert_eq!(super::export_file_name("a   ///  b", "ts"), "a-b-ts.txt");
    }

    #[test]
    fn file_name_falls_back_when_the_title_has_nothing_usable() {
        assert_eq!(super::export_file_name("", "ts"), "terminal-ts.txt");
        assert_eq!(super::export_file_name("   ", "ts"), "terminal-ts.txt");
        assert_eq!(super::export_file_name("///", "ts"), "terminal-ts.txt");
    }

    #[test]
    fn file_name_is_capped_and_does_not_end_in_a_separator() {
        let name = super::export_file_name(&"ab ".repeat(60), "ts");
        assert!(name.len() < 80, "got {} chars: {name}", name.len());
        assert!(!name.contains("-.txt"), "got {name}");
    }

    #[test]
    fn timestamp_is_sortable() {
        use chrono::TimeZone;
        let when = chrono::Local
            .with_ymd_and_hms(2026, 8, 5, 16, 4, 9)
            .single()
            .expect("valid local time");
        assert_eq!(super::export_timestamp(when), "20260805-160409");
    }
}
