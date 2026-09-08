use mi_terminal::runtime;

use runtime::{PtyManager, SessionSpec};
use uuid::Uuid;

#[test]
fn pty_manager_closes_session_without_panel_object() {
    let mut manager = PtyManager::new_for_tests();
    let session_id = manager.create_detached(SessionSpec::default());

    assert!(manager.is_alive(session_id));

    manager.close(session_id);

    assert!(!manager.is_alive(session_id));
}

#[test]
fn a_detached_session_can_keep_an_existing_id() {
    // Con el daemon hosteando, un panel restaurado tiene que poder pedir el
    // id de su sesión de la corrida anterior (P3.15, T4).
    let mut manager = PtyManager::new_for_tests();
    let wanted = Uuid::from_u128(4242);
    let id = manager.create_detached_with_id(SessionSpec::default(), Some(wanted));
    assert_eq!(id, wanted);
    assert!(manager.is_alive(id));
}

#[test]
fn an_id_already_taken_falls_back_to_a_fresh_one() {
    // Nunca se puede pisar una sesión viva: si el id ya está tomado, se genera
    // otro en vez de reemplazarla.
    let mut manager = PtyManager::new_for_tests();
    let wanted = Uuid::from_u128(4242);
    let first = manager.create_detached_with_id(SessionSpec::default(), Some(wanted));
    let second = manager.create_detached_with_id(SessionSpec::default(), Some(wanted));
    assert_eq!(first, wanted);
    assert_ne!(second, wanted, "no puede pisar la sesión existente");
    assert!(manager.is_alive(first));
    assert!(manager.is_alive(second));
}

#[test]
fn without_a_remote_spawner_sessions_stay_in_process() {
    // El default no cambió: sin daemon, todo sigue in-process.
    let manager = PtyManager::new_for_tests();
    assert!(!manager.hosts_out_of_process());
}
