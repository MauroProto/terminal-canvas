pub mod default_bindings;

use egui::{Key, Modifiers};

use crate::command_palette::commands::Command;

pub fn shortcut_command(modifiers: &Modifiers, key: Key) -> Option<Command> {
    use Command::*;
    use Key::*;

    match (modifiers.ctrl, modifiers.shift, key) {
        (true, true, T) => Some(NewTerminal),
        (true, true, O) => Some(OpenFolder),
        (true, true, W) => Some(CloseTerminal),
        (true, true, OpenBracket) => Some(FocusPrev),
        (true, true, CloseBracket) => Some(FocusNext),
        (true, true, Num0) => Some(ZoomToFitAll),
        (true, false, B) => Some(ToggleSidebar),
        (true, false, M) => Some(ToggleMinimap),
        (true, false, G) => Some(ToggleGrid),
        (true, false, Plus) | (true, false, Equals) => Some(ZoomIn),
        (true, false, Minus) => Some(ZoomOut),
        (true, false, Num0) => Some(ResetZoom),
        (_, _, F11) => Some(ToggleFullscreen),
        (_, _, F2) => Some(RenameTerminal),
        _ => None,
    }
}
