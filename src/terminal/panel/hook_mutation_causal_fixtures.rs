//! Source-only fixtures. An acknowledged fence is read from an actual closed
//! ledger row in an exclusive temporary DB; no status mocks or profile DBs.

use super::*;
use crate::memory::hook_ledger::LedgerIntent;
use crate::memory::MemoryStore;

struct LedgerFixture {
    path: PathBuf,
    store: MemoryStore,
}

impl LedgerFixture {
    fn fresh() -> Self {
        let path =
            std::env::temp_dir().join(format!("tc-ui-alias-fixture-{}.sqlite", Uuid::new_v4()));
        let store = MemoryStore::open(&path).unwrap();
        Self { path, store }
    }

    fn closed(&self, key: LeafKey) -> AliasFence {
        self.store
            .hook_transition(
                Uuid::new_v4(),
                Uuid::new_v4(),
                &LedgerIntent::CloseLegacyAlias {
                    workspace: key.workspace,
                    panel: key.panel,
                    expected_epoch: None,
                    root: key.leaf,
                },
            )
            .unwrap();
        AliasFence::from_closed_ledger_for_fixture(&self.store, key).unwrap()
    }
}

impl Drop for LedgerFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(PathBuf::from(format!("{}-wal", self.path.display())));
        let _ = std::fs::remove_file(PathBuf::from(format!("{}-shm", self.path.display())));
    }
}

fn panel() -> TerminalPanel {
    let mut panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(500.0, 300.0), Color32::BLACK, 1);
    panel.workspace_id = Some(Uuid::new_v4());
    panel
}

#[test]
fn an_open_ledger_cannot_be_fabricated_into_an_acknowledged_ui_fence() {
    let ledger = LedgerFixture::fresh();
    let panel = panel();
    let snapshot = panel.hook_mutation_snapshot(panel.workspace_id.unwrap());
    assert!(AliasFence::from_closed_ledger_for_fixture(&ledger.store, snapshot.key).is_err());
    assert!(panel.pending_hook_mutation.is_none());
    assert_eq!(panel.root_leaf, snapshot.key.leaf);
}

#[test]
fn acknowledged_close_is_consumed_once_only_after_a_closed_ledger_row() {
    let ledger = LedgerFixture::fresh();
    let mut panel = panel();
    let snapshot = panel.hook_mutation_snapshot(panel.workspace_id.unwrap());
    let fence = ledger.closed(snapshot.key);
    panel.pending_hook_mutation = Some(PendingHookMutation {
        snapshot,
        fence,
        kind: HookMutationKind::CloseLeaf,
    });
    assert!(panel.poll_pending_hook_mutation());
    assert!(!panel.poll_pending_hook_mutation());
    assert_eq!(
        AliasFence::from_closed_ledger_for_fixture(&ledger.store, snapshot.key)
            .unwrap()
            .status(),
        AliasStatus::Acknowledged
    );
}

#[test]
fn root_promotion_before_consumption_rejects_the_old_ack_and_keeps_tombstone_closed() {
    let ledger = LedgerFixture::fresh();
    let mut panel = panel();
    let snapshot = panel.hook_mutation_snapshot(panel.workspace_id.unwrap());
    let fence = ledger.closed(snapshot.key);
    panel.pending_hook_mutation = Some(PendingHookMutation {
        snapshot,
        fence,
        kind: HookMutationKind::CloseLeaf,
    });
    let promoted = Uuid::new_v4();
    panel.root_leaf = promoted;
    panel.focused_leaf = promoted;
    assert!(!panel.poll_pending_hook_mutation());
    assert_eq!(panel.root_leaf, promoted);
    assert_eq!(
        AliasFence::from_closed_ledger_for_fixture(&ledger.store, snapshot.key)
            .unwrap()
            .status(),
        AliasStatus::Acknowledged
    );
}

#[test]
fn a_full_claim_mismatch_blocks_a_stale_ack_even_when_panel_leaf_runtime_are_reused() {
    let ledger = LedgerFixture::fresh();
    let mut panel = panel();
    let mut snapshot = panel.hook_mutation_snapshot(panel.workspace_id.unwrap());
    snapshot.root_claim = Some(Claim {
        key: snapshot.key,
        runtime: Uuid::new_v4(),
        generation: Some(Uuid::new_v4()),
        epoch: 1,
        owner: Some(Uuid::new_v4()),
        phase: ClaimPhase::Active,
        require_binding: true,
    });
    let fence = ledger.closed(snapshot.key);
    panel.pending_hook_mutation = Some(PendingHookMutation {
        snapshot,
        fence,
        kind: HookMutationKind::CloseLeaf,
    });
    // Current controller has no active claim. A detached/invalidated birth
    // cannot consume an ACK obtained for the old owned claim.
    assert!(!panel.poll_pending_hook_mutation());
    assert_eq!(panel.root_leaf, snapshot.key.leaf);
}

#[test]
fn selected_resume_id_waits_for_exact_ack_and_survives_replacement_metadata_clear() {
    let ledger = LedgerFixture::fresh();
    let mut panel = panel();
    panel.set_agent_session_id(Some("old-conversation".to_owned()));
    let workspace = panel.workspace_id.unwrap();
    let snapshot = panel.hook_mutation_snapshot(workspace);
    let manager = Arc::new(Mutex::new(PtyManager::new_for_tests()));
    panel.pending_hook_mutation = Some(PendingHookMutation {
        snapshot,
        fence: ledger.closed(snapshot.key),
        kind: HookMutationKind::Replace {
            manager,
            cwd: None,
            workspace,
            startup_command: "claude --resume chosen-conversation".to_owned(),
            agent_command: "claude".to_owned(),
            resume_session_id: None,
        },
    });
    assert!(!panel.stage_replacement_resume_id(Uuid::new_v4(), Some("wrong-leaf".to_owned())));
    assert!(panel.stage_replacement_resume_id(
        snapshot.focused_leaf,
        Some("chosen-conversation".to_owned())
    ));
    assert_eq!(panel.agent_session_id(), Some("old-conversation"));
    assert_eq!(
        panel.to_saved().agent_session_id.as_deref(),
        Some("old-conversation")
    );
    assert!(!panel.poll_pending_hook_mutation());
    assert!(!panel.has_pending_hook_mutation());
    assert!(panel.take_hook_mutation_error().is_none());
    assert_eq!(panel.agent_session_id(), Some("chosen-conversation"));
    assert_eq!(
        panel.to_saved().agent_session_id.as_deref(),
        Some("chosen-conversation")
    );
}
