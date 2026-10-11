//! Real process proof for the managed nonce and durable physical-close producer.
//! Each child gets its own SQLite, daemon socket and scrollback directory.

use super::*;
use crate::hook_authority::{BirthBinding, Claim, ClaimPhase, HostEvidence, LeafKey};
use crate::memory::hook_ledger::LedgerIntent;
use crate::memory::MemoryStore;
use std::process::{Child, Command, Stdio};

struct TestDaemon {
    child: Child,
    root: PathBuf,
    dir: PathBuf,
    history: PathBuf,
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The directory was exclusively created by this fixture under /tmp.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixture_root() -> PathBuf {
    let short = Uuid::new_v4().simple().to_string();
    let root = PathBuf::from("/tmp").join(format!("tcm-{}", &short[..16]));
    std::fs::create_dir(&root).unwrap();
    root
}

fn start(root: PathBuf, db: &Path) -> (TestDaemon, String) {
    let dir = root.join("d");
    let history = root.join("history");
    let mut binary = std::env::current_exe().unwrap();
    binary.pop();
    if binary.ends_with("deps") {
        binary.pop();
    }
    binary.push("mi-terminal-daemon");
    assert!(
        binary.is_file(),
        "real daemon binary is required: {}",
        binary.display()
    );
    let child = Command::new(binary)
        .env("MI_TERMINAL_DAEMON_DIR", &dir)
        .env("MI_TERMINAL_SCROLLBACK_DIR", &history)
        .env("TC_MEMORY_DB", db)
        .env("SHELL", "/bin/bash")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = TestDaemon {
        child,
        root,
        dir,
        history,
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(stream) = UnixStream::connect(super::super::protocol::socket_path(&daemon.dir)) {
            drop(stream);
            break;
        }
        assert!(
            daemon.child.try_wait().unwrap().is_none(),
            "daemon exited before listening"
        );
        assert!(
            Instant::now() < deadline,
            "daemon socket did not become ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let token = super::super::protocol::ensure_token(&daemon.dir).unwrap();
    (daemon, token)
}

struct WireClient {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl WireClient {
    fn connect(
        daemon: &TestDaemon,
        token: &str,
        owner: Uuid,
    ) -> (Self, super::super::protocol::BirthCapabilities) {
        let (stream, caps) = super::super::client::DaemonConn::connect_raw_with_capabilities_as(
            &daemon.dir,
            token,
            owner,
        )
        .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        (
            Self {
                writer: stream.try_clone().unwrap(),
                reader: BufReader::new(stream),
            },
            caps,
        )
    }
    fn send(&mut self, request: &Request) {
        self.writer
            .write_all(super::super::protocol::encode_line(request).as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }
    fn next(&mut self) -> Response {
        let line = read_protocol_line(&mut self.reader)
            .unwrap()
            .expect("host stream remains framed");
        decode_line(&line).expect("real host response is valid NDJSON")
    }
    fn request(&mut self, request: &Request) -> Response {
        self.send(request);
        loop {
            let response = self.next();
            if !super::super::client::is_pushed_event(&response) {
                return response;
            }
        }
    }
}

fn transition(store: &MemoryStore, owner: Uuid, intent: LedgerIntent) -> Claim {
    store
        .hook_transition(owner, Uuid::new_v4(), &intent)
        .unwrap()
        .claim
        .unwrap()
}

fn process_alive(pid: i32) -> bool {
    assert!(pid > 0);
    // SAFETY: signal 0 only asks the kernel about this positive child PID.
    // It does not send a signal, modify memory or terminate any process.
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return true;
    }
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    false
}

#[test]
fn real_managed_birth_transfer_and_close_confirm_native_wait_eof_and_durable_output() {
    const OUTPUT_BYTES: usize = 512 * 1024;
    let root = fixture_root();
    let db = root.join("memory.db");
    let store = MemoryStore::open(&db).unwrap();
    let owner_a = Uuid::new_v4();
    let owner_b = Uuid::new_v4();
    let id = Uuid::new_v4();
    let key = LeafKey {
        workspace: Uuid::new_v4(),
        panel: Uuid::new_v4(),
        leaf: Uuid::new_v4(),
    };
    let reserved = transition(
        &store,
        owner_a,
        LedgerIntent::Reserve {
            expected: None,
            key,
            birth: BirthBinding {
                runtime: id,
                generation: Uuid::new_v4(),
            },
            owner: owner_a,
        },
    );
    assert!(!store.hook_birth_consumed(reserved).unwrap());
    let (daemon, token) = start(root, &db);
    let (mut first, caps) = WireClient::connect(&daemon, &token, owner_a);
    assert!(
        caps.permits_hook_birth(),
        "real v3 store and physical ACK producer advertise together"
    );
    let mut spec = WireSpec {
        workspace_id: Some(key.workspace),
        panel_id: Some(key.panel),
        leaf_id: Some(key.leaf),
        ..WireSpec::default()
    };
    let denied = first.request(&Request::Spawn {
        id: Some(id),
        spec: spec.clone(),
    });
    assert_eq!(
        super::super::protocol::birth_rejected_from_response(&denied),
        Some(id)
    );
    assert!(
        !store.hook_birth_consumed(reserved).unwrap(),
        "missing nonce never enters native spawn"
    );
    spec.hook_generation = reserved.generation;
    assert_eq!(
        first.request(&Request::Spawn {
            id: Some(id),
            spec: spec.clone()
        }),
        Response::Spawned { id }
    );
    assert!(store.hook_birth_consumed(reserved).unwrap());
    // A repeated UUID after a lost reply acknowledges the same consumed factory.
    assert_eq!(
        first.request(&Request::Spawn { id: Some(id), spec }),
        Response::Spawned { id }
    );
    assert_eq!(
        first.request(&Request::List),
        Response::Sessions { ids: vec![id] }
    );
    let attached = first.request(&Request::Attach { id });
    let Response::Attached {
        id: actual,
        hook_generation,
        hook_target,
        ..
    } = attached
    else {
        panic!("managed attach did not return host identity");
    };
    assert_eq!(actual, id);
    assert_eq!(hook_generation, reserved.generation);
    let target = hook_target.unwrap();
    assert_eq!(
        (target.workspace_id, target.panel_id, target.leaf_id),
        (key.workspace, key.panel, key.leaf)
    );
    let host = HostEvidence {
        runtime: id,
        generation: hook_generation,
        target: Some(crate::hook_authority::HostHookTarget {
            workspace_id: key.workspace,
            panel_id: key.panel,
            leaf_id: key.leaf,
        }),
    };
    let active = transition(
        &store,
        owner_a,
        LedgerIntent::Install {
            expected: reserved,
            host,
        },
    );
    assert_eq!(active.phase, ClaimPhase::Active);

    let marker = format!("native_{}", Uuid::new_v4().simple());
    let (prefix, suffix) = marker.split_at(marker.len() / 2);
    let command = format!("exec python3 -u -c \"import os,sys,time;m='{prefix}'+'{suffix}';print(m+':PID='+str(os.getpid()),flush=True);sys.stdout.write('x'*{OUTPUT_BYTES});print(m+':END');print(m+':END');sys.stdout.flush();time.sleep(600)\"\n");
    assert!(
        !command.contains(&marker),
        "markers cannot originate in shell echo"
    );
    first.send(&Request::Write {
        id,
        data: command.into_bytes(),
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let end = format!("{marker}:END");
    let mut output = Vec::new();
    loop {
        if let Response::Output {
            id: actual, data, ..
        } = first.next()
        {
            assert_eq!(actual, id);
            output.extend_from_slice(&data);
        }
        assert!(
            output.len() < 2 * 1024 * 1024,
            "fixture output remains bounded"
        );
        if output
            .windows(end.len())
            .filter(|s| *s == end.as_bytes())
            .count()
            == 2
        {
            break;
        }
        assert!(Instant::now() < deadline, "managed payload did not finish");
    }
    assert!(
        output
            .split(|b| *b != b'x')
            .map(|s| s.len())
            .max()
            .unwrap_or(0)
            >= OUTPUT_BYTES
    );
    let pid_marker = format!("{marker}:PID=");
    let text = String::from_utf8_lossy(&output);
    let position = text.find(&pid_marker).unwrap() + pid_marker.len();
    let pid: i32 = text[position..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap();
    assert!(process_alive(pid));

    let (mut second, caps_b) = WireClient::connect(&daemon, &token, owner_b);
    assert!(caps_b.permits_hook_birth());
    assert!(
        matches!(second.request(&Request::Attach { id }), Response::Attached { id: actual, .. } if actual == id)
    );
    let adopted = transition(
        &store,
        owner_b,
        LedgerIntent::Adopt {
            expected: active,
            host,
            new_owner: owner_b,
        },
    );
    let closing = transition(
        &store,
        owner_b,
        LedgerIntent::FenceRetire { expected: adopted },
    );
    assert_eq!(closing.phase, ClaimPhase::Retiring);
    // Even the original native creator cannot use its stale endpoint against B.
    let stale = first.request(&Request::CloseBirth { id });
    assert_eq!(
        super::super::protocol::birth_rejected_from_response(&stale),
        Some(id)
    );
    assert!(process_alive(pid));
    assert_eq!(store.hook_claim(key).unwrap(), Some(closing));
    assert_eq!(
        second.request(&Request::List),
        Response::Sessions { ids: vec![id] }
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let response = second.request(&Request::CloseBirth { id });
        if response == (Response::Killed { id }) {
            break;
        }
        assert_eq!(response, super::super::protocol::close_pending_response(id));
        assert!(
            Instant::now() < deadline,
            "physical closure has not completed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !process_alive(pid),
        "Killed must follow actual native process reaping"
    );
    assert_eq!(
        second.request(&Request::List),
        Response::Sessions { ids: Vec::new() }
    );
    assert_eq!(
        store.hook_claim(key).unwrap(),
        Some(closing),
        "host ACK does not impersonate ledger retirement"
    );
    let history = crate::state::scrollback_store::try_load_leaf_session(
        &daemon.history,
        key.panel,
        Some(key.leaf),
    )
    .unwrap();
    let durable: Vec<u8> = history
        .checkpoint
        .iter()
        .copied()
        .chain(
            history
                .frames
                .iter()
                .filter(|frame| frame.kind == crate::state::scrollback_log::FrameKind::Output)
                .flat_map(|frame| frame.payload.iter().copied()),
        )
        .collect();
    assert!(
        durable.iter().filter(|b| **b == b'x').count() >= OUTPUT_BYTES,
        "all native payload bytes must be durable before Killed"
    );
    assert!(
        durable.windows(end.len()).any(|s| s == end.as_bytes()),
        "final output marker is durable"
    );
    let retired = transition(&store, owner_b, LedgerIntent::Retire { expected: closing });
    assert_eq!(retired.phase, ClaimPhase::Retired);
}

#[test]
fn real_daemon_with_unavailable_memory_never_advertises_or_spawns_a_managed_birth() {
    let root = fixture_root();
    let db = root.join("memory.db");
    std::fs::write(&db, b"not a SQLite database").unwrap();
    let (daemon, token) = start(root, &db);
    let owner = Uuid::new_v4();
    let (mut client, caps) = WireClient::connect(&daemon, &token, owner);
    assert!(!caps.permits_hook_birth());
    let id = Uuid::new_v4();
    let spec = WireSpec {
        workspace_id: Some(Uuid::new_v4()),
        panel_id: Some(Uuid::new_v4()),
        leaf_id: Some(Uuid::new_v4()),
        hook_generation: Some(Uuid::new_v4()),
        ..WireSpec::default()
    };
    let rejected = client.request(&Request::Spawn { id: Some(id), spec });
    assert_eq!(
        super::super::protocol::birth_rejected_from_response(&rejected),
        Some(id)
    );
    assert_eq!(
        client.request(&Request::List),
        Response::Sessions { ids: Vec::new() }
    );
    assert_eq!(std::fs::read(db).unwrap(), b"not a SQLite database");
}
