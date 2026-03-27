#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    NewTerminal,
    OpenFolder,
    CloseTerminal,
    RenameTerminal,
    FocusNext,
    FocusPrev,
    ZoomToFitAll,
    ToggleSidebar,
    ToggleMinimap,
    ToggleGrid,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    ToggleFullscreen,
}

#[derive(Debug, Clone, Copy)]
pub struct CommandEntry {
    pub command: Command,
    pub label: &'static str,
    pub shortcut: &'static str,
}

pub const COMMANDS: &[CommandEntry] = &[
    CommandEntry {
        command: Command::NewTerminal,
        label: "New Terminal",
        shortcut: "Ctrl+Shift+T",
    },
    CommandEntry {
        command: Command::OpenFolder,
        label: "Open Folder",
        shortcut: "Ctrl+Shift+O",
    },
    CommandEntry {
        command: Command::CloseTerminal,
        label: "Close Terminal",
        shortcut: "Ctrl+Shift+W",
    },
    CommandEntry {
        command: Command::RenameTerminal,
        label: "Rename Terminal",
        shortcut: "F2",
    },
    CommandEntry {
        command: Command::FocusNext,
        label: "Focus Next",
        shortcut: "Ctrl+Shift+]",
    },
    CommandEntry {
        command: Command::FocusPrev,
        label: "Focus Prev",
        shortcut: "Ctrl+Shift+[",
    },
    CommandEntry {
        command: Command::ZoomToFitAll,
        label: "Zoom to Fit All",
        shortcut: "Ctrl+Shift+0",
    },
    CommandEntry {
        command: Command::ToggleSidebar,
        label: "Toggle Sidebar",
        shortcut: "Ctrl+B",
    },
    CommandEntry {
        command: Command::ToggleMinimap,
        label: "Toggle Minimap",
        shortcut: "Ctrl+M",
    },
    CommandEntry {
        command: Command::ToggleGrid,
        label: "Toggle Grid",
        shortcut: "Ctrl+G",
    },
    CommandEntry {
        command: Command::ZoomIn,
        label: "Zoom In",
        shortcut: "Ctrl+=",
    },
    CommandEntry {
        command: Command::ZoomOut,
        label: "Zoom Out",
        shortcut: "Ctrl+-",
    },
    CommandEntry {
        command: Command::ResetZoom,
        label: "Reset Zoom",
        shortcut: "Ctrl+0",
    },
    CommandEntry {
        command: Command::ToggleFullscreen,
        label: "Toggle Fullscreen",
        shortcut: "F11",
    },
];
