#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mi_terminal::runtime::RuntimeScheduler;
use mi_terminal::terminal::pty::{HookIdentity, PtyHandle};

#[test]
fn async_restore_keeps_new_shell_output_after_persisted_history() {
    let session_id = uuid::Uuid::new_v4();
    let handle = PtyHandle::spawn(
        None,
        80,
        24,
        session_id,
        Arc::new(Mutex::new(RuntimeScheduler::new())),
        HookIdentity::default(),
    )
    .expect("PTY");
    let live = format!("LIVE_{}", uuid::Uuid::new_v4().simple());
    let old = format!("OLD_{}", uuid::Uuid::new_v4().simple());
    handle.write_all(format!("printf '{live}\\n'\n").as_bytes());

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let seen = handle
            .with_term(|term| mi_terminal::terminal::export::scrollback_to_text(term))
            .unwrap_or_default();
        if seen.contains(&live) {
            break;
        }
        assert!(Instant::now() < deadline, "el shell no produjo {live}");
        std::thread::sleep(Duration::from_millis(10));
    }

    handle.replay_session_preserving_live(format!("{old}\r\n").as_bytes(), &[], |term| {
        mi_terminal::terminal::export::scrollback_to_ansi(term).into_bytes()
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let restored = loop {
        let seen = handle
            .with_term(|term| mi_terminal::terminal::export::scrollback_to_text(term))
            .unwrap_or_default();
        if seen.contains(&old) && seen.contains(&live) {
            break seen;
        }
        assert!(Instant::now() < deadline, "el restore no terminó: {seen:?}");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        restored.find(&old) < restored.find(&live),
        "la salida viva quedó antes del historial restaurado: {restored:?}"
    );
}
