use mi_terminal::runtime;

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
    assert!(harness.scheduler_drained_all_updates());
    assert!(harness.snapshot_is_consistent());
    assert!(harness.has_mixed_render_tiers());
    assert!(harness.every_session_is_registered_in_seeded_workspace());
}
