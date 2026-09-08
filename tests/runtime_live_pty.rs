//! Exercise the actual PTY readers, terminal grids and shared scheduler together.
use std::collections::HashSet;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use mi_terminal::runtime::{PtyManager, SessionSpec};
use mi_terminal::terminal::export::scrollback_to_text;

#[test]
fn twenty_live_terminals_deliver_output_resize_and_exit() {
    let mut manager = PtyManager::new();
    let run = uuid::Uuid::new_v4().simple().to_string();
    let mut sessions = Vec::new();
    for index in 0..20 {
        let id = manager
            .spawn(SessionSpec::default(), None, 80, 24)
            .expect("spawn real PTY");
        let marker = format!("TC_PTY_{run}_{index}");
        let handle = manager.handle(id).unwrap();
        handle
            .lock()
            .unwrap()
            .write_all(format!("echo {marker}\r").as_bytes());
        sessions.push((id, marker));
    }
    manager.set_priority_session(Some(sessions[0].0));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut observed_updates = HashSet::new();
    loop {
        for update in manager.drain_ui_updates().session_updates {
            if update.output {
                observed_updates.insert(update.session_id);
            }
        }
        let complete = sessions.iter().all(|(id, marker)| {
            manager
                .handle(*id)
                .unwrap()
                .lock()
                .unwrap()
                .with_term(|term| {
                    scrollback_to_text(term)
                        .lines()
                        .any(|line| line.trim() == marker)
                })
                .unwrap_or(false)
        });
        if complete && observed_updates.len() == sessions.len() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "PTY readers or scheduler stalled: {observed_updates:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    for (id, _) in &sessions {
        let handle = manager.handle(*id).unwrap();
        let mut handle = handle.lock().unwrap();
        handle.resize(100, 30);
        assert_eq!(
            handle.with_term(|term| (term.columns(), term.screen_lines())),
            Some((100, 30))
        );
        handle.write_all(b"exit\r");
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while sessions.iter().any(|(id, _)| manager.is_alive(*id)) {
        manager.drain_ui_updates();
        assert!(Instant::now() < deadline, "PTY exit was not observed");
        std::thread::sleep(Duration::from_millis(20));
    }
    for (id, _) in sessions {
        assert!(manager.close(id));
    }
    assert_eq!(manager.attached_session_count(), 0);
}
