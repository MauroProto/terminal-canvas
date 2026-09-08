use mi_terminal::runtime;

use runtime::{drain_order, RuntimeScheduler};
use uuid::Uuid;

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

#[test]
fn the_focused_session_is_drained_first() {
    let priority = Uuid::from_u128(999);
    let others: Vec<Uuid> = (1..=5).map(Uuid::from_u128).collect();
    let mut pending = others.clone();
    pending.push(priority);

    let order = drain_order(&pending, Some(priority), 10);
    assert_eq!(order[0], priority, "el panel enfocado va primero");
    assert_eq!(order.len(), 6);
}

#[test]
fn without_a_priority_the_order_is_stable() {
    let pending: Vec<Uuid> = (1..=4).rev().map(Uuid::from_u128).collect();
    let first = drain_order(&pending, None, 10);
    let second = drain_order(&pending, None, 10);
    assert_eq!(first, second, "el batch no puede cambiar entre frames");
    assert!(first
        .windows(2)
        .all(|pair| pair[0].as_u128() < pair[1].as_u128()));
}

#[test]
fn a_priority_that_has_nothing_pending_is_skipped() {
    let pending: Vec<Uuid> = (1..=3).map(Uuid::from_u128).collect();
    let order = drain_order(&pending, Some(Uuid::from_u128(777)), 10);
    assert_eq!(order.len(), 3, "no se inventa un update que no existe");
    assert!(!order.contains(&Uuid::from_u128(777)));
}

#[test]
fn the_priority_session_survives_a_tight_budget() {
    let priority = Uuid::from_u128(999);
    let mut pending: Vec<Uuid> = (1..=20).map(Uuid::from_u128).collect();
    pending.push(priority);

    let order = drain_order(&pending, Some(priority), 3);
    assert_eq!(
        order[0], priority,
        "el foco entra aunque el batch sea chico"
    );
    assert!(order.len() <= 4, "presupuesto respetado: {}", order.len());
}

#[test]
fn a_zero_budget_drains_nothing() {
    let pending: Vec<Uuid> = (1..=3).map(Uuid::from_u128).collect();
    assert!(drain_order(&pending, None, 0).is_empty());
}
