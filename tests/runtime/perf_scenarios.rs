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
fn smoke_budget_single_visible_terminal_stays_focused_and_stable() {
    let mut harness = RuntimeHarness::new();
    harness.seed_budget(1, 1, 0);
    harness.emit_output_bursts();
    harness.step();

    assert_eq!(harness.session_count(), 1);
    assert!(harness.no_deadlocks());
    assert!(harness.snapshot_is_consistent());
    assert_eq!(harness.render_tier_counts(), (1, 0, 0, 0));
}

#[test]
fn smoke_budget_four_open_two_visible_one_streaming_uses_preview_and_hidden_tiers() {
    let mut harness = RuntimeHarness::new();
    harness.seed_budget(4, 2, 1);
    harness.emit_output_bursts();
    harness.step();

    assert_eq!(harness.session_count(), 4);
    assert!(harness.no_deadlocks());
    assert!(harness.snapshot_is_consistent());
    assert_eq!(harness.render_tier_counts(), (1, 0, 1, 2));
}

#[test]
fn smoke_budget_twenty_open_six_visible_three_streaming_hits_target_shape() {
    let mut harness = RuntimeHarness::new();
    harness.seed_budget(20, 6, 3);
    harness.emit_output_bursts();
    harness.step();

    assert_eq!(harness.session_count(), 20);
    assert!(harness.no_deadlocks());
    assert!(harness.snapshot_is_consistent());
    assert_eq!(harness.render_tier_counts(), (1, 2, 3, 14));
}
