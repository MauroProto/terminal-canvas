mod terminal {
    #[path = "/Users/mauro/Desktop/proyectos/terminalcanvas/src/terminal/colors.rs"]
    pub mod colors;
    #[path = "/Users/mauro/Desktop/proyectos/terminalcanvas/src/terminal/input.rs"]
    pub mod input;
    #[path = "/Users/mauro/Desktop/proyectos/terminalcanvas/src/terminal/pty.rs"]
    pub mod pty;
}

mod utils {
    #[path = "/Users/mauro/Desktop/proyectos/terminalcanvas/src/utils/platform.rs"]
    pub mod platform;
}

#[allow(dead_code)]
#[path = "../../src/runtime/mod.rs"]
mod runtime;

use runtime::RuntimeScheduler;

#[test]
fn scheduler_coalesces_many_session_updates() {
    let mut scheduler = RuntimeScheduler::new_for_tests();
    scheduler.enqueue_output_batch(20, 50);

    let batch = scheduler.drain_ui_updates();

    assert!(batch.session_updates.len() <= 20);
    assert_eq!(batch.session_updates.len(), 20);
    assert!(batch.repaint_requested);

    let next = scheduler.drain_ui_updates();
    assert!(next.session_updates.is_empty());
    assert!(!next.repaint_requested);
}
