#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalBackendKind {
    #[default]
    Alacritty,
    Ghostty,
}

impl TerminalBackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alacritty => "alacritty",
            Self::Ghostty => "ghostty",
        }
    }

    #[cfg_attr(not(feature = "ghostty-vt"), allow(dead_code))]
    pub fn from_env_value(value: Option<&str>) -> Self {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("ghostty") | Some("libghostty") | Some("libghostty-vt") => Self::Ghostty,
            _ => Self::Alacritty,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalBackendStatus {
    pub kind: TerminalBackendKind,
    pub available: bool,
    pub reason: String,
    pub requires_single_thread_actor: bool,
    pub production_default: bool,
}

impl TerminalBackendStatus {
    pub fn alacritty() -> Self {
        Self {
            kind: TerminalBackendKind::Alacritty,
            available: true,
            reason: "current production backend".to_owned(),
            requires_single_thread_actor: false,
            production_default: true,
        }
    }

    pub fn ghostty() -> Self {
        ghostty_status()
    }
}

#[cfg(feature = "ghostty-vt")]
fn ghostty_status() -> TerminalBackendStatus {
    TerminalBackendStatus {
        kind: TerminalBackendKind::Ghostty,
        available: true,
        reason: "compiled with ghostty-vt feature".to_owned(),
        requires_single_thread_actor: true,
        production_default: false,
    }
}

#[cfg_attr(not(feature = "ghostty-vt"), allow(dead_code))]
pub fn requested_backend_from_env() -> TerminalBackendKind {
    TerminalBackendKind::from_env_value(std::env::var("MI_TERMINAL_BACKEND").ok().as_deref())
}

#[cfg_attr(not(feature = "ghostty-vt"), allow(dead_code))]
pub fn runtime_backend_from_env() -> TerminalBackendKind {
    match requested_backend_from_env() {
        TerminalBackendKind::Ghostty if TerminalBackendStatus::ghostty().available => {
            TerminalBackendKind::Ghostty
        }
        _ => TerminalBackendKind::Alacritty,
    }
}

#[cfg(test)]
mod tests {
    use super::TerminalBackendKind;

    #[test]
    fn env_value_accepts_ghostty_aliases() {
        assert_eq!(
            TerminalBackendKind::from_env_value(Some("ghostty")),
            TerminalBackendKind::Ghostty
        );
        assert_eq!(
            TerminalBackendKind::from_env_value(Some(" libghostty-vt ")),
            TerminalBackendKind::Ghostty
        );
    }

    #[test]
    fn env_value_defaults_to_alacritty_for_unknown_values() {
        assert_eq!(
            TerminalBackendKind::from_env_value(None),
            TerminalBackendKind::Alacritty
        );
        assert_eq!(
            TerminalBackendKind::from_env_value(Some("warp")),
            TerminalBackendKind::Alacritty
        );
    }

    #[cfg(not(feature = "ghostty-vt"))]
    #[test]
    fn runtime_backend_falls_back_to_alacritty_without_ghostty_feature() {
        unsafe {
            std::env::set_var("MI_TERMINAL_BACKEND", "ghostty");
        }
        assert_eq!(
            super::runtime_backend_from_env(),
            TerminalBackendKind::Alacritty
        );
        unsafe {
            std::env::remove_var("MI_TERMINAL_BACKEND");
        }
    }
}

#[cfg(not(feature = "ghostty-vt"))]
fn ghostty_status() -> TerminalBackendStatus {
    TerminalBackendStatus {
        kind: TerminalBackendKind::Ghostty,
        available: false,
        reason: "build without ghostty-vt feature".to_owned(),
        requires_single_thread_actor: true,
        production_default: false,
    }
}
