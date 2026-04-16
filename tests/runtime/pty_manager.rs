mod terminal {
    #![allow(dead_code)]

    #[path = "../../../src/terminal/colors.rs"]
    pub mod colors;
    #[path = "../../../src/terminal/input.rs"]
    pub mod input;
    #[path = "../../../src/terminal/pty.rs"]
    pub mod pty;
}

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
