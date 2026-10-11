//! Causal regressions for the actual store, ledger and verified schema upgrade.

use std::path::PathBuf;

use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::hook_authority::{BirthBinding, Claim, HostEvidence, LeafKey, OwnedHookContext};
use crate::memory::hook_ledger::LedgerIntent;
use crate::memory::HandoffRequest;

#[path = "baseline_migrate_v2_fixture.rs"]
mod baseline_v2;

struct Fixture {
    path: PathBuf,
    store: super::MemoryStore,
}

impl Fixture {
    fn fresh() -> Self {
        let path = std::env::temp_dir().join(format!("tc-hook-causal-{}.sqlite", Uuid::new_v4()));
        let store = super::MemoryStore::open(&path).unwrap();
        Self { path, store }
    }

    fn empty_file() -> Self {
        let path = std::env::temp_dir().join(format!("tc-hook-causal-{}.sqlite", Uuid::new_v4()));
        let store = super::MemoryStore { path: path.clone() };
        Self { path, store }
    }

    fn key() -> LeafKey {
        LeafKey {
            workspace: Uuid::new_v4(),
            panel: Uuid::new_v4(),
            leaf: Uuid::new_v4(),
        }
    }

    fn reserve(
        &self,
        key: LeafKey,
        expected: Option<Claim>,
        birth: BirthBinding,
        owner: Uuid,
    ) -> Claim {
        self.store
            .hook_transition(
                owner,
                Uuid::new_v4(),
                &LedgerIntent::Reserve {
                    expected,
                    key,
                    birth,
                    owner,
                },
            )
            .unwrap()
            .claim
            .unwrap()
    }

    fn active(&self, key: LeafKey, owner: Uuid) -> Claim {
        let a = self.reserve(
            key,
            None,
            BirthBinding {
                runtime: Uuid::new_v4(),
                generation: Uuid::new_v4(),
            },
            owner,
        );
        self.store
            .hook_consume_birth_once(a)
            .unwrap()
            .unwrap()
            .run(|_| ());
        self.store
            .hook_transition(
                owner,
                Uuid::new_v4(),
                &LedgerIntent::Install {
                    expected: a,
                    host: HostEvidence {
                        runtime: a.runtime,
                        generation: a.generation,
                        target: Some(key.into()),
                    },
                },
            )
            .unwrap()
            .claim
            .unwrap()
    }

    fn snapshot(&self) -> Vec<Vec<String>> {
        let conn = Connection::open(&self.path).unwrap();
        [
            "projects",
            "tasks",
            "memories",
            "memory_revisions",
            "handoffs",
            "mutation_receipts",
            "tc_hook_leaf_authority",
            "tc_hook_generations",
            "tc_hook_panel_legacy_policy",
            "tc_hook_operation_receipts",
        ]
        .into_iter()
        .map(|table| {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let columns = stmt.column_count();
            let rows = stmt
                .query_map([], |row| {
                    Ok((0..columns)
                        .map(|i| format!("{:?}", row.get_ref(i).unwrap()))
                        .collect::<Vec<_>>()
                        .join("|"))
                })
                .unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
        })
        .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Source only: tests own these unique temp files, never a user/profile DB.
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(PathBuf::from(format!("{}-wal", self.path.display())));
        let _ = std::fs::remove_file(PathBuf::from(format!("{}-shm", self.path.display())));
    }
}

#[test]
fn consumed_permit_revalidates_retirement_before_delayed_factory() {
    let fixture = Fixture::fresh();
    let owner = Uuid::new_v4();
    let a = fixture.reserve(
        Fixture::key(),
        None,
        BirthBinding {
            runtime: Uuid::new_v4(),
            generation: Uuid::new_v4(),
        },
        owner,
    );
    let permit = fixture.store.hook_consume_birth_once(a).unwrap().unwrap();
    fixture
        .store
        .hook_transition(
            owner,
            Uuid::new_v4(),
            &LedgerIntent::FenceRetire { expected: a },
        )
        .unwrap();
    let before = fixture.snapshot();
    let mut called = false;
    assert!(permit
        .run_guarded(&fixture.store, |_| {
            called = true;
            Ok(())
        })
        .is_err());
    assert!(!called, "retirement wins before delayed factory begins");
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn first_session_start_ticket_survives_same_birth_phase_activation() {
    use crate::hook_authority::{Denial, HookAuthority, QueryIdentity};
    let fixture = Fixture::fresh();
    let owner = Uuid::new_v4();
    let a = fixture.reserve(
        Fixture::key(),
        None,
        BirthBinding {
            runtime: Uuid::new_v4(),
            generation: Uuid::new_v4(),
        },
        owner,
    );
    let authority = HookAuthority::default();
    authority
        .install(OwnedHookContext {
            claim: a,
            legacy_alias_epoch: None,
        })
        .unwrap();
    let permit = fixture.store.hook_consume_birth_once(a).unwrap().unwrap();
    let identity = QueryIdentity::Bound {
        key: a.key,
        binding: BirthBinding {
            runtime: a.runtime,
            generation: a.generation.unwrap(),
        },
    };
    // This is the real handler authorization/owned-memory path while factory
    // has consumed but before the Attached response activates UI publication.
    let context = fixture.store.hook_resolve(identity, owner).unwrap();
    let ticket = authority.ticket(context).unwrap();
    let effects = ticket.begin_effects().unwrap();
    fixture.store.hook_validate(effects.context()).unwrap();
    assert!(matches!(
        authority.validate_ui(&ticket),
        Err(Denial::PendingBirth)
    ));
    drop(effects);
    permit.run_guarded(&fixture.store, |_| Ok(())).unwrap();
    let active = fixture
        .store
        .hook_transition(
            owner,
            Uuid::new_v4(),
            &LedgerIntent::Install {
                expected: a,
                host: HostEvidence {
                    runtime: a.runtime,
                    generation: a.generation,
                    target: Some(a.key.into()),
                },
            },
        )
        .unwrap()
        .claim
        .unwrap();
    assert_eq!(active.epoch, a.epoch);
    authority.mark_installed(active).unwrap();
    assert_eq!(authority.validate_ui(&ticket).unwrap(), active);
    fixture
        .store
        .with_hook_publication(ticket.context(), || Ok(()))
        .unwrap();
}

#[test]
fn physical_close_owner_cas_blocks_adoption_and_successor_until_finalize() {
    let fixture = Fixture::fresh();
    let owner = Uuid::new_v4();
    let a = fixture.active(Fixture::key(), owner);
    let retiring = fixture
        .store
        .hook_transition(
            owner,
            Uuid::new_v4(),
            &LedgerIntent::FenceRetire { expected: a },
        )
        .unwrap()
        .claim
        .unwrap();
    assert_eq!(
        fixture
            .store
            .hook_close_claim(a.runtime, Some(owner))
            .unwrap(),
        Some(retiring)
    );
    assert!(fixture
        .store
        .hook_close_claim(a.runtime, Some(Uuid::new_v4()))
        .is_err());
    let before = fixture.snapshot();
    assert!(fixture
        .store
        .hook_transition(
            Uuid::new_v4(),
            Uuid::new_v4(),
            &LedgerIntent::Adopt {
                expected: retiring,
                host: HostEvidence {
                    runtime: a.runtime,
                    generation: a.generation,
                    target: Some(a.key.into())
                },
                new_owner: Uuid::new_v4()
            }
        )
        .is_err());
    assert!(fixture
        .store
        .hook_transition(
            owner,
            Uuid::new_v4(),
            &LedgerIntent::Reserve {
                expected: Some(retiring),
                key: a.key,
                birth: BirthBinding {
                    runtime: Uuid::new_v4(),
                    generation: Uuid::new_v4()
                },
                owner
            }
        )
        .is_err());
    assert_eq!(fixture.snapshot(), before);
    let retired = fixture
        .store
        .hook_transition(
            owner,
            Uuid::new_v4(),
            &LedgerIntent::Retire { expected: retiring },
        )
        .unwrap()
        .claim
        .unwrap();
    fixture.reserve(
        a.key,
        Some(retired),
        BirthBinding {
            runtime: Uuid::new_v4(),
            generation: Uuid::new_v4(),
        },
        owner,
    );
}

#[test]
fn explicit_cutover_reopens_verified_backup_and_refuses_generation_loss() {
    let fixture = Fixture::empty_file();
    let mut old = Connection::open(&fixture.path).unwrap();
    baseline_v2::configure(&old).unwrap();
    baseline_v2::migrate(&mut old).unwrap();
    old.execute("INSERT INTO handoffs(id,project_id,task_id,summary,created_at) VALUES ('preserved','p','t','before-cutover',1)",[]).unwrap();
    let backup = fixture.path.with_extension("v2backup.sqlite");
    let receipt = super::MemoryStore::upgrade_with_backup(&fixture.path, &backup).unwrap();
    assert!(receipt.migration_committed);
    assert_eq!(receipt.previous_version, 2);
    assert_eq!(receipt.current_version, 3);
    let preserved =
        Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        preserved
            .query_row(
                "SELECT summary FROM handoffs WHERE id='preserved'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "before-cutover"
    );
    assert_eq!(
        preserved
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    use sha2::Digest;
    assert_eq!(
        format!(
            "{:x}",
            sha2::Sha256::digest(std::fs::read(&backup).unwrap())
        ),
        receipt.backup_sha256,
        "read-only inspection preserves verified backup bytes"
    );
    let live = super::MemoryStore::open(&fixture.path).unwrap();
    live.hook_transition(
        Uuid::from_u128(1),
        Uuid::new_v4(),
        &LedgerIntent::Reserve {
            expected: None,
            key: Fixture::key(),
            birth: BirthBinding {
                runtime: Uuid::new_v4(),
                generation: Uuid::new_v4(),
            },
            owner: Uuid::from_u128(1),
        },
    )
    .unwrap();
    let explanation = crate::memory::cutover::explain_downgrade(&fixture.path).unwrap();
    assert!(explanation.contains("refused"));
    drop(preserved);
    std::fs::remove_file(&backup).unwrap();
    std::fs::remove_file(backup.with_extension("cutover.json")).unwrap();
}

#[test]
fn app_startup_verifies_a_backup_before_upgrade_and_never_repeats_it() {
    let directory = crate::memory::test_support::temp_dir();
    let path = directory.join("memory.db");
    let mut old = Connection::open(&path).unwrap();
    baseline_v2::configure(&old).unwrap();
    baseline_v2::migrate(&mut old).unwrap();
    old.execute("INSERT INTO handoffs(id,project_id,task_id,summary,created_at) VALUES ('app-preserved','p','t','before-app-upgrade',1)", []).unwrap();
    let mut cached = old.prepare("INSERT INTO handoffs(id,project_id,task_id,summary,created_at) VALUES ('old-late','p','t','unfenced',2)").unwrap();
    assert!(
        super::MemoryStore::open(&path).is_err(),
        "generic opens retain the explicit cutover boundary"
    );
    let store = super::MemoryStore::open_for_app(&path).unwrap();
    assert!(
        cached.execute([]).is_err(),
        "the old prepared writer must hit the new schema fence"
    );
    drop(cached);
    drop(old);
    let backup_dir = directory.join("memory-backups");
    let files = std::fs::read_dir(&backup_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 2);
    let backup = files
        .iter()
        .find(|file| {
            file.extension()
                .is_some_and(|extension| extension == "sqlite3")
        })
        .unwrap();
    let snapshot =
        Connection::open_with_flags(backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        snapshot
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        2
    );
    assert_eq!(
        snapshot
            .query_row(
                "SELECT summary FROM handoffs WHERE id='app-preserved'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "before-app-upgrade"
    );
    let evidence: serde_json::Value =
        serde_json::from_slice(&std::fs::read(backup.with_extension("cutover.json")).unwrap())
            .unwrap();
    use sha2::Digest;
    assert_eq!(
        format!("{:x}", sha2::Sha256::digest(std::fs::read(backup).unwrap())),
        evidence["backup_sha256"].as_str().unwrap()
    );
    assert_eq!(
        store
            .with_conn(|conn| Ok(conn.query_row(
                "SELECT MAX(version) FROM schema_migrations",
                [],
                |row| row.get::<_, i64>(0)
            )?))
            .unwrap(),
        3
    );
    assert_eq!(
        store
            .with_conn(|conn| Ok(conn.query_row(
                "SELECT summary FROM handoffs WHERE id='app-preserved'",
                [],
                |row| row.get::<_, String>(0)
            )?))
            .unwrap(),
        "before-app-upgrade"
    );
    super::MemoryStore::open_for_app(&path).unwrap();
    assert_eq!(
        std::fs::read_dir(&backup_dir).unwrap().count(),
        2,
        "reopening schema 3 creates no additional backup"
    );
    drop(snapshot);
    drop(store);
    for file in files {
        std::fs::remove_file(file).unwrap();
    }
    std::fs::remove_dir(backup_dir).unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(directory.join(format!("memory.db{suffix}")));
    }
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn app_startup_preserves_a_verified_v1_backup_and_fences_the_old_prepared_writer() {
    let directory = crate::memory::test_support::temp_dir();
    let path = directory.join("memory.db");
    let mut old = Connection::open(&path).unwrap();
    baseline_v2::configure(&old).unwrap();
    baseline_v2::migrate(&mut old).unwrap();
    old.execute_batch(
        "DELETE FROM schema_migrations; INSERT INTO schema_migrations VALUES (1);
         DROP INDEX tasks_project_orchestrator;",
    )
    .unwrap();
    old.execute(
        "INSERT INTO handoffs(id,project_id,task_id,summary,created_at)
         VALUES ('v1-preserved','p','t','before-v1-upgrade',1)",
        [],
    )
    .unwrap();
    let logical_before = crate::memory::cutover::logical_digest(&old).unwrap();
    let mut cached = old
        .prepare(
            "INSERT INTO handoffs(id,project_id,task_id,summary,created_at)
         VALUES ('v1-old-late','p','t','old-writer',2)",
        )
        .unwrap();
    assert!(super::MemoryStore::open(&path).is_err());
    assert_eq!(
        old.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1,
        "a generic open must leave the old schema intact"
    );
    let store = super::MemoryStore::open_for_app(&path).unwrap();
    assert!(
        cached.execute([]).is_err(),
        "a prepared old writer must hit the committed v3 fence"
    );
    drop(cached);
    drop(old);
    let backup_dir = directory.join("memory-backups");
    let files = std::fs::read_dir(&backup_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 2);
    let backup = files
        .iter()
        .find(|file| file.extension().is_some_and(|ext| ext == "sqlite3"))
        .unwrap();
    assert!(backup
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("memory-v1-"));
    let snapshot =
        Connection::open_with_flags(backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        snapshot
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        crate::memory::cutover::logical_digest(&snapshot).unwrap(),
        logical_before
    );
    let evidence: serde_json::Value =
        serde_json::from_slice(&std::fs::read(backup.with_extension("cutover.json")).unwrap())
            .unwrap();
    assert_eq!(evidence["previous_version"].as_i64(), Some(1));
    assert_eq!(
        evidence["logical_sha256"].as_str(),
        Some(logical_before.as_str())
    );
    use sha2::Digest;
    assert_eq!(
        format!("{:x}", sha2::Sha256::digest(std::fs::read(backup).unwrap())),
        evidence["backup_sha256"].as_str().unwrap()
    );
    assert_eq!(
        store
            .with_conn(|conn| Ok(conn.query_row(
                "SELECT MAX(version) FROM schema_migrations",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        3
    );
    assert_eq!(
        store
            .with_conn(|conn| Ok(conn.query_row(
                "SELECT summary FROM handoffs WHERE id='v1-preserved'",
                [],
                |r| r.get::<_, String>(0)
            )?))
            .unwrap(),
        "before-v1-upgrade"
    );
    super::MemoryStore::open_for_app(&path).unwrap();
    assert_eq!(
        std::fs::read_dir(&backup_dir).unwrap().count(),
        2,
        "a schema 3 reopen must not repeat the backup"
    );
    drop(snapshot);
    drop(store);
    for file in files {
        std::fs::remove_file(file).unwrap();
    }
    std::fs::remove_dir(backup_dir).unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(directory.join(format!("memory.db{suffix}")));
    }
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn real_old_v2_connection_and_cached_statement_are_fenced_after_cutover() {
    let fixture = Fixture::empty_file();
    let mut old = Connection::open(&fixture.path).unwrap();
    baseline_v2::configure(&old).unwrap();
    baseline_v2::migrate(&mut old).unwrap();
    old.execute("INSERT INTO handoffs(id,project_id,task_id,summary,created_at) VALUES ('before-old','p','t','valid-v2',1)",[]).unwrap();
    let mut cached = old
        .prepare(
            "INSERT INTO handoffs(id,project_id,task_id,summary,created_at)
        VALUES ('late-old','p','t','old',1)",
        )
        .unwrap();
    let mut new = Connection::open(&fixture.path).unwrap();
    super::configure(&new).unwrap();
    // Explicit test-only authorization; production migrate refuses automatic v2 upgrade.
    super::migrate_impl(&mut new, true).unwrap();
    let before = fixture.snapshot();
    let error = cached.execute([]).unwrap_err().to_string();
    assert!(
        error.contains("tc_memory_writer_v3"),
        "cached old writer must hit new schema fence: {error}"
    );
    assert_eq!(fixture.snapshot(), before);
    drop(cached);
    let mut newly_opened_old = Connection::open(&fixture.path).unwrap();
    baseline_v2::configure(&newly_opened_old).unwrap();
    assert!(baseline_v2::migrate(&mut newly_opened_old).is_err());
}

#[test]
fn default_open_does_not_silently_upgrade_a_real_v2_database() {
    let fixture = Fixture::empty_file();
    let mut old = Connection::open(&fixture.path).unwrap();
    baseline_v2::configure(&old).unwrap();
    baseline_v2::migrate(&mut old).unwrap();
    assert!(super::MemoryStore::open(&fixture.path).is_err());
    assert_eq!(
        old.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| row
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        2
    );
}

#[test]
fn stale_owned_handoff_context_and_search_have_zero_durable_effects() {
    let fixture = Fixture::fresh();
    let owner = Uuid::new_v4();
    let a = fixture.active(Fixture::key(), owner);
    let context = OwnedHookContext {
        claim: a,
        legacy_alias_epoch: None,
    };
    fixture
        .store
        .hook_transition(owner, Uuid::new_v4(), &LedgerIntent::Retire { expected: a })
        .unwrap();
    let before = fixture.snapshot();
    let cwd = std::env::temp_dir();
    assert!(fixture
        .store
        .load_visible_active_owned(&cwd, None, Some(a.key.workspace), &context)
        .is_err());
    assert_eq!(fixture.snapshot(), before);
    assert!(fixture
        .store
        .search_for_context_owned(&cwd, "anything", None, Some(a.key.workspace), &context)
        .is_err());
    assert_eq!(fixture.snapshot(), before);
    assert!(fixture
        .store
        .create_handoff_owned(
            HandoffRequest {
                cwd,
                summary: "late A".into(),
                provider: None,
                session_id: Some("sess-A".into()),
                orchestrator_task_id: None,
                ttl_secs: None
            },
            &context
        )
        .is_err());
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn one_nonce_authorizes_one_factory_call_and_retired_nonce_cannot_be_reused() {
    let fixture = Fixture::fresh();
    let key = Fixture::key();
    let owner = Uuid::new_v4();
    let birth = BirthBinding {
        runtime: Uuid::new_v4(),
        generation: Uuid::new_v4(),
    };
    let a = fixture.reserve(key, None, birth, owner);
    let mut calls = 0;
    fixture
        .store
        .hook_consume_birth_once(a)
        .unwrap()
        .unwrap()
        .run(|_| calls += 1);
    assert!(fixture.store.hook_consume_birth_once(a).unwrap().is_none());
    assert_eq!(calls, 1);
    let retired = fixture
        .store
        .hook_transition(owner, Uuid::new_v4(), &LedgerIntent::Retire { expected: a })
        .unwrap()
        .claim
        .unwrap();
    let before = fixture.snapshot();
    assert!(fixture
        .store
        .hook_transition(
            owner,
            Uuid::new_v4(),
            &LedgerIntent::Reserve {
                expected: Some(retired),
                key,
                birth,
                owner
            }
        )
        .is_err());
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn hot_release_and_new_endpoint_transfer_preserve_birth_and_fence_old_owner() {
    let fixture = Fixture::fresh();
    let owner_a = Uuid::new_v4();
    let a = fixture.active(Fixture::key(), owner_a);
    let old_context = OwnedHookContext {
        claim: a,
        legacy_alias_epoch: None,
    };
    let released = fixture
        .store
        .hook_transition(
            owner_a,
            Uuid::new_v4(),
            &LedgerIntent::Release { expected: a },
        )
        .unwrap()
        .claim
        .unwrap();
    let owner_b = Uuid::new_v4();
    let b = fixture
        .store
        .hook_transition(
            owner_b,
            Uuid::new_v4(),
            &LedgerIntent::Adopt {
                expected: released,
                host: HostEvidence {
                    runtime: a.runtime,
                    generation: a.generation,
                    target: Some(a.key.into()),
                },
                new_owner: owner_b,
            },
        )
        .unwrap()
        .claim
        .unwrap();
    assert_eq!(b.generation, a.generation);
    assert_eq!(b.runtime, a.runtime);
    assert!(b.epoch > a.epoch);
    let before = fixture.snapshot();
    assert!(fixture
        .store
        .load_visible_active_owned(&std::env::temp_dir(), None, None, &old_context)
        .is_err());
    assert!(fixture
        .store
        .hook_transition(
            owner_a,
            Uuid::new_v4(),
            &LedgerIntent::Retire { expected: a }
        )
        .is_err());
    assert_eq!(fixture.snapshot(), before);
    let current = OwnedHookContext {
        claim: b,
        legacy_alias_epoch: None,
    };
    fixture
        .store
        .load_visible_active_owned(&std::env::temp_dir(), None, None, &current)
        .unwrap();
}

#[test]
fn root_alias_closure_is_sticky_before_any_replacement_birth() {
    let fixture = Fixture::fresh();
    let owner = Uuid::new_v4();
    let key = Fixture::key();
    let a = fixture
        .store
        .hook_transition(
            owner,
            Uuid::new_v4(),
            &LedgerIntent::ImportLegacy {
                key,
                runtime: Uuid::new_v4(),
                owner,
                single_original_root: true,
            },
        )
        .unwrap()
        .claim
        .unwrap();
    let context = OwnedHookContext {
        claim: a,
        legacy_alias_epoch: Some(1),
    };
    fixture
        .store
        .load_visible_active_owned(&std::env::temp_dir(), None, None, &context)
        .unwrap();
    fixture
        .store
        .hook_transition(
            owner,
            Uuid::new_v4(),
            &LedgerIntent::CloseLegacyAlias {
                workspace: key.workspace,
                panel: key.panel,
                expected_epoch: Some(1),
                root: key.leaf,
            },
        )
        .unwrap();
    let before = fixture.snapshot();
    assert!(fixture
        .store
        .load_visible_active_owned(&std::env::temp_dir(), None, None, &context)
        .is_err());
    let promoted = LeafKey {
        leaf: Uuid::new_v4(),
        ..key
    };
    assert!(fixture
        .store
        .hook_transition(
            owner,
            Uuid::new_v4(),
            &LedgerIntent::ImportLegacy {
                key: promoted,
                runtime: Uuid::new_v4(),
                owner,
                single_original_root: true
            }
        )
        .is_err());
    fixture
        .store
        .with_immediate_tx(|tx| {
            assert!(tx
                .execute(
                    "UPDATE tc_hook_panel_legacy_policy SET legacy_root_alias_closed=0
            WHERE workspace_id=?1 AND panel_id=?2",
                    params![key.workspace.to_string(), key.panel.to_string()]
                )
                .is_err());
            Ok(())
        })
        .unwrap();
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn historical_receipt_never_reauthorizes_an_old_claim_or_changed_intent() {
    let fixture = Fixture::fresh();
    let owner = Uuid::new_v4();
    let key = Fixture::key();
    let op = Uuid::new_v4();
    let intent = LedgerIntent::Reserve {
        expected: None,
        key,
        owner,
        birth: BirthBinding {
            runtime: Uuid::new_v4(),
            generation: Uuid::new_v4(),
        },
    };
    let a = fixture
        .store
        .hook_transition(owner, op, &intent)
        .unwrap()
        .claim
        .unwrap();
    fixture
        .store
        .hook_transition(owner, Uuid::new_v4(), &LedgerIntent::Retire { expected: a })
        .unwrap();
    let before = fixture.snapshot();
    let replay = fixture
        .store
        .hook_transition(owner, op, &intent)
        .unwrap()
        .claim
        .unwrap();
    assert_eq!(replay, a);
    assert!(fixture.store.hook_consume_birth_once(replay).is_err());
    assert!(fixture
        .store
        .hook_transition(
            owner,
            op,
            &LedgerIntent::Reserve {
                expected: None,
                key,
                owner,
                birth: BirthBinding {
                    runtime: Uuid::new_v4(),
                    generation: Uuid::new_v4()
                }
            }
        )
        .is_err());
    assert_eq!(fixture.snapshot(), before);
}
