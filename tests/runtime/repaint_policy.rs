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

    // El stream enfocado repinta más rápido que el fondo (16 ms vs 33 ms):
    // la prioridad sigue la atención del usuario.
    policy.note_focused_runtime_event();
    assert!(!policy.should_repaint_now_at(now + Duration::from_millis(8)));
    assert!(policy.should_repaint_now_at(now + Duration::from_millis(18)));
}

#[test]
fn repaint_policy_focused_window_is_tighter_than_background() {
    let now = Instant::now();
    let mut policy = RepaintPolicy::new(Duration::from_millis(33));

    policy.note_runtime_event();
    assert!(policy.should_repaint_now_at(now));
    policy.note_runtime_event();
    assert!(
        !policy.should_repaint_now_at(now + Duration::from_millis(18)),
        "el fondo todavía está dentro de su ventana de 33 ms"
    );

    policy.note_focused_runtime_event();
    assert!(
        policy.should_repaint_now_at(now + Duration::from_millis(18)),
        "el foco pasa a su ventana corta en cuanto llega un evento enfocado"
    );
}
