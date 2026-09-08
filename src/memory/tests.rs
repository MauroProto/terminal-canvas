//! Pruebas del store, pack, inyección y fail-open sobre fixtures reales.

use crate::memory::context::{build_context_pack, build_context_pack_scoped, pack_contains};
use crate::memory::inject::{
    format_remember_status, launch_brief_with_memory, remember_selection, respond_to_hook,
    PromptBoundary,
};
use crate::memory::model::{
    Actor, HandoffRequest, MemoryStatus, RememberRequest, ScopeKind, TrustClass, WriteResult,
};
use crate::memory::store::MemoryStore;
use crate::memory::test_support::{clone_repo, init_git_repo, make_worktree, temp_dir};

fn store_in(root: &std::path::Path) -> MemoryStore {
    MemoryStore::open(root.join("memory.db")).expect("store")
}

#[test]
fn case_only_memory_update_preserves_the_requested_value() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);
    let original = store
        .remember(RememberRequest::human(
            repo.clone(),
            "deployment/path",
            "usar /srv/Project",
        ))
        .unwrap();
    let mut correction =
        RememberRequest::human(repo.clone(), "deployment/path", "usar /srv/project");
    correction.expected_revision = Some(original.memory().current_revision);
    let result = store.remember(correction).unwrap();
    let active = store
        .list_visible(&repo, MemoryStatus::Active)
        .unwrap()
        .into_iter()
        .find(|memory| memory.stable_key == "deployment/path")
        .unwrap();
    assert_eq!(active.content, "usar /srv/project");
    assert!(matches!(result, WriteResult::Committed(_)));
    assert_eq!(
        active.current_revision,
        original.memory().current_revision + 1
    );
}

#[test]
fn mcp_proposal_conflict_does_not_disclose_another_pending_memory() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);
    let pending = store
        .propose(RememberRequest::agent(
            repo.clone(),
            "pending/decision",
            "unreviewed content from another session",
        ))
        .unwrap();
    let response = crate::memory::handle_message_json(
        &store,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "memory_propose",
                "arguments": {
                    "cwd": repo,
                    "key": "pending/decision",
                    "content": "a different proposal"
                }
            }
        }),
    )
    .unwrap();
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    let reply: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(reply["result"], "conflict");
    assert!(reply["candidate_id"].is_string());
    assert!(!text.contains(&pending.memory().content));
    assert!(!text.contains(&pending.memory().id));
    assert!(store
        .get_visible(&repo, &pending.memory().id)
        .unwrap()
        .is_none());
}

#[test]
fn worktrees_share_project_memory_but_not_task_memory() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let worktree = root.join("wt");
    make_worktree(&repo, &worktree, "agent-b");
    let other = root.join("other");
    init_git_repo(&other);
    let clone = root.join("clone");
    clone_repo(&repo, &clone);

    let store = store_in(&root);
    let project_a = store.resolve_project(&repo).unwrap();
    let project_wt = store.resolve_project(&worktree).unwrap();
    let project_other = store.resolve_project(&other).unwrap();
    let project_clone = store.resolve_project(&clone).unwrap();
    assert_eq!(project_a.canonical_id, project_wt.canonical_id);
    assert_ne!(project_a.canonical_id, project_other.canonical_id);
    assert_ne!(
        project_a.canonical_id, project_clone.canonical_id,
        "un clone no se fusiona solo porque comparte remote"
    );

    store
        .remember(RememberRequest::human(
            repo.clone(),
            "architecture/auth-strategy",
            "Las sesiones usan cookies HttpOnly",
        ))
        .unwrap();

    let mut task_note =
        RememberRequest::human(repo.clone(), "todo/local", "solo el worktree principal");
    task_note.scope = ScopeKind::Task;
    store.remember(task_note).unwrap();

    let pack_wt = build_context_pack(&store, &worktree, Some("auth"), None).unwrap();
    assert!(
        pack_contains(&pack_wt, "cookies HttpOnly"),
        "el otro worktree lee la memoria de proyecto: {pack_wt:?}"
    );
    assert!(
        !pack_contains(&pack_wt, "solo el worktree principal"),
        "la memoria de tarea no cruza worktrees: {pack_wt:?}"
    );

    let pack_other = build_context_pack(&store, &other, Some("auth"), None).unwrap();
    assert!(
        !pack_contains(&pack_other, "cookies HttpOnly"),
        "otro repo no recibe la memoria: {pack_other:?}"
    );
    let pack_clone = build_context_pack(&store, &clone, Some("auth"), None).unwrap();
    assert!(
        !pack_contains(&pack_clone, "cookies HttpOnly"),
        "el clone sin vínculo no recibe la memoria: {pack_clone:?}"
    );
}

#[test]
fn human_remember_is_active_inferred_stays_candidate_until_approve_and_forget_drops_it() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let other_cwd = root.join("wt");
    make_worktree(&repo, &other_cwd, "second");
    let store = store_in(&root);

    let remembered = store
        .remember(RememberRequest::human(
            repo.clone(),
            "style/lang",
            "responder en español",
        ))
        .unwrap();
    assert_eq!(remembered.memory().status, MemoryStatus::Active);

    let pack = build_context_pack(&store, &other_cwd, None, None).unwrap();
    assert!(pack_contains(&pack, "responder en español"));

    let proposed = store
        .propose(RememberRequest::agent(
            repo.clone(),
            "guess/stack",
            "esto parece un monorepo",
        ))
        .unwrap();
    assert_eq!(proposed.memory().status, MemoryStatus::Candidate);
    let pack = build_context_pack(&store, &other_cwd, Some("monorepo"), None).unwrap();
    assert!(
        !pack_contains(&pack, "esto parece un monorepo"),
        "las candidatas no entran al pack: {pack:?}"
    );

    store
        .approve(proposed.memory().id.as_str(), &Actor::human())
        .unwrap();
    let pack = build_context_pack(&store, &other_cwd, Some("monorepo"), None).unwrap();
    assert!(pack_contains(&pack, "esto parece un monorepo"));

    store
        .forget(remembered.memory().id.as_str(), &Actor::human())
        .unwrap();
    let pack = build_context_pack(&store, &other_cwd, Some("español"), None).unwrap();
    assert!(!pack_contains(&pack, "responder en español"));
    let hits = store.search(&other_cwd, "español").unwrap();
    assert!(
        hits.iter()
            .all(|memory| memory.id != remembered.memory().id || !memory.status.is_retrievable()),
        "forget la saca de search: {hits:?}"
    );
}

#[test]
fn revision_at_u32_max_fails_closed_without_mutating_the_memory() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);
    let original = store
        .remember(RememberRequest::human(
            repo.clone(),
            "stability/revision-overflow",
            "valor original",
        ))
        .unwrap()
        .memory()
        .clone();
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    conn.execute(
        "UPDATE memories SET current_revision = ?1 WHERE id = ?2",
        rusqlite::params![u32::MAX, original.id],
    )
    .unwrap();
    drop(conn);

    let mut update =
        RememberRequest::human(repo, "stability/revision-overflow", "valor actualizado");
    update.expected_revision = Some(u32::MAX);
    let update_error = store.remember(update).unwrap_err().to_string();
    assert!(update_error.contains("máximo de revisiones"));

    let forget_error = store
        .forget(&original.id, &Actor::human())
        .unwrap_err()
        .to_string();
    assert!(forget_error.contains("máximo de revisiones"));

    let unchanged = store.get(&original.id).unwrap().unwrap();
    assert_eq!(unchanged.current_revision, u32::MAX);
    assert_eq!(unchanged.status, MemoryStatus::Active);
    assert_eq!(unchanged.content, "valor original");
}

#[test]
fn approving_a_conflict_replaces_the_active_value_without_changing_its_key() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);

    store
        .remember(RememberRequest::human(
            repo.clone(),
            "architecture/auth-strategy",
            "cookies HttpOnly",
        ))
        .unwrap();
    let conflict = store
        .remember(RememberRequest::human(
            repo.clone(),
            "architecture/auth-strategy",
            "tokens bearer",
        ))
        .unwrap();
    let candidate_id = match conflict {
        WriteResult::Conflict(write) => write.candidate_id.expect("candidate id"),
        other => panic!("expected conflict, got {other:?}"),
    };

    let approved = store.approve(&candidate_id, &Actor::human()).unwrap();
    assert_eq!(approved.stable_key, "architecture/auth-strategy");
    let active = store
        .list_visible(&repo, MemoryStatus::Active)
        .unwrap()
        .into_iter()
        .filter(|memory| memory.stable_key == "architecture/auth-strategy")
        .collect::<Vec<_>>();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].content, "tokens bearer");

    let pack = build_context_pack(&store, &repo, None, None).unwrap();
    assert!(pack_contains(&pack, "tokens bearer"));
    assert!(!pack_contains(&pack, "cookies HttpOnly"));
}

#[test]
fn agents_cannot_approve_reject_or_forget_memory() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);
    let proposed = store
        .propose(RememberRequest::agent(repo, "guess/stack", "parece rust"))
        .unwrap();
    let id = proposed.memory().id.clone();
    let agent = Actor::agent("mcp");

    assert!(store.approve(&id, &agent).is_err());
    assert!(store.reject(&id, &agent).is_err());
    assert!(store.forget(&id, &agent).is_err());
}

#[test]
fn agent_with_the_current_revision_still_cannot_replace_active_memory() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);
    let active = store
        .remember(RememberRequest::human(
            repo.clone(),
            "architecture/auth",
            "cookies HttpOnly",
        ))
        .unwrap()
        .memory()
        .clone();

    let mut proposal =
        RememberRequest::agent(repo.clone(), "architecture/auth", "tokens bearer inferidos");
    proposal.client_id = "agent-client".into();
    proposal.request_id = "agent-current-revision".into();
    proposal.expected_revision = Some(active.current_revision);
    let candidate_id = match store.remember(proposal).unwrap() {
        WriteResult::Conflict(conflict) => conflict.candidate_id.expect("candidate id"),
        other => panic!("an agent update must remain pending, got {other:?}"),
    };

    let active_after = store
        .list_visible(&repo, MemoryStatus::Active)
        .unwrap()
        .into_iter()
        .find(|memory| memory.stable_key == "architecture/auth")
        .expect("the approved value stays active");
    assert_eq!(active_after.id, active.id);
    assert_eq!(active_after.content, "cookies HttpOnly");

    let approved = store.approve(&candidate_id, &Actor::human()).unwrap();
    assert_eq!(approved.status, MemoryStatus::Active);
    assert_eq!(
        approved.trust_class,
        TrustClass::Agent,
        "approval changes status but must preserve provenance"
    );
}

#[test]
fn workspace_scope_uses_the_canonical_cwd_for_write_and_read() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    let non_canonical = repo.join("sub").join("..");
    let store = store_in(&root);
    let mut request = RememberRequest::human(
        non_canonical.clone(),
        "workspace/layout",
        "paneles en cuadrícula",
    );
    request.scope = ScopeKind::Workspace;
    store.remember(request).unwrap();

    let visible = store
        .list_visible(&non_canonical, MemoryStatus::Active)
        .unwrap();
    assert!(
        visible
            .iter()
            .any(|memory| memory.stable_key == "workspace/layout"),
        "equivalent path representations must resolve the same workspace scope: {visible:?}"
    );
}

#[test]
fn workspace_uuid_memory_is_visible_only_in_the_matching_app_workspace() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);
    let first_workspace = uuid::Uuid::new_v4();
    let second_workspace = uuid::Uuid::new_v4();
    let mut request = RememberRequest::human(
        repo.clone(),
        "workspace/layout",
        "este workspace usa dos paneles",
    );
    request.scope = ScopeKind::Workspace;
    request.workspace_id = Some(first_workspace);
    store.remember(request).unwrap();

    let first =
        build_context_pack_scoped(&store, &repo, None, None, Some(first_workspace)).unwrap();
    let second =
        build_context_pack_scoped(&store, &repo, None, None, Some(second_workspace)).unwrap();
    assert!(pack_contains(&first, "este workspace usa dos paneles"));
    assert!(!pack_contains(&second, "este workspace usa dos paneles"));
}

#[test]
fn context_resolution_persists_the_orchestrator_task_mapping() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);
    let task_id = uuid::Uuid::new_v4();

    build_context_pack(&store, &repo, None, Some(task_id)).unwrap();
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    let stored: String = conn
        .query_row(
            "SELECT orchestrator_task_id FROM tasks WHERE orchestrator_task_id = ?1",
            [task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, task_id.to_string());
}

#[test]
fn same_revision_double_write_is_a_visible_conflict_and_survives_reopen() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let db = root.join("memory.db");
    let store = MemoryStore::open(&db).unwrap();

    let first = store
        .remember(RememberRequest {
            client_id: "a".into(),
            request_id: "r1".into(),
            cwd: repo.clone(),
            scope: ScopeKind::Project,
            kind: crate::memory::model::MemoryKind::Decision,
            key: "architecture/auth-strategy".into(),
            content: "cookies".into(),
            actor: Actor::human(),
            expected_revision: None,
            orchestrator_task_id: None,
            workspace_id: None,
        })
        .unwrap();
    assert!(!first.is_conflict());
    let rev = first.memory().current_revision;

    let second = store
        .remember(RememberRequest {
            client_id: "b".into(),
            request_id: "r2".into(),
            cwd: repo.clone(),
            scope: ScopeKind::Project,
            kind: crate::memory::model::MemoryKind::Decision,
            key: "architecture/auth-strategy".into(),
            content: "tokens bearer".into(),
            actor: Actor::human(),
            expected_revision: Some(rev),
            orchestrator_task_id: None,
            workspace_id: None,
        })
        .unwrap();
    let third = store
        .remember(RememberRequest {
            client_id: "c".into(),
            request_id: "r3".into(),
            cwd: repo.clone(),
            scope: ScopeKind::Project,
            kind: crate::memory::model::MemoryKind::Decision,
            key: "architecture/auth-strategy".into(),
            content: "sesiones en redis".into(),
            actor: Actor::human(),
            expected_revision: Some(rev),
            orchestrator_task_id: None,
            workspace_id: None,
        })
        .unwrap();
    assert!(
        matches!(second, WriteResult::Committed(_)) || matches!(third, WriteResult::Committed(_))
    );
    assert!(second.is_conflict() || third.is_conflict());
    let winner = if second.is_conflict() {
        third.memory()
    } else {
        second.memory()
    };
    assert!(
        winner.content == "tokens bearer" || winner.content == "sesiones en redis",
        "{}",
        winner.content
    );

    let replay = store
        .remember(RememberRequest {
            client_id: "a".into(),
            request_id: "r1".into(),
            cwd: repo.clone(),
            scope: ScopeKind::Project,
            kind: crate::memory::model::MemoryKind::Decision,
            key: "architecture/auth-strategy".into(),
            content: "cookies".into(),
            actor: Actor::human(),
            expected_revision: None,
            orchestrator_task_id: None,
            workspace_id: None,
        })
        .unwrap();
    assert_eq!(replay.memory().id, first.memory().id);

    drop(store);
    let reopened = MemoryStore::open(&db).unwrap();
    let pack = build_context_pack(&reopened, &repo, Some("auth"), None).unwrap();
    assert!(
        pack_contains(&pack, "tokens bearer") || pack_contains(&pack, "sesiones en redis"),
        "el estado sobrevive un reopen: {pack:?}"
    );
}

#[test]
fn concurrent_process_style_writes_are_serialized_without_losing_entries() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);
    store.resolve_project(&repo).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
    let mut workers = Vec::new();
    for index in 0..12 {
        let worker_store = store.clone();
        let worker_repo = repo.clone();
        let worker_barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            worker_barrier.wait();
            worker_store.remember(RememberRequest {
                client_id: format!("concurrent-{index}"),
                request_id: format!("request-{index}"),
                cwd: worker_repo,
                scope: ScopeKind::Project,
                kind: crate::memory::MemoryKind::Fact,
                key: format!("concurrency/{index}"),
                content: format!("value {index}"),
                actor: Actor::human(),
                expected_revision: None,
                orchestrator_task_id: None,
                workspace_id: None,
            })
        }));
    }
    for worker in workers {
        worker.join().unwrap().unwrap();
    }

    let active = store.list_visible(&repo, MemoryStatus::Active).unwrap();
    for index in 0..12 {
        assert!(active
            .iter()
            .any(|memory| memory.stable_key == format!("concurrency/{index}")));
    }
}

#[test]
fn idempotency_key_cannot_be_reused_to_read_or_mutate_another_project() {
    let root = temp_dir();
    let first_repo = root.join("first");
    let second_repo = root.join("second");
    init_git_repo(&first_repo);
    init_git_repo(&second_repo);
    let store = store_in(&root);

    let request = |cwd: std::path::PathBuf, content: &str| RememberRequest {
        client_id: "shared-client".into(),
        request_id: "same-request-id".into(),
        cwd,
        scope: ScopeKind::Project,
        kind: crate::memory::MemoryKind::Decision,
        key: "architecture/auth".into(),
        content: content.into(),
        actor: Actor::human(),
        expected_revision: None,
        orchestrator_task_id: None,
        workspace_id: None,
    };

    store
        .remember(request(first_repo, "secret from first"))
        .unwrap();
    let error = store
        .remember(request(second_repo.clone(), "decision from second"))
        .expect_err("reused id must be rejected");
    assert!(error.to_string().contains("payload diferente"), "{error:#}");
    assert!(store
        .list_visible(&second_repo, MemoryStatus::Active)
        .unwrap()
        .is_empty());
}

#[test]
fn handoff_is_returned_only_for_the_same_task() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let other = root.join("wt");
    make_worktree(&repo, &other, "other-task");
    let store = store_in(&root);

    store
        .create_handoff(HandoffRequest {
            cwd: repo.clone(),
            summary: "auth a medio implementar, sigue con los tests".into(),
            provider: Some("claude".into()),
            session_id: Some("s1".into()),
            orchestrator_task_id: None,
            ttl_secs: Some(3600),
        })
        .unwrap();

    let pack_same = build_context_pack(&store, &repo, None, None).unwrap();
    assert!(pack_contains(&pack_same, "auth a medio implementar"));

    let pack_other = build_context_pack(&store, &other, None, None).unwrap();
    assert!(
        !pack_contains(&pack_other, "auth a medio implementar"),
        "el handoff no cruza de tarea: {pack_other:?}"
    );
}

#[test]
fn launch_and_hook_inject_only_at_safe_boundaries_and_fail_open() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);
    store
        .remember(RememberRequest::human(
            repo.clone(),
            "architecture/auth-strategy",
            "Las sesiones usan cookies HttpOnly",
        ))
        .unwrap();

    let brief = launch_brief_with_memory("implementá login", Some(&repo), Some(&store), None);
    assert!(brief.contains("implementá login"));
    assert!(brief.contains("cookies HttpOnly"));

    let without_store = launch_brief_with_memory("implementá login", Some(&repo), None, None);
    assert_eq!(without_store, "implementá login");

    let session_start = respond_to_hook(
        Some(&store),
        PromptBoundary::SessionStart,
        "SessionStart",
        Some(&repo),
        None,
        None,
    )
    .expect("SessionStart with memory returns context");
    let extra = session_start["hookSpecificOutput"]
        .get("additionalContext")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        extra.contains("cookies HttpOnly"),
        "SessionStart inyecta: {session_start}"
    );
    assert_eq!(
        session_start["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );
    assert!(session_start.get("ok").is_none());
    assert!(session_start.get("additionalContext").is_none());

    let prompt = respond_to_hook(
        Some(&store),
        PromptBoundary::UserPromptSubmit,
        "UserPromptSubmit",
        Some(&repo),
        None,
        None,
    )
    .expect("UserPromptSubmit with memory returns context");
    assert!(prompt["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("")
        .contains("cookies HttpOnly"));

    let mid = respond_to_hook(
        Some(&store),
        PromptBoundary::Other,
        "PreToolUse",
        Some(&repo),
        None,
        None,
    );
    assert!(mid.is_none(), "mid-generation no inyecta: {mid:?}");

    let down = respond_to_hook(
        None,
        PromptBoundary::SessionStart,
        "SessionStart",
        Some(&repo),
        None,
        None,
    );
    assert!(down.is_none());

    let stop = respond_to_hook(
        Some(&store),
        PromptBoundary::Stop,
        "Stop",
        Some(&repo),
        Some("listo el login, faltan tests"),
        None,
    );
    assert!(stop.is_none());
    let pack = build_context_pack(&store, &repo, None, None).unwrap();
    assert!(pack_contains(&pack, "listo el login, faltan tests"));
}

#[test]
fn summary_less_stop_does_not_hide_an_explicit_handoff() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);
    store
        .create_handoff(HandoffRequest {
            cwd: repo.clone(),
            summary: "auth a medio implementar, sigue con los tests".into(),
            provider: Some("claude".into()),
            session_id: Some("s1".into()),
            orchestrator_task_id: None,
            ttl_secs: Some(72 * 3600),
        })
        .unwrap();

    // El payload típico de Claude Stop no trae reason/summary: hook_server
    // pasa None. Un resumen vacío o solo espacios tampoco debe grabar.
    for summary in [None, Some(""), Some("   ")] {
        let stop = respond_to_hook(
            Some(&store),
            PromptBoundary::Stop,
            "Stop",
            Some(&repo),
            summary,
            None,
        );
        assert!(stop.is_none(), "fail-open en Stop: {stop:?}");
    }
    let session_end = respond_to_hook(
        Some(&store),
        PromptBoundary::SessionEnd,
        "SessionEnd",
        Some(&repo),
        Some("clear"),
        None,
    );
    assert!(session_end.is_none());

    let pack = build_context_pack(&store, &repo, None, None).unwrap();
    assert!(
        pack_contains(&pack, "auth a medio implementar, sigue con los tests"),
        "el handoff explícito tiene que seguir siendo el vigente: {pack:?}"
    );
    assert!(
        !pack_contains(&pack, "Session stopped"),
        "un Stop sin resumen no puede inventar un handoff: {pack:?}"
    );
    assert!(
        !pack_contains(&pack, "clear"),
        "SessionEnd.reason is lifecycle metadata, not a handoff: {pack:?}"
    );
}

#[test]
fn latest_handoff_breaks_same_second_ties_by_insertion_order() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);
    store
        .create_handoff(HandoffRequest {
            cwd: repo.clone(),
            summary: "primero".into(),
            provider: None,
            session_id: None,
            orchestrator_task_id: None,
            ttl_secs: Some(3600),
        })
        .unwrap();
    let second = store
        .create_handoff(HandoffRequest {
            cwd: repo.clone(),
            summary: "segundo".into(),
            provider: None,
            session_id: None,
            orchestrator_task_id: None,
            ttl_secs: Some(3600),
        })
        .unwrap();
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    conn.execute("UPDATE handoffs SET created_at = 123", [])
        .unwrap();
    drop(conn);

    let latest = store.latest_handoff(&repo, None).unwrap().unwrap();
    assert_eq!(latest.id, second.id);
    assert_eq!(latest.summary, "segundo");
}

#[test]
fn remember_status_distinguishes_commit_conflict_and_dedupe() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);

    let first = remember_selection(
        &store,
        &repo,
        "architecture/auth-strategy",
        "Usar cookies HttpOnly",
        ScopeKind::Project,
    )
    .unwrap();
    let first_status = format_remember_status(&first);
    assert!(
        first_status.contains("activa") || first_status.to_ascii_lowercase().contains("active"),
        "primer remember es un commit activo: {first_status}"
    );
    assert!(!first_status.to_ascii_lowercase().contains("conflicto"));
    assert!(!first.is_conflict());

    let conflict = remember_selection(
        &store,
        &repo,
        "architecture/auth-strategy",
        "tokens bearer en Authorization",
        ScopeKind::Project,
    )
    .unwrap();
    assert!(
        conflict.is_conflict(),
        "segunda escritura distinta de la misma clave es conflicto: {conflict:?}"
    );
    let conflict_status = format_remember_status(&conflict);
    assert!(
        conflict_status.to_ascii_lowercase().contains("conflicto")
            || conflict_status.to_ascii_lowercase().contains("conflict"),
        "la UI no puede decir que se guardó: {conflict_status}"
    );
    assert!(
        !conflict_status.contains("Guardado como memoria activa"),
        "conflicto no es un remember activo: {conflict_status}"
    );

    let pack = build_context_pack(&store, &repo, None, None).unwrap();
    assert!(
        pack_contains(&pack, "Usar cookies HttpOnly"),
        "el conflicto no pisa la activa: {pack:?}"
    );
    assert!(!pack_contains(&pack, "tokens bearer"));

    let deduped = remember_selection(
        &store,
        &repo,
        "architecture/auth-strategy",
        "Usar cookies HttpOnly",
        ScopeKind::Project,
    )
    .unwrap();
    assert!(
        matches!(deduped, WriteResult::Deduped(_)),
        "mismo contenido es dedupe: {deduped:?}"
    );
    let dedupe_status = format_remember_status(&deduped);
    assert!(
        dedupe_status.to_ascii_lowercase().contains("existía")
            || dedupe_status.to_ascii_lowercase().contains("reescribió")
            || dedupe_status.to_ascii_lowercase().contains("dedup"),
        "dedupe no se presenta como guardado nuevo: {dedupe_status}"
    );
}

#[test]
fn mcp_propose_stays_candidate_and_context_matches_the_launch_pack() {
    let root = temp_dir();
    let repo = root.join("repo");
    init_git_repo(&repo);
    let store = store_in(&root);
    store
        .remember(RememberRequest::human(
            repo.clone(),
            "pref/lang",
            "castellano rioplatense",
        ))
        .unwrap();

    let launch = launch_brief_with_memory("", Some(&repo), Some(&store), None);
    let message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "memory_context",
            "arguments": { "cwd": repo.display().to_string() }
        }
    });
    let response = crate::memory::mcp::handle_message_json(&store, &message).expect("mcp");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(text.contains("castellano rioplatense"), "{text}");
    assert!(launch.contains("castellano rioplatense"));

    let propose = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "memory_propose",
            "arguments": {
                "cwd": repo.display().to_string(),
                "key": "guess/x",
                "content": "inferido por el agente"
            }
        }
    });
    let proposed = crate::memory::mcp::handle_message_json(&store, &propose).expect("propose");
    let body = proposed["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(body.contains("candidate"), "{body}");
    let pack = build_context_pack(&store, &repo, Some("inferido"), None).unwrap();
    assert!(!pack_contains(&pack, "inferido por el agente"));
}

#[test]
fn mcp_get_cannot_read_an_id_from_an_unrelated_project() {
    let root = temp_dir();
    let repo = root.join("repo");
    let other = root.join("other");
    init_git_repo(&repo);
    init_git_repo(&other);
    let store = store_in(&root);
    let memory = store
        .remember(RememberRequest::human(
            repo.clone(),
            "private/project-rule",
            "sólo proyecto A",
        ))
        .unwrap()
        .memory()
        .clone();

    let request = |id: u64, cwd: &std::path::Path| {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "memory_get",
                "arguments": { "cwd": cwd, "id": memory.id }
            }
        })
    };
    let visible = crate::memory::handle_message_json(&store, &request(1, &repo)).unwrap();
    let hidden = crate::memory::handle_message_json(&store, &request(2, &other)).unwrap();
    let visible_text = visible["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    let hidden_text = hidden["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(visible_text.contains("sólo proyecto A"), "{visible}");
    assert_eq!(hidden_text, "null", "{hidden}");
}

#[test]
fn future_memory_schema_is_refused() {
    let root = temp_dir();
    let db = root.join("memory.db");
    drop(MemoryStore::open(&db).unwrap());
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("INSERT INTO schema_migrations(version) VALUES (99)", [])
        .unwrap();
    drop(conn);

    let error = MemoryStore::open(&db).expect_err("future schema rejected");
    assert!(error.to_string().contains("schema 99"), "{error:#}");
}

#[cfg(unix)]
#[test]
fn memory_database_is_private_to_the_current_user() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_dir();
    let db = root.join("memory.db");
    drop(MemoryStore::open(&db).unwrap());
    let mode = std::fs::metadata(db).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn explicit_link_is_required_to_share_a_clone() {
    let root = temp_dir();
    let origin = root.join("origin");
    init_git_repo(&origin);
    let clone = root.join("clone");
    clone_repo(&origin, &clone);
    let store = store_in(&root);
    store
        .remember(RememberRequest::human(
            origin.clone(),
            "shared/rule",
            "formato de commit convencional",
        ))
        .unwrap();
    assert!(!pack_contains(
        &build_context_pack(&store, &clone, None, None).unwrap(),
        "commit convencional"
    ));
    store.link_projects(&origin, &clone).unwrap();
    assert!(pack_contains(
        &build_context_pack(&store, &clone, None, None).unwrap(),
        "commit convencional"
    ));
}

#[test]
fn linking_projects_preserves_both_sides_and_surfaces_key_collisions() {
    let root = temp_dir();
    let first = root.join("first");
    let second = root.join("second");
    init_git_repo(&first);
    init_git_repo(&second);
    let store = store_in(&root);

    store
        .remember(RememberRequest::human(
            first.clone(),
            "shared/first",
            "memoria del primero",
        ))
        .unwrap();
    store
        .remember(RememberRequest::human(
            second.clone(),
            "shared/second",
            "memoria del segundo",
        ))
        .unwrap();
    store
        .remember(RememberRequest::human(
            first.clone(),
            "architecture/auth",
            "cookies",
        ))
        .unwrap();
    store
        .remember(RememberRequest::human(
            second.clone(),
            "architecture/auth",
            "bearer",
        ))
        .unwrap();

    store.link_projects(&first, &second).unwrap();

    let active = store.list_visible(&second, MemoryStatus::Active).unwrap();
    assert!(active
        .iter()
        .any(|memory| memory.content == "memoria del primero"));
    assert!(active
        .iter()
        .any(|memory| memory.content == "memoria del segundo"));
    let auth_active: Vec<_> = active
        .iter()
        .filter(|memory| memory.stable_key == "architecture/auth")
        .collect();
    assert_eq!(auth_active.len(), 1);
    let candidates = store
        .list_visible(&second, MemoryStatus::Candidate)
        .unwrap();
    assert!(candidates
        .iter()
        .any(|memory| memory.stable_key == "architecture/auth" && memory.content == "bearer"));
}
