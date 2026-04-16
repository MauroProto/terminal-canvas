use egui::{Key, Modifiers};

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
    pub alt_screen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WheelAction {
    Pty(Vec<u8>),
    Scrollback(i32),
}

pub fn wheel_action(delta: f32, mode: &InputMode, point: Option<GridPoint>) -> Option<WheelAction> {
    if delta.abs() <= f32::EPSILON {
        return None;
    }

    if mode.alt_screen {
        return Some(WheelAction::Pty(alt_screen_scroll_sequence(delta).to_vec()));
    }

    if mode.mouse_mode {
        let point = point.unwrap_or(GridPoint { line: 0, column: 0 });
        return Some(WheelAction::Pty(mouse_scroll_sgr_sequence(
            mouse_scroll_button(delta),
            point.column,
            point.line,
        )));
    }

    Some(WheelAction::Scrollback(scrollback_delta_from_input(delta)))
}

pub fn scroll_lines_from_input_delta(delta: f32) -> i32 {
    ((delta.abs() / 24.0).round() as i32).max(1)
}

pub fn alt_screen_scroll_sequence(delta: f32) -> &'static [u8] {
    if scrollback_delta_from_input(delta) > 0 {
        b"\x1b[A"
    } else {
        b"\x1b[B"
    }
}

pub fn mouse_scroll_button(delta: f32) -> u8 {
    if scrollback_delta_from_input(delta) > 0 {
        64
    } else {
        65
    }
}

pub fn mouse_scroll_sgr_sequence(button: u8, column: usize, row: usize) -> Vec<u8> {
    format!("\x1b[<{};{};{}M", button, column + 1, row + 1).into_bytes()
}

pub fn scrollback_delta_from_input(delta: f32) -> i32 {
    let lines = scroll_lines_from_input_delta(delta);
    if native_scrolls_toward_history(delta) {
        lines
    } else {
        -lines
    }
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

        let action = wheel_action(-48.0, &mode, Some(GridPoint { line: 2, column: 3 })).unwrap();

        #[cfg(target_os = "macos")]
        assert_eq!(action, WheelAction::Pty(b"\x1b[<65;4;3M".to_vec()));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(action, WheelAction::Pty(b"\x1b[<64;4;3M".to_vec()));
    }

    #[test]
    fn wheel_action_falls_back_to_scrollback_without_mouse_mode() {
        let action = wheel_action(
            48.0,
            &InputMode::default(),
            Some(GridPoint { line: 0, column: 0 }),
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
