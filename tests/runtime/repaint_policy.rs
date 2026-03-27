#[allow(dead_code)]
#[path = "../../src/update.rs"]
mod update;

use std::time::{Duration, Instant};

use update::RepaintPolicy;

#[test]
fn repaint_policy_batches_bursty_runtime_events() {
    let mut policy = RepaintPolicy::new(Duration::from_millis(33));
    policy.note_runtime_event();
    policy.note_runtime_event();

    assert!(policy.should_repaint_now());
    assert!(!policy.should_repaint_now());
}

#[test]
fn repaint_policy_caps_background_repaint_frequency() {
    let now = Instant::now();
    let mut policy = RepaintPolicy::new(Duration::from_millis(33));
    policy.note_runtime_event();
    assert!(policy.should_repaint_now_at(now));

    policy.note_runtime_event();
    assert!(!policy.should_repaint_now_at(now + Duration::from_millis(8)));
    assert!(policy.should_repaint_now_at(now + Duration::from_millis(40)));
}

#[test]
fn repaint_policy_keeps_focused_terminal_responsive() {
    let now = Instant::now();
    let mut policy = RepaintPolicy::new(Duration::from_millis(33));
    policy.note_focused_runtime_event();
    assert!(policy.should_repaint_now_at(now));

    policy.note_focused_runtime_event();
    assert!(!policy.should_repaint_now_at(now + Duration::from_millis(8)));
    assert!(policy.should_repaint_now_at(now + Duration::from_millis(18)));
}
