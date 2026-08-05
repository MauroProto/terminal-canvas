#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    NewTerminal,
    LaunchAgent,
    ShareWorkspace,
    JoinSharedSession,
    OpenFolder,
    CloseTerminal,
    RenameTerminal,
    SearchTerminal,
    ReviewChanges,
    QuickOpen,
    OpenSettings,
    ExportScrollback,
    BroadcastCommand,
    SharePanelPrivate,
    SharePanelVisibleOnly,
    SharePanelVisibleAndHistory,
    SharePanelControllable,
    FocusNext,
    FocusPrev,
    ZoomToFitAll,
    ToggleSidebar,
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
        command: Command::LaunchAgent,
        label: "Launch Agent",
        shortcut: "Ctrl+Shift+A",
    },
    CommandEntry {
        command: Command::ShareWorkspace,
        label: "Share Workspace",
        shortcut: "Ctrl+Shift+S",
    },
    CommandEntry {
        command: Command::JoinSharedSession,
        label: "Join Shared Session",
        shortcut: "Ctrl+Shift+J",
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
        command: Command::SearchTerminal,
        label: "Search in Terminal",
        shortcut: "Ctrl+Shift+F",
    },
    CommandEntry {
        command: Command::ReviewChanges,
        label: "Review Changes (Code Review)",
        shortcut: "Ctrl+Shift+D",
    },
    CommandEntry {
        command: Command::QuickOpen,
        label: "Quick Open File",
        shortcut: "Ctrl+P",
    },
    CommandEntry {
        command: Command::OpenSettings,
        label: "Open Settings",
        shortcut: "Ctrl+,",
    },
    CommandEntry {
        command: Command::ExportScrollback,
        label: "Export Terminal Output",
        shortcut: "Ctrl+Shift+E",
    },
    CommandEntry {
        command: Command::BroadcastCommand,
        label: "Broadcast Command To Terminals",
        shortcut: "Ctrl+Shift+Enter",
    },
    CommandEntry {
        command: Command::SharePanelPrivate,
        label: "Set Panel Private",
        shortcut: "",
    },
    CommandEntry {
        command: Command::SharePanelVisibleOnly,
        label: "Set Panel Shared: Visible Only",
        shortcut: "",
    },
    CommandEntry {
        command: Command::SharePanelVisibleAndHistory,
        label: "Set Panel Shared: Visible + History",
        shortcut: "",
    },
    CommandEntry {
        command: Command::SharePanelControllable,
        label: "Set Panel Shared: Controllable",
        shortcut: "",
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
