use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShareSessionId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PanelShareScope {
    Private,
    #[default]
    VisibleOnly,
    VisibleAndHistory,
    Controllable,
}

impl PanelShareScope {
    pub fn allows_visible_text(self) -> bool {
        !matches!(self, Self::Private)
    }

    pub fn allows_history(self) -> bool {
        matches!(self, Self::VisibleAndHistory | Self::Controllable)
    }

    pub fn allows_control(self) -> bool {
        matches!(self, Self::Controllable)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Private => "Private",
            Self::VisibleOnly => "Visible",
            Self::VisibleAndHistory => "History",
            Self::Controllable => "Control",
        }
    }
}

impl Default for ShareSessionId {
    fn default() -> Self {
        Self(Uuid::nil())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GuestId(pub Uuid);

impl Default for GuestId {
    fn default() -> Self {
        Self(Uuid::nil())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ParticipantId {
    Host,
    Guest(GuestId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionRole {
    Host,
    Guest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GuestConnectionState {
    #[default]
    Pending,
    Approved,
    Connected,
    Disconnected,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteCode {
    pub broker_url: String,
    pub session_id: ShareSessionId,
    pub session_secret: String,
    #[serde(default)]
    pub invite_secret: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub requires_passphrase: bool,
    #[serde(default)]
    pub tls_cert_pem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedDevice {
    pub device_id: String,
    pub last_display_name: String,
    pub approved_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestPresence {
    pub id: GuestId,
    pub display_name: String,
    pub joined_at: DateTime<Utc>,
    pub connection_state: GuestConnectionState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalControlState {
    pub terminal_id: Uuid,
    #[serde(default)]
    pub controller: Option<GuestId>,
    #[serde(default)]
    pub controller_name: Option<String>,
    #[serde(default)]
    pub queue: Vec<GuestId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedPanelSnapshot {
    pub panel_id: Uuid,
    pub title: String,
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub color: [u8; 3],
    pub z_index: u32,
    pub focused: bool,
    #[serde(default)]
    pub minimized: bool,
    pub alive: bool,
    #[serde(default)]
    pub preview_label: String,
    #[serde(default)]
    pub share_scope: PanelShareScope,
    #[serde(default)]
    pub visible_text: String,
    #[serde(default)]
    pub history_text: String,
    #[serde(default)]
    pub controller: Option<GuestId>,
    #[serde(default)]
    pub controller_name: Option<String>,
    #[serde(default)]
    pub queue_len: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedWorkspaceSnapshot {
    pub workspace_id: Uuid,
    pub workspace_name: String,
    pub generated_at: DateTime<Utc>,
    #[serde(default)]
    pub guests: Vec<GuestPresence>,
    #[serde(default)]
    pub terminal_controls: Vec<TerminalControlState>,
    #[serde(default)]
    pub panels: Vec<SharedPanelSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub guest_id: GuestId,
    pub display_name: String,
    #[serde(default)]
    pub device_id: String,
    pub requested_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinDecision {
    pub guest_id: GuestId,
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlRequest {
    pub terminal_id: Uuid,
    pub guest_id: GuestId,
    pub display_name: String,
    pub requested_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlGrant {
    pub terminal_id: Uuid,
    pub guest_id: GuestId,
    pub granted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlRevoke {
    pub terminal_id: Uuid,
    #[serde(default)]
    pub guest_id: Option<GuestId>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerializableModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub command: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SerializableKey {
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    Escape,
    Tab,
    Backspace,
    Enter,
    Space,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Minus,
    Plus,
    Equals,
    OpenBracket,
    CloseBracket,
    Backslash,
    Slash,
    Period,
    Comma,
    Semicolon,
    Quote,
    Backtick,
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

impl SerializableKey {
    pub fn from_egui(key: egui::Key) -> Option<Self> {
        use egui::Key;
        Some(match key {
            Key::ArrowDown => Self::ArrowDown,
            Key::ArrowLeft => Self::ArrowLeft,
            Key::ArrowRight => Self::ArrowRight,
            Key::ArrowUp => Self::ArrowUp,
            Key::Escape => Self::Escape,
            Key::Tab => Self::Tab,
            Key::Backspace => Self::Backspace,
            Key::Enter => Self::Enter,
            Key::Space => Self::Space,
            Key::Insert => Self::Insert,
            Key::Delete => Self::Delete,
            Key::Home => Self::Home,
            Key::End => Self::End,
            Key::PageUp => Self::PageUp,
            Key::PageDown => Self::PageDown,
            Key::Minus => Self::Minus,
            Key::Plus => Self::Plus,
            Key::Equals => Self::Equals,
            Key::OpenBracket => Self::OpenBracket,
            Key::CloseBracket => Self::CloseBracket,
            Key::Backslash => Self::Backslash,
            Key::Slash => Self::Slash,
            Key::Period => Self::Period,
            Key::Comma => Self::Comma,
            Key::Semicolon => Self::Semicolon,
            Key::Quote => Self::Quote,
            Key::Backtick => Self::Backtick,
            Key::Num0 => Self::Num0,
            Key::Num1 => Self::Num1,
            Key::Num2 => Self::Num2,
            Key::Num3 => Self::Num3,
            Key::Num4 => Self::Num4,
            Key::Num5 => Self::Num5,
            Key::Num6 => Self::Num6,
            Key::Num7 => Self::Num7,
            Key::Num8 => Self::Num8,
            Key::Num9 => Self::Num9,
            Key::A => Self::A,
            Key::B => Self::B,
            Key::C => Self::C,
            Key::D => Self::D,
            Key::E => Self::E,
            Key::F => Self::F,
            Key::G => Self::G,
            Key::H => Self::H,
            Key::I => Self::I,
            Key::J => Self::J,
            Key::K => Self::K,
            Key::L => Self::L,
            Key::M => Self::M,
            Key::N => Self::N,
            Key::O => Self::O,
            Key::P => Self::P,
            Key::Q => Self::Q,
            Key::R => Self::R,
            Key::S => Self::S,
            Key::T => Self::T,
            Key::U => Self::U,
            Key::V => Self::V,
            Key::W => Self::W,
            Key::X => Self::X,
            Key::Y => Self::Y,
            Key::Z => Self::Z,
            Key::F1 => Self::F1,
            Key::F2 => Self::F2,
            Key::F3 => Self::F3,
            Key::F4 => Self::F4,
            Key::F5 => Self::F5,
            Key::F6 => Self::F6,
            Key::F7 => Self::F7,
            Key::F8 => Self::F8,
            Key::F9 => Self::F9,
            Key::F10 => Self::F10,
            Key::F11 => Self::F11,
            Key::F12 => Self::F12,
            _ => return None,
        })
    }

    pub fn to_egui(self) -> egui::Key {
        use egui::Key;
        match self {
            Self::ArrowDown => Key::ArrowDown,
            Self::ArrowLeft => Key::ArrowLeft,
            Self::ArrowRight => Key::ArrowRight,
            Self::ArrowUp => Key::ArrowUp,
            Self::Escape => Key::Escape,
            Self::Tab => Key::Tab,
            Self::Backspace => Key::Backspace,
            Self::Enter => Key::Enter,
            Self::Space => Key::Space,
            Self::Insert => Key::Insert,
            Self::Delete => Key::Delete,
            Self::Home => Key::Home,
            Self::End => Key::End,
            Self::PageUp => Key::PageUp,
            Self::PageDown => Key::PageDown,
            Self::Minus => Key::Minus,
            Self::Plus => Key::Plus,
            Self::Equals => Key::Equals,
            Self::OpenBracket => Key::OpenBracket,
            Self::CloseBracket => Key::CloseBracket,
            Self::Backslash => Key::Backslash,
            Self::Slash => Key::Slash,
            Self::Period => Key::Period,
            Self::Comma => Key::Comma,
            Self::Semicolon => Key::Semicolon,
            Self::Quote => Key::Quote,
            Self::Backtick => Key::Backtick,
            Self::Num0 => Key::Num0,
            Self::Num1 => Key::Num1,
            Self::Num2 => Key::Num2,
            Self::Num3 => Key::Num3,
            Self::Num4 => Key::Num4,
            Self::Num5 => Key::Num5,
            Self::Num6 => Key::Num6,
            Self::Num7 => Key::Num7,
            Self::Num8 => Key::Num8,
            Self::Num9 => Key::Num9,
            Self::A => Key::A,
            Self::B => Key::B,
            Self::C => Key::C,
            Self::D => Key::D,
            Self::E => Key::E,
            Self::F => Key::F,
            Self::G => Key::G,
            Self::H => Key::H,
            Self::I => Key::I,
            Self::J => Key::J,
            Self::K => Key::K,
            Self::L => Key::L,
            Self::M => Key::M,
            Self::N => Key::N,
            Self::O => Key::O,
            Self::P => Key::P,
            Self::Q => Key::Q,
            Self::R => Key::R,
            Self::S => Key::S,
            Self::T => Key::T,
            Self::U => Key::U,
            Self::V => Key::V,
            Self::W => Key::W,
            Self::X => Key::X,
            Self::Y => Key::Y,
            Self::Z => Key::Z,
            Self::F1 => Key::F1,
            Self::F2 => Key::F2,
            Self::F3 => Key::F3,
            Self::F4 => Key::F4,
            Self::F5 => Key::F5,
            Self::F6 => Key::F6,
            Self::F7 => Key::F7,
            Self::F8 => Key::F8,
            Self::F9 => Key::F9,
            Self::F10 => Key::F10,
            Self::F11 => Key::F11,
            Self::F12 => Key::F12,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TerminalInputEvent {
    Text(String),
    Paste(String),
    Key {
        key: SerializableKey,
        modifiers: SerializableModifiers,
    },
    Scroll {
        delta: f32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuestTerminalInput {
    pub terminal_id: Uuid,
    pub events: Vec<TerminalInputEvent>,
}
