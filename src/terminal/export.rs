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
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

use super::colors::{dim_color, indexed_to_egui};
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

/// Historial + pantalla como texto con colores ANSI (SGR mínimo).
///
/// Cada celda compara su estilo con el de la anterior y emite SGR solo cuando
/// cambia: `38;2;r;g;b` para el foreground, `48;2;…` para el background distinto
/// del default (`39`/`49` para volver al default) y `1`/`22` para bold. Cada
/// línea cierra con `\x1b[0m` para que ningún estilo se derrame a la siguiente.
/// El replay ya pasa por el parser VTE, así que restaurar esto devuelve los
/// colores sin tocar nada más.
pub fn scrollback_to_ansi(term: &Term<EventProxy>) -> String {
    let grid = term.grid();
    let history = grid.history_size();
    let rows = grid.screen_lines();
    let mut lines = Vec::with_capacity(history + rows);
    for line in -(history as i32)..rows as i32 {
        lines.push(row_to_ansi(&grid[Line(line)]));
    }
    join_document(lines)
}

/// A live attach is not a text document: retain row wrapping, cursor and
/// input modes so subsequent PTY bytes continue at the same visible state.
/// This deliberately differs from the compact durable history export.
pub fn live_snapshot_to_ansi(term: &Term<EventProxy>) -> String {
    live_snapshot_with_options(term, 0, term.grid().screen_lines() - 1, None)
}

/// The public VT handler exposes cursor movement constrained by DECSTBM.
/// Probe those bounds and tab stops under the caller's terminal lock, then
/// restore the exact cursor and origin flag. No text, saved cursor, screen,
/// parser state, or selection is modified; only repaint damage is broadened.
pub fn live_snapshot_with_terminal_state(term: &mut Term<EventProxy>) -> String {
    use alacritty_terminal::vte::ansi::{Handler, NamedPrivateMode, PrivateMode};
    let cursor = term.grid().cursor.clone();
    let origin = term.mode().contains(TermMode::ORIGIN);
    let mode = PrivateMode::Named(NamedPrivateMode::Origin);
    term.set_private_mode(mode);
    let top = term.grid().cursor.point.line.0 as usize;
    term.goto(term.grid().screen_lines() as i32, 0);
    let bottom = term.grid().cursor.point.line.0 as usize;
    let mut tabs = Vec::new();
    if term.grid().columns() > 1 {
        term.grid_mut().cursor.point.column.0 = 1;
        term.move_backward_tabs(1);
        if term.grid().cursor.point.column.0 == 0 {
            tabs.push(0);
        }
    }
    term.grid_mut().cursor.point.column.0 = 0;
    while term.grid().cursor.point.column.0 + 1 < term.grid().columns() {
        term.move_forward_tabs(1);
        tabs.push(term.grid().cursor.point.column.0);
    }
    if !origin {
        term.unset_private_mode(mode);
    }
    term.grid_mut().cursor = cursor;
    live_snapshot_with_options(term, top, bottom, Some(&tabs))
}

fn live_snapshot_with_options(
    term: &Term<EventProxy>,
    top: usize,
    bottom: usize,
    tabs: Option<&[usize]>,
) -> String {
    use std::fmt::Write as _;
    let grid = term.grid();
    let mut out = String::from("\x1bc");
    if term.mode().contains(TermMode::ALT_SCREEN) {
        out.push_str("\x1b[?1049h");
    }
    let first = -(grid.history_size() as i32);
    let last = grid.screen_lines() as i32 - 1;
    let mut previous_style = String::new();
    for line in first..=last {
        let row = &grid[Line(line)];
        for cell in row {
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let style = live_cell_style(cell);
            if style != previous_style {
                out.push_str(&style);
                previous_style = style;
            }
            out.push(cell.c);
            if let Some(extra) = cell.zerowidth() {
                out.extend(extra);
            }
        }
        // A wrapped row must stay wrapped after a later resize. Let the next
        // printable cell trigger autowrap; explicit line endings use CRLF.
        if line != last
            && !row[alacritty_terminal::index::Column(row.len() - 1)]
                .flags
                .contains(Flags::WRAPLINE)
        {
            out.push_str("\r\n");
        }
    }
    out.push_str("\x1b[0m");
    let _ = write!(out, "\x1b[{};{}r", top + 1, bottom + 1);
    if let Some(tabs) = tabs {
        out.push_str("\x1b[3g");
        for column in tabs {
            let _ = write!(out, "\x1b[1;{}H\x1bH", column + 1);
        }
    }
    let saved = &grid.saved_cursor;
    let _ = write!(
        out,
        "\x1b[{};{}H{}\x1b7",
        saved.point.line.0 + 1,
        saved.point.column.0 + 1,
        live_cell_style(&saved.template)
    );
    let cursor = &grid.cursor;
    // Restore the saved cursor with origin disabled so absolute saved
    // positions outside the current scroll region remain representable.
    out.push_str(&live_mode_sequence(*term.mode()));
    let cursor_line = cursor.point.line.0
        - if term.mode().contains(TermMode::ORIGIN) {
            top as i32
        } else {
            0
        };
    let _ = write!(
        out,
        "\x1b[{};{}H",
        cursor_line.max(0) + 1,
        cursor.point.column.0 + 1
    );
    if cursor.input_needs_wrap {
        let mut point = cursor.point;
        if grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) && point.column.0 > 0 {
            point.column.0 -= 1;
            let _ = write!(
                out,
                "\x1b[{};{}H",
                cursor_line.max(0) + 1,
                point.column.0 + 1
            );
        }
        let cell = &grid[point];
        out.push_str(&live_cell_style(cell));
        out.push(cell.c);
        if let Some(extra) = cell.zerowidth() {
            out.extend(extra);
        }
    }
    out.push_str(&live_cell_style(&cursor.template));
    let style = term.cursor_style();
    let shape = match style.shape {
        alacritty_terminal::vte::ansi::CursorShape::Underline => 3,
        alacritty_terminal::vte::ansi::CursorShape::Beam => 5,
        _ => 1,
    } + usize::from(!style.blinking);
    let _ = write!(out, "\x1b[{shape} q");
    out
}

fn live_cell_style(cell: &Cell) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("\x1b[0");
    for (flag, code) in [
        (Flags::BOLD, 1),
        (Flags::DIM, 2),
        (Flags::ITALIC, 3),
        (Flags::UNDERLINE, 4),
        (Flags::INVERSE, 7),
        (Flags::HIDDEN, 8),
        (Flags::STRIKEOUT, 9),
        (Flags::DOUBLE_UNDERLINE, 21),
    ] {
        if cell.flags.contains(flag) {
            let _ = write!(out, ";{code}");
        }
    }
    for (color, foreground) in [(&cell.fg, true), (&cell.bg, false)] {
        if let Some((r, g, b)) = color_rgb(color, foreground) {
            let _ = write!(out, ";{};2;{r};{g};{b}", if foreground { 38 } else { 48 });
        }
    }
    out.push('m');
    out
}

pub fn live_mode_sequence(mode: TermMode) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (flag, code) in [
        (TermMode::APP_CURSOR, 1),
        (TermMode::ORIGIN, 6),
        (TermMode::LINE_WRAP, 7),
        (TermMode::SHOW_CURSOR, 25),
        (TermMode::MOUSE_REPORT_CLICK, 1000),
        (TermMode::MOUSE_DRAG, 1002),
        (TermMode::MOUSE_MOTION, 1003),
        (TermMode::FOCUS_IN_OUT, 1004),
        (TermMode::UTF8_MOUSE, 1005),
        (TermMode::SGR_MOUSE, 1006),
        (TermMode::ALTERNATE_SCROLL, 1007),
        (TermMode::BRACKETED_PASTE, 2004),
    ] {
        let _ = write!(
            out,
            "\x1b[?{code}{}",
            if mode.contains(flag) { 'h' } else { 'l' }
        );
    }
    for (flag, code) in [(TermMode::INSERT, 4), (TermMode::LINE_FEED_NEW_LINE, 20)] {
        let _ = write!(
            out,
            "\x1b[{code}{}",
            if mode.contains(flag) { 'h' } else { 'l' }
        );
    }
    out.push_str(if mode.contains(TermMode::APP_KEYPAD) {
        "\x1b="
    } else {
        "\x1b>"
    });
    let mut keyboard = 0;
    for (flag, bit) in [
        (TermMode::DISAMBIGUATE_ESC_CODES, 1),
        (TermMode::REPORT_EVENT_TYPES, 2),
        (TermMode::REPORT_ALTERNATE_KEYS, 4),
        (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
        (TermMode::REPORT_ASSOCIATED_TEXT, 16),
    ] {
        if mode.contains(flag) {
            keyboard |= bit;
        }
    }
    let _ = write!(out, "\x1b[={keyboard}u");
    out
}

/// Fila del grid con SGR mínimo y sin el relleno de la derecha (misma regla de
/// recorte que `row_to_string`).
fn row_to_ansi(row: &Row<Cell>) -> String {
    let visible_chars = row_to_string(row).chars().count();

    type Rgb = (u8, u8, u8);
    let mut out = String::new();
    let (mut fg, mut bg, mut bold): (Option<Rgb>, Option<Rgb>, bool) = (None, None, false);
    let mut emitted = 0usize;
    for cell in row {
        if emitted >= visible_chars {
            break; // padding de la derecha recortado
        }
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER | Flags::HIDDEN)
        {
            continue;
        }
        let want_fg = color_rgb(&cell.fg, true);
        let want_bg = color_rgb(&cell.bg, false);
        let want_bold = cell.flags.contains(Flags::BOLD);

        if want_fg != fg || want_bg != bg || want_bold != bold {
            let mut parts: Vec<String> = Vec::new();
            if want_bold != bold {
                parts.push(if want_bold {
                    "1".to_owned()
                } else {
                    "22".to_owned()
                });
            }
            if want_fg != fg {
                match want_fg {
                    Some((r, g, b)) => parts.push(format!("38;2;{r};{g};{b}")),
                    None => parts.push("39".to_owned()),
                }
            }
            if want_bg != bg {
                match want_bg {
                    Some((r, g, b)) => parts.push(format!("48;2;{r};{g};{b}")),
                    None => parts.push("49".to_owned()),
                }
            }
            out.push_str("\x1b[");
            out.push_str(&parts.join(";"));
            out.push('m');
            (fg, bg, bold) = (want_fg, want_bg, want_bold);
        }

        out.push(cell.c);
        emitted += 1;
    }
    if fg.is_some() || bg.is_some() || bold {
        // Cierra el estilo de la línea: nada se derrama a la siguiente.
        out.push_str("\x1b[0m");
    }
    out
}

/// RGB de un color ANSI. `None` significa "default del terminal" (no emitir
/// SGR): foreground para el texto y background para el fondo.
fn color_rgb(color: &AnsiColor, foreground: bool) -> Option<(u8, u8, u8)> {
    let rgb = match color {
        AnsiColor::Spec(rgb) => (rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(idx) => {
            let c = indexed_to_egui(*idx);
            (c.r(), c.g(), c.b())
        }
        AnsiColor::Named(name) => named_to_rgb(*name, foreground)?,
    };
    Some(rgb)
}

/// Equivalente RGB de un color nombrado; `None` para los defaults del terminal
/// (foreground/background según el rol).
fn named_to_rgb(name: NamedColor, foreground: bool) -> Option<(u8, u8, u8)> {
    let c = match name {
        NamedColor::Foreground | NamedColor::BrightForeground if foreground => return None,
        NamedColor::Background if !foreground => return None,
        NamedColor::Foreground | NamedColor::BrightForeground => indexed_to_egui(7),
        NamedColor::Background | NamedColor::DimForeground | NamedColor::DimBlack => {
            egui::Color32::from_rgb(0, 0, 0)
        }
        NamedColor::Cursor => egui::Color32::from_rgb(244, 244, 244),
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
    };
    Some((c.r(), c.g(), c.b()))
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
    use alacritty_terminal::index::Line;
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
    fn live_snapshot_preserves_cursor_modes_and_combining_text() {
        let original = term_with(
            "first\r\nsecond e\u{301}\x1b[2;3H\x1b[31m\x1b7\x1b[3;5H\x1b[?1h\x1b[?2004h\x1b[?25l\x1b[4 q",
            5,
            24,
        );
        let replayed = term_with(&super::live_snapshot_to_ansi(&original), 5, 24);
        assert_eq!(scrollback_to_text(&original), scrollback_to_text(&replayed));
        assert_eq!(original.grid().cursor.point, replayed.grid().cursor.point);
        assert_eq!(
            original.grid().saved_cursor.point,
            replayed.grid().saved_cursor.point
        );
        assert_eq!(original.mode(), replayed.mode());
        assert_eq!(original.cursor_style(), replayed.cursor_style());
        assert_eq!(
            original.grid()[Line(1)][alacritty_terminal::index::Column(7)].zerowidth(),
            replayed.grid()[Line(1)][alacritty_terminal::index::Column(7)].zerowidth()
        );
    }

    #[test]
    fn live_snapshot_preserves_pending_wrap_and_alternate_screen() {
        for input in ["12345678", "\x1b[?1049h12345678"] {
            let mut original = term_with(input, 3, 8);
            let mut replayed = term_with(&super::live_snapshot_to_ansi(&original), 3, 8);
            assert_eq!(
                original.grid().cursor.input_needs_wrap,
                replayed.grid().cursor.input_needs_wrap
            );
            let mut parser = Processor::<StdSyncHandler>::new();
            parser.advance(&mut original, b"X");
            let mut parser = Processor::<StdSyncHandler>::new();
            parser.advance(&mut replayed, b"X");
            assert_eq!(scrollback_to_text(&original), scrollback_to_text(&replayed));
            assert_eq!(original.grid().cursor.point, replayed.grid().cursor.point);
            assert_eq!(original.mode(), replayed.mode());
        }
    }

    #[test]
    fn live_snapshot_preserves_custom_scroll_margins_and_tab_stops() {
        for origin in ["", "\x1b[?6h"] {
            let input = format!("one\r\ntwo\r\nthree\r\nfour\r\nfive\x1b[3g\x1b[1;5H\x1bH\x1b[2;4r{origin}\x1b[3;1H");
            let mut original = term_with(&input, 5, 12);
            let before = original.grid().cursor.clone();
            let mode = *original.mode();
            let snapshot = super::live_snapshot_with_terminal_state(&mut original);
            assert_eq!(original.grid().cursor.point, before.point);
            assert_eq!(
                original.grid().cursor.input_needs_wrap,
                before.input_needs_wrap
            );
            assert_eq!(*original.mode(), mode);
            let mut replayed = term_with(&snapshot, 5, 12);
            for bytes in [b"\r\nNEW\r\nLINE".as_slice(), b"\x1b[1;1H\tTAB"] {
                let mut parser = Processor::<StdSyncHandler>::new();
                parser.advance(&mut original, bytes);
                let mut parser = Processor::<StdSyncHandler>::new();
                parser.advance(&mut replayed, bytes);
                assert_eq!(scrollback_to_text(&original), scrollback_to_text(&replayed));
                assert_eq!(original.grid().cursor.point, replayed.grid().cursor.point);
            }
        }
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
    fn ansi_export_emits_sgr_for_colored_text() {
        // SGR 31 = rojo nombrado; el export lo baja a RGB de la paleta.
        let term = term_with("\x1b[31mrojo\x1b[0m\r\n", 6, 20);
        let ansi = super::scrollback_to_ansi(&term);
        assert!(
            ansi.contains("\x1b[38;2;204;0;0m"),
            "expected red SGR, got {ansi:?}"
        );
        // Cada línea cierra su estilo.
        assert!(ansi.contains("\x1b[0m"), "got {ansi:?}");
    }

    #[test]
    fn ansi_round_trip_restores_the_color_in_a_fresh_term() {
        let term = term_with("\x1b[31mrojo\x1b[0m\r\n", 6, 20);
        let ansi = super::scrollback_to_ansi(&term);
        let replayed = term_with(&ansi, 6, 40);
        let grid = replayed.grid();
        let fg = grid[Line(0)][alacritty_terminal::index::Column(0)].fg;
        match fg {
            alacritty_terminal::vte::ansi::Color::Spec(rgb) => {
                assert_eq!((rgb.r, rgb.g, rgb.b), (204, 0, 0), "got {fg:?}");
            }
            other => panic!("expected truecolor fg, got {other:?}"),
        }
    }

    #[test]
    fn ansi_export_stays_plain_for_default_text() {
        let term = term_with("plain\r\n", 6, 20);
        let ansi = super::scrollback_to_ansi(&term);
        assert!(!ansi.contains("38;2"), "no fg SGR for default: {ansi:?}");
        assert!(!ansi.contains("48;2"), "no bg SGR for default: {ansi:?}");
    }

    #[test]
    fn ansi_export_emits_bold_transitions() {
        let term = term_with("\x1b[1mfuerte\x1b[22m suave\r\n", 6, 30);
        let ansi = super::scrollback_to_ansi(&term);
        assert!(ansi.contains("\x1b[1m"), "bold on: {ansi:?}");
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
