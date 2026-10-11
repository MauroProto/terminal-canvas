//! Actual local retirement and durable ownership boundaries; no OS child is spawned.
use super::*;
use crate::terminal::pty::PtyHandle;
use std::io::{self, Read};
use std::path::PathBuf;
use std::time::Instant;

struct Fixture {
    runtime: HookRuntime,
    work: Receiver<Work>,
    _publications: Receiver<PublicationJob>,
    directory: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("tc-local-retirement-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let store = MemoryStore::open(directory.join("memory.db")).unwrap();
        let (work_tx, work) = std::sync::mpsc::sync_channel(64);
        let (publication_tx, publications) = std::sync::mpsc::sync_channel(256);
        Self {
            runtime: HookRuntime {
                owner: Uuid::new_v4(),
                authority: Arc::new(HookAuthority::default()),
                store: Some(store),
                claims: Mutex::new(HashMap::new()),
                closing: Mutex::new(HashMap::new()),
                cancelled: Mutex::new(HashSet::new()),
                aliases: Mutex::new(HashMap::new()),
                work: work_tx,
                publications: publication_tx,
                birth_admission_closed: Mutex::new(false),
                local_factories_pending: Mutex::new(HashSet::new()),
                local_queued: Mutex::new(HashMap::new()),
                observed_local: Mutex::new(HashMap::new()),
            },
            work,
            _publications: publications,
            directory,
        }
    }
    fn key() -> LeafKey {
        LeafKey {
            workspace: Uuid::new_v4(),
            panel: Uuid::new_v4(),
            leaf: Uuid::new_v4(),
        }
    }
    fn birth(&self) -> Claim {
        let claim = self
            .runtime
            .authorize_birth(Self::key(), Uuid::new_v4())
            .unwrap();
        self.runtime
            .store()
            .unwrap()
            .hook_consume_birth_once(claim)
            .unwrap()
            .unwrap()
            .run_guarded(self.runtime.store().unwrap(), |_| Ok(()))
            .unwrap();
        claim
    }
    fn activate(&self, claim: Claim) -> Claim {
        self.runtime
            .attach(
                claim.key,
                HostEvidence {
                    runtime: claim.runtime,
                    generation: claim.generation,
                    target: Some(claim.key.into()),
                },
                false,
            )
            .unwrap()
    }
    fn next_local_retirement(&self) -> LocalRetirement {
        match self.work.try_recv().unwrap() {
            Work::CloseLocal(handle) => LocalRetirement::new(handle),
            _ => panic!("expected actual local close ownership handoff"),
        }
    }
    fn journal_output(&self, claim: Claim) -> Vec<u8> {
        let path = self
            .directory
            .join("hook-retirement")
            .join(format!("{}.mtlg", claim.generation.unwrap()));
        crate::state::scrollback_log::read_frames(&std::fs::read(path).unwrap())
            .unwrap()
            .1
            .into_iter()
            .filter(|frame| frame.kind == crate::state::scrollback_log::FrameKind::Output)
            .flat_map(|frame| frame.payload)
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // The fixture owns this newly created UUID directory, never a profile.
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn dropping_the_last_ui_reference_retains_the_actual_handle_until_durable_close() {
    let fixture = Fixture::new();
    let claim = fixture.birth();
    let mut pty = PtyHandle::for_persistence_tests(b"LAST-OUTPUT");
    pty.bind_local_hook_for_retirement_tests(claim);
    let handle = Arc::new(Mutex::new(pty));
    fixture.activate(claim);
    fixture.runtime.observe_local(&handle).unwrap();
    let weak = Arc::downgrade(&handle);
    drop(handle);
    assert!(
        weak.upgrade().is_some(),
        "the ledger worker must retain the physical handle"
    );
    let mut observed = fixture.runtime.observed_local_handles();
    assert_eq!(observed.len(), 1);
    let (actual, abandoned) = observed.pop().unwrap();
    assert!(
        abandoned,
        "detect abandonment before cloning the observer reference"
    );
    fixture.runtime.request_local_close(actual).unwrap();
    let mut retirement = fixture.next_local_retirement();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !retirement.advance(&fixture.runtime).unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(fixture.journal_output(claim), b"LAST-OUTPUT");
    assert!(fixture.runtime.local_close_completed(claim.runtime));
    assert!(fixture.runtime.observed_local.lock().unwrap().is_empty());
    assert!(fixture.runtime.finish(claim.runtime, false).unwrap());
    assert_eq!(
        fixture
            .runtime
            .store()
            .unwrap()
            .hook_claim(claim.key)
            .unwrap()
            .unwrap()
            .phase,
        ClaimPhase::Retired
    );
    drop(retirement);
    assert!(
        weak.upgrade().is_none(),
        "release the handle only after the complete durable boundary"
    );
}

#[test]
fn a_stale_local_handle_cannot_fence_a_new_generation_with_the_same_runtime_id() {
    let fixture = Fixture::new();
    let birth = fixture.birth();
    let active = fixture.activate(birth);
    let store = fixture.runtime.store().unwrap();
    let retired = store
        .hook_transition(
            fixture.runtime.owner,
            Uuid::new_v4(),
            &LedgerIntent::Retire { expected: active },
        )
        .unwrap()
        .claim
        .unwrap();
    let successor = store
        .hook_transition(
            fixture.runtime.owner,
            Uuid::new_v4(),
            &LedgerIntent::Reserve {
                expected: Some(retired),
                key: birth.key,
                birth: BirthBinding {
                    runtime: birth.runtime,
                    generation: Uuid::new_v4(),
                },
                owner: fixture.runtime.owner,
            },
        )
        .unwrap()
        .claim
        .unwrap();
    store
        .hook_consume_birth_once(successor)
        .unwrap()
        .unwrap()
        .run_guarded(store, |_| Ok(()))
        .unwrap();
    let successor = fixture.activate(successor);
    let ticket = fixture
        .runtime
        .admit(QueryIdentity::Bound {
            key: successor.key,
            binding: BirthBinding {
                runtime: successor.runtime,
                generation: successor.generation.unwrap(),
            },
        })
        .unwrap();
    let mut stale = PtyHandle::for_persistence_tests(b"OLD");
    stale.bind_local_hook_for_retirement_tests(birth);
    assert!(fixture
        .runtime
        .request_local_close(Arc::new(Mutex::new(stale)))
        .is_err());
    assert_eq!(store.hook_claim(successor.key).unwrap(), Some(successor));
    assert_eq!(
        fixture
            .runtime
            .current_claim_for_runtime(successor.key, successor.runtime),
        Some(successor)
    );
    assert_eq!(
        fixture.runtime.authority.validate_ui(&ticket).unwrap(),
        successor
    );
    assert!(fixture.runtime.closing.lock().unwrap().is_empty());
    assert!(!fixture
        .runtime
        .cancelled
        .lock()
        .unwrap()
        .contains(&successor.runtime));
    assert!(fixture.work.try_recv().is_err());
}

#[test]
fn local_retirement_waits_for_real_eof_wait_success_and_the_durable_final_tail() {
    struct Reader {
        step: usize,
        events: SyncSender<&'static str>,
        release: Receiver<()>,
    }
    impl Read for Reader {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.step += 1;
            match self.step {
                1 => {
                    bytes[..6].copy_from_slice(b"PREFIX");
                    Ok(6)
                }
                2 => {
                    self.events.send("before-pause").unwrap();
                    self.release.recv().unwrap();
                    Err(io::ErrorKind::Interrupted.into())
                }
                3 => {
                    self.events.send("final-read-admitted").unwrap();
                    self.release.recv().unwrap();
                    bytes[..4].copy_from_slice(b"TAIL");
                    Ok(4)
                }
                _ => Ok(0),
            }
        }
    }
    let fixture = Fixture::new();
    let birth = fixture.birth();
    let (events_tx, events_rx) = std::sync::mpsc::sync_channel(4);
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (child_tx, child_rx) = std::sync::mpsc::channel();
    let (wait_tx, wait_rx) = std::sync::mpsc::channel();
    let mut pty = PtyHandle::for_retirement_boundary_tests(
        Box::new(Reader {
            step: 0,
            events: events_tx,
            release: release_rx,
        }),
        child_tx,
        wait_rx,
    );
    pty.bind_local_hook_for_retirement_tests(birth);
    let handle = Arc::new(Mutex::new(pty));
    fixture.activate(birth);
    fixture.runtime.observe_local(&handle).unwrap();
    assert_eq!(
        events_rx.recv_timeout(Duration::from_secs(30)).unwrap(),
        "before-pause"
    );
    fixture
        .runtime
        .request_local_close(Arc::clone(&handle))
        .unwrap();
    let mut retirement = fixture.next_local_retirement();
    assert!(!retirement.advance(&fixture.runtime).unwrap());
    assert!(
        child_rx.try_recv().is_err(),
        "no termination before durable prefix"
    );
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !retirement.draining {
        assert!(!retirement.advance(&fixture.runtime).unwrap());
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(fixture.journal_output(birth), b"PREFIX");
    assert_eq!(
        child_rx.recv_timeout(Duration::from_secs(30)).unwrap(),
        "terminate"
    );
    assert_eq!(
        child_rx.recv_timeout(Duration::from_secs(30)).unwrap(),
        "wait"
    );
    assert_eq!(
        events_rx.recv_timeout(Duration::from_secs(30)).unwrap(),
        "final-read-admitted"
    );
    wait_tx
        .send(Err(io::Error::other("unconfirmed native wait")))
        .unwrap();
    assert_eq!(
        child_rx.recv_timeout(Duration::from_secs(30)).unwrap(),
        "terminate"
    );
    assert_eq!(
        child_rx.recv_timeout(Duration::from_secs(30)).unwrap(),
        "wait"
    );
    assert!(!retirement.advance(&fixture.runtime).unwrap());
    assert!(!fixture.runtime.local_close_completed(birth.runtime));
    wait_tx.send(Ok(())).unwrap();
    assert!(
        !retirement.advance(&fixture.runtime).unwrap(),
        "an empty tail before EOF is not a close acknowledgement"
    );
    assert_eq!(fixture.journal_output(birth), b"PREFIX");
    assert_eq!(
        fixture
            .runtime
            .store()
            .unwrap()
            .hook_claim(birth.key)
            .unwrap()
            .unwrap()
            .phase,
        ClaimPhase::Retiring
    );
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !retirement.advance(&fixture.runtime).unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(fixture.journal_output(birth), b"PREFIXTAIL");
    let (grid, pending_bytes) = handle
        .lock()
        .unwrap()
        .checkpoint_snapshot(final_grid)
        .unwrap();
    assert!(grid.contains("PREFIXTAIL"));
    assert_eq!(pending_bytes, 0);
    assert!(fixture.runtime.local_close_completed(birth.runtime));
    assert!(fixture.runtime.finish(birth.runtime, false).unwrap());
    assert_eq!(
        fixture
            .runtime
            .store()
            .unwrap()
            .hook_claim(birth.key)
            .unwrap()
            .unwrap()
            .phase,
        ClaimPhase::Retired
    );
}

#[test]
fn a_delayed_authority_refresh_keeps_the_successor_registry_and_ui_ticket() {
    let fixture = Fixture::new();
    let birth = fixture.birth();
    let active = fixture.activate(birth);
    let old_snapshot = OwnedHookContext {
        claim: active,
        legacy_alias_epoch: None,
    };
    let store = fixture.runtime.store().unwrap();
    let retired = store
        .hook_transition(
            fixture.runtime.owner,
            Uuid::new_v4(),
            &LedgerIntent::Retire { expected: active },
        )
        .unwrap()
        .claim
        .unwrap();
    let successor = store
        .hook_transition(
            fixture.runtime.owner,
            Uuid::new_v4(),
            &LedgerIntent::Reserve {
                expected: Some(retired),
                key: active.key,
                birth: BirthBinding {
                    runtime: Uuid::new_v4(),
                    generation: Uuid::new_v4(),
                },
                owner: fixture.runtime.owner,
            },
        )
        .unwrap()
        .claim
        .unwrap();
    store
        .hook_consume_birth_once(successor)
        .unwrap()
        .unwrap()
        .run_guarded(store, |_| Ok(()))
        .unwrap();
    let successor = fixture.activate(successor);
    let ticket = fixture
        .runtime
        .admit(QueryIdentity::Bound {
            key: successor.key,
            binding: BirthBinding {
                runtime: successor.runtime,
                generation: successor.generation.unwrap(),
            },
        })
        .unwrap();
    fixture.runtime.refresh_claim(old_snapshot);
    assert_eq!(
        fixture
            .runtime
            .current_claim_for_runtime(successor.key, successor.runtime),
        Some(successor)
    );
    assert_eq!(
        fixture.runtime.authority.validate_ui(&ticket).unwrap(),
        successor
    );
    assert_eq!(store.hook_claim(successor.key).unwrap(), Some(successor));
}

fn final_grid(term: &alacritty_terminal::term::Term<crate::terminal::pty::EventProxy>) -> String {
    use alacritty_terminal::grid::Dimensions;
    (-(term.grid().history_size() as i32)..term.screen_lines() as i32)
        .flat_map(|line| {
            term.grid()[alacritty_terminal::index::Line(line)]
                .into_iter()
                .map(|cell| cell.c)
        })
        .collect()
}

#[test]
fn shutdown_fences_births_and_waits_for_the_delayed_actual_handle_and_ledger_retirement() {
    let fixture = Fixture::new();
    let birth = fixture.birth();
    fixture.runtime.begin_local_factory(birth).unwrap();
    assert!(fixture
        .runtime
        .drain_local_for_shutdown(Duration::ZERO)
        .is_err());
    assert!(fixture
        .runtime
        .authorize_birth(Fixture::key(), Uuid::new_v4())
        .is_err());

    // This factory had already entered before the barrier. Publication must
    // transfer its actual controller before the pending-factory slot disappears.
    let mut pty = PtyHandle::for_persistence_tests(b"SHUTDOWN-LAST");
    pty.bind_local_hook_for_retirement_tests(birth);
    let handle = Arc::new(Mutex::new(pty));
    fixture.activate(birth);
    fixture.runtime.observe_local(&handle).unwrap();
    assert!(fixture
        .runtime
        .local_factories_pending
        .lock()
        .unwrap()
        .is_empty());

    std::thread::scope(|scope| {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let runtime = &fixture.runtime;
        let closing = scope.spawn(move || {
            done_tx
                .send(runtime.drain_local_for_shutdown(Duration::from_secs(30)))
                .unwrap();
        });
        let work = fixture.work.recv_timeout(Duration::from_secs(30)).unwrap();
        let mut retirement = match work {
            Work::CloseLocal(actual) => LocalRetirement::new(actual),
            _ => panic!("shutdown must transfer the actual local handle"),
        };
        assert!(done_rx.try_recv().is_err());
        assert!(matches!(
            handle.lock().unwrap().recovery_snapshot(false, final_grid),
            crate::terminal::pty::RecoverySnapshot::Unavailable
        ));
        assert!(handle.lock().unwrap().with_term(|_| ()).is_none());
        let deadline = Instant::now() + Duration::from_secs(30);
        while !retirement.advance(&fixture.runtime).unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(fixture.journal_output(birth), b"SHUTDOWN-LAST");
        assert!(
            done_rx.try_recv().is_err(),
            "physical ACK alone cannot skip the ledger commit"
        );
        fixture.runtime.finish(birth.runtime, false).unwrap();
        done_rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .unwrap();
        closing.join().unwrap();
    });
    match handle.lock().unwrap().recovery_snapshot(false, final_grid) {
        crate::terminal::pty::RecoverySnapshot::Checkpoint {
            value,
            pending_bytes,
        } => {
            assert!(value.contains("SHUTDOWN-LAST"));
            assert_eq!(pending_bytes, 0);
        }
        _ => panic!("final stable grid must re-enter the normal history save path"),
    }
    assert!(handle.lock().unwrap().with_term(|_| ()).is_some());
    assert_eq!(
        fixture
            .runtime
            .store()
            .unwrap()
            .hook_claim(birth.key)
            .unwrap()
            .unwrap()
            .phase,
        ClaimPhase::Retired
    );
}

#[test]
fn failed_retirement_journal_never_publishes_a_final_grid_or_a_clean_shutdown_barrier() {
    let fixture = Fixture::new();
    let birth = fixture.birth();
    let mut pty = PtyHandle::for_persistence_tests(b"KEEP-RAW");
    pty.bind_local_hook_for_retirement_tests(birth);
    let handle = Arc::new(Mutex::new(pty));
    fixture.activate(birth);
    fixture.runtime.observe_local(&handle).unwrap();
    fixture
        .runtime
        .request_local_close(Arc::clone(&handle))
        .unwrap();
    let mut retirement = fixture.next_local_retirement();
    std::fs::write(
        fixture.directory.join("hook-retirement"),
        b"blocks directory creation",
    )
    .unwrap();
    assert!(retirement.advance(&fixture.runtime).is_err());
    assert!(!fixture.runtime.local_close_completed(birth.runtime));
    assert!(fixture
        .runtime
        .drain_local_for_shutdown(Duration::ZERO)
        .is_err());
    assert!(matches!(
        handle.lock().unwrap().recovery_snapshot(false, final_grid),
        crate::terminal::pty::RecoverySnapshot::Unavailable
    ));
    assert!(!handle
        .lock()
        .unwrap()
        .retirement_pending_snapshot()
        .unwrap()
        .unwrap()
        .is_empty());
    assert!(!fixture.runtime.observed_local.lock().unwrap().is_empty());
}
