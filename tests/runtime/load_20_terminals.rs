mod terminal {
    #![allow(dead_code)]

    #[path = "/Users/mauro/Desktop/proyectos/terminalcanvas/src/terminal/colors.rs"]
    pub mod colors;
    #[path = "/Users/mauro/Desktop/proyectos/terminalcanvas/src/terminal/input.rs"]
    pub mod input;
    #[path = "/Users/mauro/Desktop/proyectos/terminalcanvas/src/terminal/pty.rs"]
    pub mod pty;
}

mod utils {
    #![allow(dead_code)]

    #[path = "/Users/mauro/Desktop/proyectos/terminalcanvas/src/utils/platform.rs"]
    pub mod platform;
}

#[allow(dead_code)]
#[path = "../../src/runtime/mod.rs"]
mod runtime;

#[path = "fixtures/mod.rs"]
mod fixtures;

use fixtures::RuntimeHarness;

#[test]
fn load_test_twenty_terminals_keeps_runtime_consistent() {
    let mut harness = RuntimeHarness::new();
    harness.seed(5, 20);
    harness.emit_output_bursts();
    harness.step();

    assert_eq!(harness.session_count(), 20);
    assert!(harness.no_deadlocks());
    assert!(harness.snapshot_is_consistent());
    assert!(harness.has_mixed_render_tiers());
    assert!(harness.every_session_is_registered_in_seeded_workspace());
}
