pub mod agent_status;
pub mod backend;
pub mod colors;
#[cfg(feature = "ghostty-vt")]
pub mod ghostty_backend;
pub mod input;
pub mod layout;
pub mod metrics;
pub mod panel;
pub mod pty;
pub mod renderer;
pub mod scrollbar;
pub mod search;
pub mod session_controller;

#[cfg(test)]
mod tests {
    use crate::terminal::backend::{TerminalBackendKind, TerminalBackendStatus};

    #[test]
    fn backend_defaults_to_alacritty_for_stable_runtime() {
        assert_eq!(
            TerminalBackendKind::default(),
            TerminalBackendKind::Alacritty
        );
    }

    #[test]
    fn ghostty_backend_reports_feature_gate_when_not_compiled() {
        let status = TerminalBackendStatus::ghostty();

        assert_eq!(status.kind, TerminalBackendKind::Ghostty);
        if !cfg!(feature = "ghostty-vt") {
            assert!(!status.available);
            assert!(status.reason.contains("ghostty-vt"));
        }
    }
}
