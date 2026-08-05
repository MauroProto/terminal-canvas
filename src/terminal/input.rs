use egui::{Key, Modifiers};

use super::metrics::scroll_points_per_line;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridPoint {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Default)]
pub struct InputMode {
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_mode: bool,
    /// El TUI pidió tracking de movimiento con botón presionado (1002/1003).
    pub mouse_drag: bool,
    /// El TUI pidió tracking de todo movimiento, incluso sin botón (1003).
    pub mouse_motion: bool,
    pub alt_screen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WheelAction {
    Pty(Vec<u8>),
    Scrollback(i32),
}

// Los puntos por línea de scroll salen de `terminal::metrics` (compartidos
// con el renderer; el harness de runtime monta este módulo sin el renderer,
// por eso viven allá).

/// Convierte los deltas de la rueda (en puntos) en líneas enteras de
/// terminal, acumulando el resto entre llamadas. El trackpad con momentum
/// entrega un delta chico por frame durante segundos; emitir "al menos una
/// línea" por llamada (el comportamiento anterior) producía ~120 líneas/s
/// con un flick suave: scroll incontrolable.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollAccumulator {
    pending: f32,
}

impl ScrollAccumulator {
    /// Líneas enteras cruzadas por `delta`; positivo = hacia el historial.
    pub fn take_lines(&mut self, delta: f32) -> i32 {
        let toward_history = if native_scrolls_toward_history(delta) {
            delta.abs()
        } else {
            -delta.abs()
        };
        // Invertir el sentido no paga el resto del gesto anterior: la
        // respuesta inmediata importa más que esa fracción de línea.
        if self.pending != 0.0 && (toward_history > 0.0) != (self.pending > 0.0) {
            self.pending = 0.0;
        }
        self.pending += toward_history;
        let points_per_line = scroll_points_per_line();
        let lines = (self.pending / points_per_line).trunc() as i32;
        self.pending -= lines as f32 * points_per_line;
        lines
    }
}

pub fn wheel_action(
    delta: f32,
    mode: &InputMode,
    point: Option<GridPoint>,
    accumulator: &mut ScrollAccumulator,
) -> Option<WheelAction> {
    if delta.abs() <= f32::EPSILON {
        return None;
    }

    // El mouse reportado tiene prioridad sobre alt screen: los TUIs de
    // pantalla completa que piden mouse (codex, ratatui, etc.) scrollean
    // nativo con estos reportes.
    if mode.mouse_mode {
        let lines = accumulator.take_lines(delta);
        if lines == 0 {
            return None;
        }
        let point = point.unwrap_or(GridPoint { line: 0, column: 0 });
        let button = if lines > 0 { 64 } else { 65 };
        let mut bytes = Vec::new();
        for _ in 0..lines.unsigned_abs() {
            bytes.extend_from_slice(&mouse_scroll_sgr_sequence(button, point.column, point.line));
        }
        return Some(WheelAction::Pty(bytes));
    }

    if mode.alt_screen {
        // La app es dueña de la pantalla y no pidió mouse: inyectar flechas
        // fantasma corrompe su input (p. ej. recall de historial en el
        // prompt de Claude Code), así que la rueda no hace nada.
        return None;
    }

    let lines = accumulator.take_lines(delta);
    if lines == 0 {
        return None;
    }
    Some(WheelAction::Scrollback(lines))
}

/// egui-winit intercepta los atajos de clipboard y entrega
/// `Event::Copy`/`Event::Cut` en lugar de la tecla. Sin selección, en las
/// plataformas donde Ctrl actúa como "command" (Linux/Windows) hay que
/// reenviar el byte de control al PTY para no perder Ctrl+C (SIGINT) ni
/// Ctrl+X; en macOS Cmd+C sin selección no hace nada, como Terminal.app.
pub fn clipboard_event_fallback_bytes(cut: bool) -> Option<&'static [u8]> {
    #[cfg(target_os = "macos")]
    {
        let _ = cut;
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(if cut { b"\x18" } else { b"\x03" })
    }
}

pub fn mouse_scroll_sgr_sequence(button: u8, column: usize, row: usize) -> Vec<u8> {
    format!("\x1b[<{};{};{}M", button, column + 1, row + 1).into_bytes()
}

/// Bits de modificadores SGR (shift=4, meta/alt=8, ctrl=16).
fn sgr_modifier_bits(modifiers: &Modifiers) -> u8 {
    4 * u8::from(modifiers.shift) + 8 * u8::from(modifiers.alt) + 16 * u8::from(modifiers.ctrl)
}

/// Secuencia SGR (1006) de click: `button` 0=izq, 1=medio, 2=der. `release`
/// usa el terminador `m` en vez de `M`.
pub fn mouse_click_sgr_sequence(
    button: u8,
    release: bool,
    modifiers: &Modifiers,
    column: usize,
    row: usize,
) -> Vec<u8> {
    let cb = button + sgr_modifier_bits(modifiers);
    let terminator = if release { 'm' } else { 'M' };
    format!("\x1b[<{};{};{}{}", cb, column + 1, row + 1, terminator).into_bytes()
}

/// Secuencia SGR de movimiento: bit 32 sobre el botón. `button` 3 = sin
/// botón (hover), 0/1/2 = arrastre con ese botón.
pub fn mouse_motion_sgr_sequence(
    button: u8,
    modifiers: &Modifiers,
    column: usize,
    row: usize,
) -> Vec<u8> {
    let cb = 32 + button + sgr_modifier_bits(modifiers);
    format!("\x1b[<{};{};{}M", cb, column + 1, row + 1).into_bytes()
}

/// Neutraliza los bytes de escape de un prompt antes de inyectarlo en un
/// agente (idea de orca): un prompt no debe poder emitir secuencias de
/// control al terminal.
pub fn sanitize_agent_prompt(text: &str) -> String {
    text.replace('\x1b', "<ESC>")
}

/// Compone los bytes para inyectar un prompt/feedback en un agente:
/// sanitizado + bracketed paste (si el TUI lo activó) + Enter para enviarlo.
/// El paste atómico evita que un prompt multi-línea ejecute la 1ª línea sola.
pub fn agent_prompt_bytes(text: &str, mode: &InputMode) -> Vec<u8> {
    let sanitized = sanitize_agent_prompt(text.trim());
    let mut bytes = paste_bytes(&sanitized, mode);
    bytes.push(b'\r');
    bytes
}

#[inline]
fn native_scrolls_toward_history(delta: f32) -> bool {
    #[cfg(target_os = "macos")]
    {
        delta > 0.0
    }
    #[cfg(not(target_os = "macos"))]
    {
        delta < 0.0
    }
}

pub fn modifier_param(modifiers: &Modifiers) -> u8 {
    1 + u8::from(modifiers.shift) + 2 * u8::from(modifiers.alt) + 4 * u8::from(modifiers.ctrl)
}

pub fn cursor_key_sequence(letter: u8, modifiers: &Modifiers, mode: &InputMode) -> Option<Vec<u8>> {
    if modifiers.shift || modifiers.alt || modifiers.ctrl {
        let param = modifier_param(modifiers);
        Some(format!("\x1b[1;{}{}", param, letter as char).into_bytes())
    } else if mode.app_cursor {
        Some(vec![0x1b, b'O', letter])
    } else {
        Some(vec![0x1b, b'[', letter])
    }
}

pub fn csi_modifier(letter: u8, modifiers: &Modifiers) -> Option<Vec<u8>> {
    Some(format!("\x1b[1;{}{}", modifier_param(modifiers), letter as char).into_bytes())
}

pub fn tilde_key_with_mods(code: u8, modifiers: &Modifiers) -> Option<Vec<u8>> {
    if modifiers.shift || modifiers.alt || modifiers.ctrl {
        Some(format!("\x1b[{};{}~", code, modifier_param(modifiers)).into_bytes())
    } else {
        Some(format!("\x1b[{}~", code).into_bytes())
    }
}

pub fn fkey_sequence(fnum: u8, modifiers: &Modifiers) -> Option<Vec<u8>> {
    let has_mods = modifiers.shift || modifiers.alt || modifiers.ctrl;
    match fnum {
        1..=4 => {
            let letter = match fnum {
                1 => b'P',
                2 => b'Q',
                3 => b'R',
                4 => b'S',
                _ => unreachable!(),
            };
            if has_mods {
                Some(format!("\x1b[1;{}{}", modifier_param(modifiers), letter as char).into_bytes())
            } else {
                Some(vec![0x1b, b'O', letter])
            }
        }
        5..=20 => {
            let code = match fnum {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                13 => 25,
                14 => 26,
                15 => 28,
                16 => 29,
                17 => 31,
                18 => 32,
                19 => 33,
                20 => 34,
                _ => unreachable!(),
            };
            if has_mods {
                Some(format!("\x1b[{};{}~", code, modifier_param(modifiers)).into_bytes())
            } else {
                Some(format!("\x1b[{}~", code).into_bytes())
            }
        }
        _ => None,
    }
}

pub fn should_copy_selection(modifiers: &Modifiers, key: &Key, has_selection: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        modifiers.command && *key == Key::C && has_selection
    }
    #[cfg(not(target_os = "macos"))]
    {
        (modifiers.ctrl && *key == Key::C && has_selection)
            || (modifiers.ctrl && modifiers.shift && *key == Key::C)
    }
}

pub fn is_paste_shortcut(modifiers: &Modifiers, key: &Key) -> bool {
    #[cfg(target_os = "macos")]
    {
        modifiers.command && *key == Key::V
    }
    #[cfg(not(target_os = "macos"))]
    {
        (modifiers.ctrl && *key == Key::V) || (modifiers.ctrl && modifiers.shift && *key == Key::V)
    }
}

pub fn key_to_bytes(key: &Key, modifiers: &Modifiers, mode: &InputMode) -> Option<Vec<u8>> {
    match key {
        Key::Enter => {
            if modifiers.shift {
                Some(b"\n".to_vec())
            } else if modifiers.alt {
                Some(b"\x1b\r".to_vec())
            } else {
                Some(b"\r".to_vec())
            }
        }
        Key::Backspace => {
            if modifiers.ctrl {
                Some(b"\x17".to_vec())
            } else if modifiers.alt {
                Some(b"\x1b\x7f".to_vec())
            } else {
                Some(b"\x7f".to_vec())
            }
        }
        Key::Tab => {
            if modifiers.shift {
                Some(b"\x1b[Z".to_vec())
            } else {
                Some(b"\t".to_vec())
            }
        }
        Key::Escape => Some(b"\x1b".to_vec()),
        Key::ArrowUp => cursor_key_sequence(b'A', modifiers, mode),
        Key::ArrowDown => cursor_key_sequence(b'B', modifiers, mode),
        Key::ArrowRight => cursor_key_sequence(b'C', modifiers, mode),
        Key::ArrowLeft => cursor_key_sequence(b'D', modifiers, mode),
        Key::Home => {
            if modifiers.any() {
                csi_modifier(b'H', modifiers)
            } else if mode.app_cursor {
                Some(b"\x1bOH".to_vec())
            } else {
                Some(b"\x1b[H".to_vec())
            }
        }
        Key::End => {
            if modifiers.any() {
                csi_modifier(b'F', modifiers)
            } else if mode.app_cursor {
                Some(b"\x1bOF".to_vec())
            } else {
                Some(b"\x1b[F".to_vec())
            }
        }
        Key::PageUp => tilde_key_with_mods(5, modifiers),
        Key::PageDown => tilde_key_with_mods(6, modifiers),
        Key::Insert => tilde_key_with_mods(2, modifiers),
        Key::Delete => {
            if modifiers.ctrl {
                Some(b"\x1bd".to_vec())
            } else {
                tilde_key_with_mods(3, modifiers)
            }
        }
        Key::F1 => fkey_sequence(1, modifiers),
        Key::F2 => fkey_sequence(2, modifiers),
        Key::F3 => fkey_sequence(3, modifiers),
        Key::F4 => fkey_sequence(4, modifiers),
        Key::F5 => fkey_sequence(5, modifiers),
        Key::F6 => fkey_sequence(6, modifiers),
        Key::F7 => fkey_sequence(7, modifiers),
        Key::F8 => fkey_sequence(8, modifiers),
        Key::F9 => fkey_sequence(9, modifiers),
        Key::F10 => fkey_sequence(10, modifiers),
        Key::F11 => fkey_sequence(11, modifiers),
        Key::F12 => fkey_sequence(12, modifiers),
        Key::Space if modifiers.ctrl => Some(vec![0x00]),
        _ if modifiers.ctrl => ctrl_alpha(*key),
        _ => None,
    }
}

pub fn paste_bytes(text: &str, mode: &InputMode) -> Vec<u8> {
    if mode.bracketed_paste {
        let mut bytes = Vec::with_capacity(text.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        text.as_bytes().to_vec()
    }
}

fn ctrl_alpha(key: Key) -> Option<Vec<u8>> {
    let index = match key {
        Key::A => 1,
        Key::B => 2,
        Key::C => 3,
        Key::D => 4,
        Key::E => 5,
        Key::F => 6,
        Key::G => 7,
        Key::H => 8,
        Key::I => 9,
        Key::J => 10,
        Key::K => 11,
        Key::L => 12,
        Key::M => 13,
        Key::N => 14,
        Key::O => 15,
        Key::P => 16,
        Key::Q => 17,
        Key::R => 18,
        Key::S => 19,
        Key::T => 20,
        Key::U => 21,
        Key::V => 22,
        Key::W => 23,
        Key::X => 24,
        Key::Y => 25,
        Key::Z => 26,
        _ => return None,
    };
    Some(vec![index])
}

#[cfg(test)]
mod tests {
    use egui::{Key, Modifiers};

    use super::{
        cursor_key_sequence, is_paste_shortcut, paste_bytes, should_copy_selection, wheel_action,
        GridPoint, InputMode, WheelAction,
    };

    #[test]
    fn arrow_keys_follow_application_cursor_mode() {
        let mode = InputMode {
            app_cursor: true,
            ..InputMode::default()
        };
        let seq = cursor_key_sequence(b'A', &Modifiers::NONE, &mode).unwrap();
        assert_eq!(seq, b"\x1bOA");
    }

    #[test]
    fn modified_arrow_keys_stay_in_csi_form() {
        let mode = InputMode {
            app_cursor: true,
            ..InputMode::default()
        };
        let modifiers = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        let seq = cursor_key_sequence(b'A', &modifiers, &mode).unwrap();
        assert_eq!(seq, b"\x1b[1;2A");
    }

    #[test]
    fn copy_event_maps_to_sigint_on_non_macos() {
        #[cfg(not(target_os = "macos"))]
        {
            let modifiers = Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            };
            let seq = super::key_to_bytes(&Key::C, &modifiers, &InputMode::default()).unwrap();
            assert_eq!(seq, vec![0x03]);
        }
    }

    #[test]
    fn copy_event_prefers_selection_over_sigint() {
        let modifiers = copy_modifiers();
        assert!(should_copy_selection(&modifiers, &Key::C, true));
    }

    #[test]
    fn paste_is_wrapped_in_bracketed_mode() {
        let bytes = paste_bytes(
            "echo hola",
            &InputMode {
                bracketed_paste: true,
                ..InputMode::default()
            },
        );
        assert_eq!(bytes, b"\x1b[200~echo hola\x1b[201~");
    }

    #[test]
    fn paste_is_raw_without_bracketed_mode() {
        let bytes = paste_bytes("echo hola", &InputMode::default());
        assert_eq!(bytes, b"echo hola");
    }

    #[test]
    fn platform_paste_shortcut_is_recognized() {
        let modifiers = paste_modifiers();
        assert!(is_paste_shortcut(&modifiers, &Key::V));
    }

    #[test]
    fn ctrl_c_with_selection_is_copy_shortcut() {
        let modifiers = copy_modifiers();
        assert!(should_copy_selection(&modifiers, &Key::C, true));
        assert!(!should_copy_selection(&modifiers, &Key::C, false));
    }

    #[test]
    fn wheel_action_uses_pointer_cell_when_mouse_mode_is_enabled() {
        let mode = InputMode {
            mouse_mode: true,
            ..InputMode::default()
        };
        let mut accumulator = super::ScrollAccumulator::default();

        let action = wheel_action(
            -48.0,
            &mode,
            Some(GridPoint { line: 2, column: 3 }),
            &mut accumulator,
        )
        .unwrap();

        // 48 puntos ≈ 2 líneas: dos reportes SGR en la celda del puntero.
        #[cfg(target_os = "macos")]
        assert_eq!(
            action,
            WheelAction::Pty(b"\x1b[<65;4;3M\x1b[<65;4;3M".to_vec())
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            action,
            WheelAction::Pty(b"\x1b[<64;4;3M\x1b[<64;4;3M".to_vec())
        );
    }

    #[test]
    fn wheel_action_ignores_alt_screen_without_mouse_reports() {
        let mode = InputMode {
            alt_screen: true,
            ..InputMode::default()
        };
        let mut accumulator = super::ScrollAccumulator::default();
        // Nada de flechas fantasma: corrompen el input de los TUIs.
        assert!(wheel_action(-48.0, &mode, None, &mut accumulator).is_none());
        assert!(wheel_action(48.0, &mode, None, &mut accumulator).is_none());
    }

    #[test]
    fn wheel_action_prefers_mouse_reports_over_alt_screen() {
        let mode = InputMode {
            alt_screen: true,
            mouse_mode: true,
            ..InputMode::default()
        };
        let mut accumulator = super::ScrollAccumulator::default();
        let action = wheel_action(
            -48.0,
            &mode,
            Some(GridPoint { line: 2, column: 3 }),
            &mut accumulator,
        )
        .unwrap();
        assert!(matches!(action, WheelAction::Pty(_)));
    }

    #[test]
    fn scroll_accumulator_smooths_trackpad_momentum() {
        let mode = InputMode::default();
        let mut accumulator = super::ScrollAccumulator::default();
        let mut total = 0;

        // Momentum: muchos eventos chicos (2 puntos por frame). Antes cada
        // uno emitía al menos una línea (30 en total); acumulados son
        // 60 puntos ≈ 3 líneas.
        for _ in 0..30 {
            if let Some(WheelAction::Scrollback(lines)) =
                wheel_action(2.0, &mode, None, &mut accumulator)
            {
                total += lines;
            }
        }

        assert_eq!(total.abs(), 3);
    }

    #[test]
    fn scroll_accumulator_resets_pending_on_direction_change() {
        let mut accumulator = super::ScrollAccumulator::default();

        assert_eq!(accumulator.take_lines(17.0), 0);
        // El resto acumulado no amortigua el gesto en sentido contrario.
        assert_eq!(accumulator.take_lines(-17.0), 0);
        assert_eq!(accumulator.take_lines(-2.0).abs(), 1);
    }

    /// LCG determinista para tests de propiedades sin dependencias.
    fn next_pseudo_random(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *state >> 33
    }

    #[test]
    fn scroll_accumulator_conserves_total_lines_for_any_same_sign_gesture() {
        let mut rng: u64 = 0x5EED;

        for case in 0..100 {
            let mut accumulator = super::ScrollAccumulator::default();
            let mut total_points = 0.0f32;
            let mut emitted: i64 = 0;

            for _ in 0..200 {
                // Deltas de 0.01 a 40.0 puntos, como los del trackpad.
                let delta = (next_pseudo_random(&mut rng) % 4000 + 1) as f32 / 100.0;
                total_points += delta;
                emitted += i64::from(accumulator.take_lines(delta));
            }

            // Propiedad: las líneas emitidas equivalen al total desplazado
            // (±1 por redondeo flotante). El bug anterior (mínimo una línea
            // por evento) emitía ~200 acá.
            let expected = (total_points / super::scroll_points_per_line()).trunc() as i64;
            assert!(
                (emitted.abs() - expected).abs() <= 1,
                "caso {case}: emitidas {emitted} vs esperadas ±{expected}"
            );
        }
    }

    #[test]
    fn scroll_accumulator_pending_never_reaches_a_full_line() {
        let mut rng: u64 = 0xACC0;
        let mut accumulator = super::ScrollAccumulator::default();

        for _ in 0..2000 {
            // Deltas con signo alternante pseudoaleatorio.
            let magnitude = (next_pseudo_random(&mut rng) % 6000) as f32 / 100.0;
            let delta = if next_pseudo_random(&mut rng).is_multiple_of(2) {
                magnitude
            } else {
                -magnitude
            };
            let _ = accumulator.take_lines(delta);
            assert!(
                accumulator.pending.abs() < super::scroll_points_per_line(),
                "el resto acumulado nunca debe llegar a una línea entera"
            );
        }
    }

    #[test]
    fn mouse_click_sgr_encodes_press_and_release() {
        let press = super::mouse_click_sgr_sequence(0, false, &Modifiers::NONE, 4, 2);
        assert_eq!(press, b"\x1b[<0;5;3M");
        let release = super::mouse_click_sgr_sequence(0, true, &Modifiers::NONE, 4, 2);
        assert_eq!(release, b"\x1b[<0;5;3m");
    }

    #[test]
    fn mouse_click_sgr_includes_modifier_bits() {
        let modifiers = Modifiers {
            shift: true,
            ctrl: true,
            ..Modifiers::NONE
        };
        // 0 base + 4 (shift) + 16 (ctrl) = 20.
        let seq = super::mouse_click_sgr_sequence(0, false, &modifiers, 0, 0);
        assert_eq!(seq, b"\x1b[<20;1;1M");
    }

    #[test]
    fn mouse_motion_sgr_sets_motion_bit() {
        let drag = super::mouse_motion_sgr_sequence(0, &Modifiers::NONE, 1, 1);
        assert_eq!(drag, b"\x1b[<32;2;2M");
        // Botón 3 = movimiento sin botón presionado (hover, modo 1003).
        let hover = super::mouse_motion_sgr_sequence(3, &Modifiers::NONE, 1, 1);
        assert_eq!(hover, b"\x1b[<35;2;2M");
    }

    #[test]
    fn sanitize_agent_prompt_neutralizes_escape_bytes() {
        assert_eq!(
            super::sanitize_agent_prompt("fix \x1b[31m bug"),
            "fix <ESC>[31m bug"
        );
        assert_eq!(super::sanitize_agent_prompt("plain brief"), "plain brief");
    }

    #[test]
    fn agent_prompt_bytes_wraps_bracketed_and_submits() {
        let mode = super::InputMode {
            bracketed_paste: true,
            ..super::InputMode::default()
        };
        let bytes = super::agent_prompt_bytes("revisá el error", &mode);
        assert_eq!(bytes, b"\x1b[200~revis\xc3\xa1 el error\x1b[201~\r");
    }

    #[test]
    fn agent_prompt_bytes_raw_without_bracketed_and_trims() {
        let mode = super::InputMode::default();
        let bytes = super::agent_prompt_bytes("  hello  ", &mode);
        assert_eq!(bytes, b"hello\r");
    }

    #[test]
    fn agent_prompt_bytes_sanitizes_escapes() {
        let mode = super::InputMode::default();
        let bytes = super::agent_prompt_bytes("a\x1bb", &mode);
        assert_eq!(bytes, b"a<ESC>b\r");
    }

    #[test]
    fn clipboard_event_fallback_matches_platform() {
        #[cfg(target_os = "macos")]
        {
            assert!(super::clipboard_event_fallback_bytes(false).is_none());
            assert!(super::clipboard_event_fallback_bytes(true).is_none());
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(
                super::clipboard_event_fallback_bytes(false),
                Some(b"\x03".as_slice())
            );
            assert_eq!(
                super::clipboard_event_fallback_bytes(true),
                Some(b"\x18".as_slice())
            );
        }
    }

    #[test]
    fn wheel_action_falls_back_to_scrollback_without_mouse_mode() {
        let mut accumulator = super::ScrollAccumulator::default();
        let action = wheel_action(
            48.0,
            &InputMode::default(),
            Some(GridPoint { line: 0, column: 0 }),
            &mut accumulator,
        )
        .unwrap();

        #[cfg(target_os = "macos")]
        assert_eq!(action, WheelAction::Scrollback(2));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(action, WheelAction::Scrollback(-2));
    }

    fn copy_modifiers() -> Modifiers {
        #[cfg(target_os = "macos")]
        {
            Modifiers {
                command: true,
                ..Modifiers::NONE
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            }
        }
    }

    fn paste_modifiers() -> Modifiers {
        #[cfg(target_os = "macos")]
        {
            Modifiers {
                command: true,
                ..Modifiers::NONE
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            }
        }
    }
}
