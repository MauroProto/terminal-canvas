//! Prepared causal tests of the production cache and reconciliation methods.
//! These isolate UI identity handling; publication/SQL admission is tested by
//! the hook author's envelope/ledger fixtures. Not executed in preparation.

use super::*;
use crate::hook_authority::{Claim, ClaimPhase, LeafKey};
use crate::orchestration::{HookEvent, HookKind};

fn claim() -> Claim {
    Claim {
        key: LeafKey {
            workspace: Uuid::new_v4(),
            panel: Uuid::new_v4(),
            leaf: Uuid::new_v4(),
        },
        runtime: Uuid::new_v4(),
        generation: Some(Uuid::new_v4()),
        epoch: 1,
        owner: Some(Uuid::new_v4()),
        phase: ClaimPhase::Active,
        require_binding: true,
    }
}

fn bind(orchestrator: &mut Orchestrator, claim: Claim) -> Uuid {
    orchestrator.ensure_claimed_panel_session(
        claim.key.workspace,
        None,
        claim.key.panel,
        Some(claim.runtime),
        "Claude Code",
        Some(claim),
    )
}

fn event(claim: Claim, session: Option<&str>) -> HookEvent {
    HookEvent {
        provider: "claude".into(),
        kind: HookKind::UserPromptSubmit,
        panel_id: Some(claim.key.panel),
        workspace_id: Some(claim.key.workspace),
        leaf_id: Some(claim.key.leaf),
        session_id: session.map(str::to_owned),
        cwd: None,
    }
}

fn observation(claim: Claim) -> PanelRuntimeObservation {
    PanelRuntimeObservation {
        workspace_id: claim.key.workspace,
        panel_id: claim.key.panel,
        runtime_session_id: Some(claim.runtime),
        hook_claim: Some(claim),
        title: "Claude Code".into(),
        visible_text: "Waiting for approval to run command".into(),
        alive: true,
        recent_output: false,
        activity_revision: Some(0),
        text_snapshot_available: true,
        attached: true,
        minimized: false,
        agent_status: None,
    }
}

#[test]
fn same_panel_and_runtime_with_new_generation_gets_fresh_session_and_no_old_resume_id() {
    let mut orchestrator = Orchestrator::new();
    let a = claim();
    let old_session = bind(&mut orchestrator, a);
    orchestrator.apply_claimed_hook_event(a, &event(a, Some("conversation-A")), true, Utc::now());
    let b = Claim {
        generation: Some(Uuid::new_v4()),
        epoch: a.epoch + 1,
        ..a
    };
    orchestrator.retain_hook_claims(&HashSet::from([b]));
    let new_session = bind(&mut orchestrator, b);
    assert_ne!(old_session, new_session);
    assert_eq!(orchestrator.hook_session_id_for_claim(a), None);
    orchestrator.apply_claimed_hook_event(b, &event(b, None), true, Utc::now());
    assert_eq!(orchestrator.hook_session_id_for_claim(b), None);
    assert_eq!(
        orchestrator
            .state
            .sessions
            .iter()
            .find(|session| session.session_id == old_session)
            .unwrap()
            .panel_id,
        None
    );
}

#[test]
fn an_unfocused_leaf_hook_preserves_its_resume_id_without_dating_focused_session_activity() {
    let mut orchestrator = Orchestrator::new();
    let a = claim();
    bind(&mut orchestrator, a);
    let b = Claim {
        key: LeafKey {
            leaf: Uuid::new_v4(),
            ..a.key
        },
        runtime: Uuid::new_v4(),
        generation: Some(Uuid::new_v4()),
        ..a
    };
    let focused = bind(&mut orchestrator, b);
    let before = orchestrator
        .state
        .sessions
        .iter()
        .find(|session| session.session_id == focused)
        .unwrap()
        .last_activity_at;
    orchestrator.apply_claimed_hook_event(
        a,
        &event(a, Some("root-A")),
        true,
        before + chrono::Duration::seconds(1),
    );
    assert_eq!(orchestrator.hook_session_id_for_claim(a), Some("root-A"));
    assert_eq!(
        orchestrator
            .state
            .sessions
            .iter()
            .find(|session| session.session_id == focused)
            .unwrap()
            .last_activity_at,
        before
    );
}

#[test]
fn a_fresh_hook_for_a_different_leaf_cannot_override_the_focused_observation() {
    let mut orchestrator = Orchestrator::new();
    let a = claim();
    bind(&mut orchestrator, a);
    orchestrator.apply_claimed_hook_event(a, &event(a, Some("A")), true, Utc::now());
    let b = Claim {
        key: LeafKey {
            leaf: Uuid::new_v4(),
            ..a.key
        },
        runtime: Uuid::new_v4(),
        generation: Some(Uuid::new_v4()),
        ..a
    };
    let focused = bind(&mut orchestrator, b);
    orchestrator.apply_observations(vec![observation(b)]);
    assert_eq!(
        orchestrator
            .state
            .sessions
            .iter()
            .find(|session| session.session_id == focused)
            .unwrap()
            .status,
        AgentStatus::WaitingApproval
    );
}

#[test]
fn canonical_workspace_and_leaf_are_required_before_any_cache_or_activity_effect() {
    let mut orchestrator = Orchestrator::new();
    let a = claim();
    let session_id = bind(&mut orchestrator, a);
    let before = orchestrator.snapshot();
    let mut wrong_workspace = event(a, Some("wrong"));
    wrong_workspace.workspace_id = Some(Uuid::new_v4());
    orchestrator.apply_claimed_hook_event(a, &wrong_workspace, true, Utc::now());
    let mut missing_leaf = event(a, Some("wrong"));
    missing_leaf.leaf_id = None;
    orchestrator.apply_claimed_hook_event(a, &missing_leaf, true, Utc::now());
    orchestrator.apply_claimed_hook_event(a, &event(a, Some("dead")), false, Utc::now());
    assert_eq!(orchestrator.hook_session_id_for_claim(a), None);
    assert_eq!(orchestrator.snapshot(), before);
    assert_eq!(orchestrator.session_hook_claims.get(&session_id), Some(&a));
}

#[test]
fn owner_transfer_with_same_birth_fences_the_old_session_and_cache() {
    let mut orchestrator = Orchestrator::new();
    let a = claim();
    let old = bind(&mut orchestrator, a);
    orchestrator.apply_claimed_hook_event(a, &event(a, Some("A")), true, Utc::now());
    let b = Claim {
        owner: Some(Uuid::new_v4()),
        epoch: a.epoch + 1,
        ..a
    };
    orchestrator.retain_hook_claims(&HashSet::from([b]));
    let current = bind(&mut orchestrator, b);
    assert_ne!(old, current);
    assert_eq!(a.runtime, b.runtime);
    assert_eq!(a.generation, b.generation);
    assert_eq!(orchestrator.hook_session_id_for_claim(a), None);
    let mut busy = observation(b);
    busy.text_snapshot_available = false;
    orchestrator.apply_observations(vec![busy]);
    assert_eq!(
        orchestrator
            .state
            .sessions
            .iter()
            .find(|session| session.session_id == current)
            .unwrap()
            .status,
        AgentStatus::Idle
    );
}

#[test]
fn same_claim_hook_without_session_id_keeps_exact_t3_resume_metadata() {
    let mut orchestrator = Orchestrator::new();
    let a = claim();
    bind(&mut orchestrator, a);
    orchestrator.apply_claimed_hook_event(
        a,
        &event(a, Some("exact-conversation")),
        true,
        Utc::now(),
    );
    orchestrator.apply_claimed_hook_event(a, &event(a, None), true, Utc::now());
    assert_eq!(
        orchestrator.hook_session_id_for_claim(a),
        Some("exact-conversation")
    );
}

#[test]
fn focus_a_b_a_reuses_the_same_exact_live_claim_session_record() {
    let mut orchestrator = Orchestrator::new();
    let a = claim();
    let b = Claim {
        key: LeafKey {
            leaf: Uuid::new_v4(),
            ..a.key
        },
        runtime: Uuid::new_v4(),
        generation: Some(Uuid::new_v4()),
        ..a
    };
    let first_a = bind(&mut orchestrator, a);
    let first_b = bind(&mut orchestrator, b);
    orchestrator.retain_hook_claims(&HashSet::from([a, b]));
    let refocused_a = bind(&mut orchestrator, a);
    assert_eq!(first_a, refocused_a);
    assert_ne!(first_a, first_b);
    assert_eq!(orchestrator.state.sessions.len(), 2);
    assert_eq!(orchestrator.session_hook_claims.get(&first_a), Some(&a));
    assert_eq!(orchestrator.session_hook_claims.get(&first_b), Some(&b));
}

#[test]
fn a_retired_record_cannot_rebind_to_its_same_panel_leaf_runtime_successor() {
    let mut orchestrator = Orchestrator::new();
    let a = claim();
    let retired_record = bind(&mut orchestrator, a);
    let b = Claim {
        generation: Some(Uuid::new_v4()),
        epoch: a.epoch + 1,
        ..a
    };
    orchestrator.retain_hook_claims(&HashSet::from([b]));
    let successor = bind(&mut orchestrator, b);
    assert_ne!(retired_record, successor);
    assert_eq!(
        orchestrator
            .state
            .sessions
            .iter()
            .find(|session| session.session_id == retired_record)
            .unwrap()
            .panel_id,
        None
    );
    assert!(!orchestrator
        .session_hook_claims
        .contains_key(&retired_record));
    assert_eq!(bind(&mut orchestrator, b), successor);
    assert_eq!(orchestrator.state.sessions.len(), 2);
}

#[test]
fn reused_panel_uuid_in_another_workspace_has_independent_session_and_prune_identity() {
    let mut orchestrator = Orchestrator::new();
    let a = claim();
    let b = Claim {
        key: LeafKey {
            workspace: Uuid::new_v4(),
            ..a.key
        },
        runtime: Uuid::new_v4(),
        generation: Some(Uuid::new_v4()),
        ..a
    };
    let first = bind(&mut orchestrator, a);
    let second = bind(&mut orchestrator, b);
    assert_ne!(first, second);
    orchestrator.apply_claimed_hook_event(a, &event(a, Some("workspace-A")), true, Utc::now());
    orchestrator.apply_claimed_hook_event(b, &event(b, Some("workspace-B")), true, Utc::now());
    assert_eq!(
        orchestrator.hook_session_id_for_claim(a),
        Some("workspace-A")
    );
    assert_eq!(
        orchestrator.hook_session_id_for_claim(b),
        Some("workspace-B")
    );
    orchestrator.prune_missing_claimed_panels(&HashSet::from([(b.key.workspace, b.key.panel)]));
    assert_eq!(orchestrator.hook_session_id_for_claim(a), None);
    assert_eq!(
        orchestrator.hook_session_id_for_claim(b),
        Some("workspace-B")
    );
    assert!(orchestrator
        .state
        .sessions
        .iter()
        .any(|session| session.session_id == second));
    assert!(!orchestrator
        .state
        .sessions
        .iter()
        .any(|session| session.session_id == first));
}

#[test]
fn actual_publication_boundary_finishes_ended_owned_stop_without_touching_live_sibling_or_successor(
) {
    use crate::terminal::panel::TerminalPanel;
    let mut orchestrator = Orchestrator::new();
    let a = claim();
    let ended_session = bind(&mut orchestrator, a);
    let b = Claim {
        key: LeafKey {
            leaf: Uuid::new_v4(),
            ..a.key
        },
        runtime: Uuid::new_v4(),
        generation: Some(Uuid::new_v4()),
        ..a
    };
    let sibling_session = bind(&mut orchestrator, b);
    let sibling_before = orchestrator
        .state
        .sessions
        .iter()
        .find(|session| session.session_id == sibling_session)
        .unwrap()
        .clone();
    let stop = HookEvent {
        kind: HookKind::Stop,
        ..event(a, Some("ended-A"))
    };
    let alive = TerminalPanel::hook_publication_liveness(Some(a), a, false, stop.kind)
        .expect("exact ended Stop crosses the actual publication boundary");
    assert!(!alive);
    let now = sibling_before.last_activity_at + chrono::Duration::seconds(1);
    orchestrator.apply_claimed_hook_event(a, &stop, alive, now);
    let ended = orchestrator
        .state
        .sessions
        .iter()
        .find(|session| session.session_id == ended_session)
        .unwrap();
    assert_eq!(ended.status, AgentStatus::Done);
    assert_eq!(ended.last_activity_at, now);
    assert_eq!(orchestrator.hook_session_id_for_claim(a), Some("ended-A"));
    let sibling = orchestrator
        .state
        .sessions
        .iter()
        .find(|session| session.session_id == sibling_session)
        .unwrap();
    assert_eq!(sibling.status, sibling_before.status);
    assert_eq!(sibling.last_activity_at, sibling_before.last_activity_at);
    assert_eq!(
        TerminalPanel::hook_publication_liveness(Some(b), b, true, HookKind::Stop),
        Some(true)
    );
    assert_eq!(
        TerminalPanel::hook_publication_liveness(Some(a), a, false, HookKind::UserPromptSubmit),
        None
    );
    let successor = Claim {
        generation: Some(Uuid::new_v4()),
        epoch: a.epoch + 1,
        ..a
    };
    assert_eq!(
        TerminalPanel::hook_publication_liveness(Some(successor), a, false, HookKind::Stop),
        None
    );
    assert_eq!(
        TerminalPanel::hook_publication_liveness(None, a, false, HookKind::Stop),
        None
    );
}
