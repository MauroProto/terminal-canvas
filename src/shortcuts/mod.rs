pub mod default_bindings;

use egui::{Key, Modifiers};

use crate::command_palette::commands::Command;

pub fn shortcut_command(modifiers: &Modifiers, key: Key) -> Option<Command> {
    use Command::*;
    use Key::*;

    // On Windows/Linux egui maps `command` to Ctrl. Use an explicit chord
    // there so splits remain reachable without colliding with Review Changes.
    #[cfg(not(target_os = "macos"))]
    if modifiers.ctrl && modifiers.alt {
        match (modifiers.shift, key) {
            (false, D) => return Some(SplitRight),
            (true, D) => return Some(SplitDown),
            (_, W) => return Some(CloseLeaf),
            _ => {}
        }
    }
    // Splits (P2.11) con la tecla Cmd (no chocan con los atajos Ctrl+Shift).
    if modifiers.command && !modifiers.ctrl {
        match (modifiers.shift, key) {
            (false, D) => return Some(SplitRight),
            (true, D) => return Some(SplitDown),
            (_, W) => return Some(CloseLeaf),
            _ => {}
        }
    }

    match (modifiers.ctrl, modifiers.shift, key) {
        (true, true, T) => Some(NewTerminal),
        (true, true, A) => Some(LaunchAgent),
        (true, true, S) => Some(ShareWorkspace),
        (true, true, J) => Some(JoinSharedSession),
        (true, true, O) => Some(OpenFolder),
        (true, true, W) => Some(CloseTerminal),
        (true, true, F) => Some(SearchTerminal),
        (true, true, D) => Some(ReviewChanges),
        (true, true, E) => Some(ExportScrollback),
        (true, true, K) => Some(AttachScreenshot),
        (true, true, R) => Some(ResumeConversation),
        (true, true, Enter) => Some(BroadcastCommand),
        (true, true, OpenBracket) => Some(FocusPrev),
        (true, true, CloseBracket) => Some(FocusNext),
        (true, true, Num0) => Some(ZoomToFitAll),
        (true, false, B) => Some(ToggleSidebar),
        (true, false, P) => Some(QuickOpen),
        (true, false, Comma) => Some(OpenSettings),
        (true, false, Plus) | (true, false, Equals) => Some(ZoomIn),
        (true, false, Minus) => Some(ZoomOut),
        (true, false, Num0) => Some(ResetZoom),
        (_, _, F11) => Some(ToggleFullscreen),
        (_, _, F2) => Some(RenameTerminal),
        _ => None,
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn splits_are_reachable_when_command_is_control() {
        let mut modifiers = Modifiers {
            ctrl: true,
            command: true,
            alt: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            shortcut_command(&modifiers, Key::D),
            Some(Command::SplitRight)
        );
        modifiers.shift = true;
        assert_eq!(
            shortcut_command(&modifiers, Key::D),
            Some(Command::SplitDown)
        );
        assert_eq!(
            shortcut_command(&modifiers, Key::W),
            Some(Command::CloseLeaf)
        );
        modifiers.alt = false;
        assert_eq!(
            shortcut_command(&modifiers, Key::D),
            Some(Command::ReviewChanges)
        );
        assert_eq!(
            shortcut_command(&modifiers, Key::W),
            Some(Command::CloseTerminal)
        );
    }
}
