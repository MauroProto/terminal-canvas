mod terminal {
    #![allow(dead_code)]

    #[path = "../../../src/terminal/agent_status.rs"]
    pub mod agent_status;
    #[path = "../../../src/terminal/backend.rs"]
    pub mod backend;
    #[path = "../../../src/terminal/colors.rs"]
    pub mod colors;
    #[path = "../../../src/terminal/flow_control.rs"]
    pub mod flow_control;
    #[cfg(feature = "ghostty-vt")]
    #[path = "../../../src/terminal/ghostty_backend.rs"]
    pub mod ghostty_backend;
    #[path = "../../../src/terminal/input.rs"]
    pub mod input;
    #[path = "../../../src/terminal/metrics.rs"]
    pub mod metrics;
    #[path = "../../../src/terminal/pty.rs"]
    pub mod pty;
    #[cfg(all(unix, feature = "daemon"))]
    #[path = "../../../src/terminal/remote_session.rs"]
    pub mod remote_session;
}

#[cfg(all(unix, feature = "daemon"))]
#[path = "../../src/daemon/protocol.rs"]
pub mod daemon_protocol_impl;
#[cfg(all(unix, feature = "daemon"))]
mod daemon {
    #![allow(dead_code)]

    pub use super::daemon_protocol_impl as protocol;
}
#[path = "../../src/state/durable_write.rs"]
pub mod durable_write_impl;
#[path = "../../src/state/scrollback_log.rs"]
pub mod scrollback_log_impl;
#[path = "../../src/state/scrollback_store.rs"]
pub mod scrollback_store_impl;

mod state {
    #![allow(dead_code)]

    pub use super::durable_write_impl as durable_write;
    pub use super::scrollback_log_impl as scrollback_log;
    pub use super::scrollback_store_impl as scrollback_store;
}

#[allow(dead_code)]
#[path = "../../src/config.rs"]
mod config;

mod utils {
    #![allow(dead_code)]

    #[path = "../../../src/utils/platform.rs"]
    pub mod platform;
}

#[allow(dead_code)]
#[path = "../../src/runtime/mod.rs"]
mod runtime;

use runtime::{PtyManager, SessionSpec};

#[test]
fn pty_manager_closes_session_without_panel_object() {
    let mut manager = PtyManager::new_for_tests();
    let session_id = manager.create_detached(SessionSpec::default());

    assert!(manager.is_alive(session_id));

    manager.close(session_id);

    assert!(!manager.is_alive(session_id));
}
