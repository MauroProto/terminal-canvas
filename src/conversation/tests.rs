use super::*;
use uuid::Uuid;
struct TestDirectory(std::path::PathBuf);
impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("tc-conversation-tests-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn thread(workspace_id: Uuid) -> Conversation {
    Conversation {
        id: Uuid::new_v4(),
        workspace_id,
        panel_id: Uuid::new_v4(),
        title: "Chat 日本語 👩🏽‍💻".into(),
        provider: Some(crate::orchestration::AgentProvider::ClaudeCode),
        cwd: None,
        memory_task_id: None,
        native_session_id: None,
        runtime_session_id: Some(Uuid::new_v4()),
        binding_id: Some(Uuid::new_v4()),
    }
}
fn fixture() -> (ConversationStore, Conversation, Conversation) {
    let store = ConversationStore::in_memory().unwrap();
    let a = thread(Uuid::new_v4());
    let b = thread(a.workspace_id);
    store.register(&a).unwrap();
    store.register(&b).unwrap();
    connect(&store, b.id);
    (store, a, b)
}
fn connect(store: &ConversationStore, id: Uuid) {
    let token = store.issue_credential(id).unwrap();
    store.renew_bridge(Uuid::new_v4(), id, &token).unwrap();
}
fn request(a: &Conversation, b: &Conversation) -> SendRequest {
    SendRequest {
        operation_id: Uuid::new_v4(),
        source_id: a.id,
        target_id: b.id,
        text: "  petición exacta\r\n\u{1b}[31m日本語  ".into(),
        message_ids: vec![],
        context_budget_bytes: DEFAULT_CONTEXT_BYTES,
    }
}
#[test]
fn durable_context_reply_and_retry_survive_reopening() {
    let directory = TestDirectory::new();
    let path = directory.path().join("history.sqlite3");
    let mut store = ConversationStore::open(&path).unwrap();
    let a = thread(Uuid::new_v4());
    let b = thread(a.workspace_id);
    store.register(&a).unwrap();
    store.register(&b).unwrap();
    let text = "完整\r\n👩🏽‍💻  exacto";
    connect(&store, b.id);
    let message = store
        .record(
            Caller::User,
            Uuid::new_v4(),
            a.id,
            MessageRole::Assistant,
            text,
            "test",
        )
        .unwrap();
    let mut request = request(&a, &b);
    request.message_ids.push(message.id);
    let transfer = store.send(Caller::User, &request).unwrap();
    assert_eq!(transfer.context.messages[0].text, text);
    drop(store);
    let mut store = ConversationStore::open(&path).unwrap();
    assert_eq!(store.send(Caller::User, &request).unwrap(), transfer);
    let claimed = store
        .claim(transfer.id, b.binding_id.unwrap())
        .unwrap()
        .unwrap();
    assert!(claimed.prompt().unwrap().contains("\\u001b"));
    store.delivery_result(claimed.id, Ok(())).unwrap();
    store
        .reply(Caller::User, claimed.id, "Respuesta 👩🏽‍💻")
        .unwrap();
    drop(store);
    let mut store = ConversationStore::open(&path).unwrap();
    assert_eq!(
        store
            .reply(Caller::User, claimed.id, "Respuesta 👩🏽‍💻")
            .unwrap()
            .state,
        TransferState::Completed
    );
    assert!(store
        .reply(Caller::User, claimed.id, "Otra respuesta")
        .is_err());
    assert_eq!(store.messages(Caller::User, b.id, 0, 10).unwrap().len(), 2);
}
#[test]
fn links_credentials_and_source_identity_are_enforced() {
    let (mut store, a, b) = fixture();
    let token = store.issue_credential(a.id).unwrap();
    let caller = store.authenticate(a.id, &token).unwrap();
    assert!(store.send(caller, &request(&a, &b)).is_err());
    store.link(a.id, b.id, false).unwrap();
    let req = request(&a, &b);
    assert_eq!(
        store.send(caller, &req).unwrap().state,
        TransferState::AwaitingApproval
    );
    let mut spoof = request(&a, &b);
    spoof.source_id = b.id;
    spoof.target_id = a.id;
    assert!(store.send(caller, &spoof).is_err());
    let other = thread(Uuid::new_v4());
    store.register(&other).unwrap();
    assert!(store.link(a.id, other.id, true).is_err());
    store.revoke(a.id).unwrap();
    assert!(store.authenticate(a.id, &token).is_err());
    assert!(store.authorize(caller, b.id).is_err());
}
#[test]
fn revoke_link_cancels_agent_queue_but_keeps_user_requests() {
    let (mut store, a, b) = fixture();
    store.issue_credential(a.id).unwrap();
    store.link(a.id, b.id, true).unwrap();
    let agent = store.send(Caller::Agent(a.id), &request(&a, &b)).unwrap();
    let user = store.send(Caller::User, &request(&a, &b)).unwrap();
    store.unlink(a.id, b.id).unwrap();
    assert_eq!(
        store.transfer(Caller::User, agent.id).unwrap().state,
        TransferState::Cancelled
    );
    assert_eq!(
        store.transfer(Caller::User, user.id).unwrap().state,
        TransferState::Pending
    );
}
#[test]
fn revoked_credential_cannot_dispatch_previously_authorized_queue() {
    let (mut store, a, b) = fixture();
    store.issue_credential(a.id).unwrap();
    store.link(a.id, b.id, true).unwrap();
    let transfer = store.send(Caller::Agent(a.id), &request(&a, &b)).unwrap();
    store.revoke(a.id).unwrap();
    assert!(store
        .claim(transfer.id, b.binding_id.unwrap())
        .unwrap()
        .is_none());
    assert_eq!(
        store.transfer(Caller::User, transfer.id).unwrap().state,
        TransferState::Cancelled
    );
}
#[test]
fn changed_binding_requires_review_and_ambiguous_delivery_is_never_replayed() {
    let (mut store, a, mut b) = fixture();
    let transfer = store.send(Caller::User, &request(&a, &b)).unwrap();
    b.binding_id = Some(Uuid::new_v4());
    store.register(&b).unwrap();
    assert!(store
        .claim(transfer.id, b.binding_id.unwrap())
        .unwrap()
        .is_none());
    assert_eq!(
        store.transfer(Caller::User, transfer.id).unwrap().state,
        TransferState::NeedsReview
    );
    store.approve(transfer.id).unwrap();
    assert!(store
        .claim(transfer.id, b.binding_id.unwrap())
        .unwrap()
        .is_some());
    assert!(store
        .claim(transfer.id, b.binding_id.unwrap())
        .unwrap()
        .is_none());
    assert!(store.approve(transfer.id).is_err());
    assert!(store.cancel(Caller::User, transfer.id).is_err());
}
#[test]
fn one_outstanding_delivery_per_target_until_explicit_reply() {
    let (mut store, a, b) = fixture();
    let one = store.send(Caller::User, &request(&a, &b)).unwrap();
    let two = store.send(Caller::User, &request(&a, &b)).unwrap();
    store.claim(one.id, b.binding_id.unwrap()).unwrap().unwrap();
    assert!(store
        .claim(two.id, b.binding_id.unwrap())
        .unwrap()
        .is_none());
    store.delivery_result(one.id, Ok(())).unwrap();
    assert!(store.next_pending(b.id).unwrap().is_none());
    store.reply(Caller::User, one.id, "Terminado").unwrap();
    assert_eq!(store.next_pending(b.id).unwrap(), Some(two.id));
}
#[test]
fn omitted_messages_are_retrievable_only_by_recipient_after_delivery() {
    let (mut store, a, b) = fixture();
    connect(&store, b.id);
    let large = store
        .record(
            Caller::User,
            Uuid::new_v4(),
            a.id,
            MessageRole::Assistant,
            &"日本語".repeat(9000),
            "test",
        )
        .unwrap();
    let unrelated = store
        .record(
            Caller::User,
            Uuid::new_v4(),
            a.id,
            MessageRole::User,
            "Privado fuera del contexto",
            "test",
        )
        .unwrap();
    let mut req = request(&a, &b);
    req.message_ids.push(large.id);
    let transfer = store.send(Caller::User, &req).unwrap();
    assert!(transfer.context.messages.is_empty());
    assert_eq!(transfer.context.omitted[0].id, large.id);
    assert_eq!(
        serde_json::to_vec(&transfer.context).unwrap().len(),
        transfer.context.used_bytes
    );
    assert!(store.message(Caller::Agent(b.id), large.id).is_err());
    store.claim(transfer.id, b.binding_id.unwrap()).unwrap();
    assert_eq!(
        store
            .message(Caller::Agent(b.id), large.id)
            .unwrap()
            .unwrap()
            .text,
        large.text
    );
    assert!(store.message(Caller::Agent(b.id), unrelated.id).is_err());
}
#[test]
fn fast_reply_wins_over_late_input_ack_without_duplicate_result() {
    let (mut store, a, b) = fixture();
    let req = request(&a, &b);
    let transfer = store.send(Caller::User, &req).unwrap();
    store.claim(transfer.id, b.binding_id.unwrap()).unwrap();
    store.reply(Caller::User, transfer.id, "Listo").unwrap();
    store.delivery_result(transfer.id, Ok(())).unwrap();
    assert_eq!(
        store.transfer(Caller::User, transfer.id).unwrap().state,
        TransferState::Completed
    );
    assert_eq!(store.messages(Caller::User, b.id, 0, 10).unwrap().len(), 1);
    let mut changed = req;
    changed.text = "Otro pedido".into();
    assert!(store.send(Caller::User, &changed).is_err());
}
#[test]
fn future_schema_is_rejected_without_overwriting_it() {
    let directory = TestDirectory::new();
    let path = directory.path().join("future.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "user_version", 99).unwrap();
    drop(db);
    assert!(ConversationStore::open(&path).is_err());
    let db = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 99);
}
#[test]
fn concurrent_same_request_has_one_durable_operation_and_history_entry() {
    let directory = TestDirectory::new();
    let path = directory.path().join("concurrent.db");
    let store = ConversationStore::open(&path).unwrap();
    let a = thread(Uuid::new_v4());
    let b = thread(a.workspace_id);
    store.register(&a).unwrap();
    store.register(&b).unwrap();
    let req = request(&a, &b);
    drop(store);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let path = path.clone();
        let req = req.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let mut store = ConversationStore::open(&path).unwrap();
            barrier.wait();
            store.send(Caller::User, &req).unwrap()
        }));
    }
    assert_eq!(
        workers.remove(0).join().unwrap(),
        workers.remove(0).join().unwrap()
    );
    let store = ConversationStore::open(&path).unwrap();
    assert_eq!(store.messages(Caller::User, a.id, 0, 100).unwrap().len(), 1);
}

fn auth(store: &ConversationStore, id: Uuid) -> launch::CredentialFile {
    let token = store.issue_credential(id).unwrap();
    store.renew_bridge(Uuid::new_v4(), id, &token).unwrap();
    launch::CredentialFile {
        database: "unused-in-memory.db".into(),
        conversation_id: id,
        token,
    }
}
fn mcp_call(
    store: &mut ConversationStore,
    auth: &launch::CredentialFile,
    name: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    mcp::dispatch(store,auth,&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}),&mut true).unwrap()["result"].clone()
}
#[test]
fn different_provider_agents_can_exchange_context_and_explicit_results() {
    use serde_json::json;
    let (mut store, a, mut b) = fixture();
    b.provider = Some(crate::orchestration::AgentProvider::CodexCli);
    store.register(&b).unwrap();
    store.link_pair(a.id, b.id, true).unwrap();
    let auth_a = auth(&store, a.id);
    let auth_b = auth(&store, b.id);
    let message = Uuid::new_v4();
    assert_eq!(
        mcp_call(
            &mut store,
            &auth_a,
            "tc_chat_record",
            json!({"message_id":message,"role":"assistant","text":"Contexto íntegro 日本語\n👩🏽‍💻"})
        )["isError"],
        false
    );
    let operation = Uuid::new_v4();
    let args = json!({"operation_id":operation,"target_id":b.id,"text":"Revisá la solución","message_ids":[message]});
    let accepted = mcp_call(&mut store, &auth_a, "tc_chat_send", args.clone());
    assert_eq!(accepted["structuredContent"]["state"], "pending");
    assert_eq!(
        mcp_call(&mut store, &auth_a, "tc_chat_send", args),
        accepted
    );
    let transfer = store
        .claim(operation, b.binding_id.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        transfer.context.messages[0].text,
        "Contexto íntegro 日本語\n👩🏽‍💻"
    );
    store.delivery_result(operation, Ok(())).unwrap();
    assert_eq!(
        mcp_call(
            &mut store,
            &auth_b,
            "tc_chat_reply",
            json!({"operation_id":operation,"text":"Respuesta del otro proveedor 👩🏽‍💻"})
        )["structuredContent"]["state"],
        "completed"
    );
    let result = mcp_call(
        &mut store,
        &auth_a,
        "tc_chat_wait",
        json!({"operation_id":operation}),
    );
    assert_eq!(
        result["structuredContent"]["result"]["text"],
        "Respuesta del otro proveedor 👩🏽‍💻"
    );
}
#[test]
fn mcp_cannot_spoof_identity_escalate_scope_or_survive_revocation() {
    use serde_json::json;
    let (mut store, a, b) = fixture();
    let auth_a = auth(&store, a.id);
    let stranger = thread(Uuid::new_v4());
    store.register(&stranger).unwrap();
    assert_eq!(
        mcp_call(
            &mut store,
            &auth_a,
            "tc_chat_list",
            json!({"workspace_id":stranger.workspace_id})
        )["structuredContent"]["conversations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let forbidden = store
        .record(
            Caller::User,
            Uuid::new_v4(),
            b.id,
            MessageRole::User,
            "Fuera de alcance",
            "test",
        )
        .unwrap();
    assert_eq!(
        mcp_call(
            &mut store,
            &auth_a,
            "tc_chat_read",
            json!({"message_id":forbidden.id})
        )["isError"],
        true
    );
    assert_eq!(
        mcp_call(
            &mut store,
            &auth_a,
            "tc_chat_send",
            json!({"operation_id":Uuid::new_v4(),"source_id":b.id,"target_id":stranger.id,"text":"Suplantación"})
        )["isError"],
        true
    );
    assert_eq!(
        mcp_call(
            &mut store,
            &auth_a,
            "tc_chat_record",
            json!({"message_id":Uuid::new_v4(),"conversation_id":b.id,"role":"user","text":"Registro propio"})
        )["isError"],
        false
    );
    assert_eq!(store.messages(Caller::User, b.id, 0, 10).unwrap().len(), 1);
    store.revoke(a.id).unwrap();
    assert_eq!(
        mcp_call(&mut store, &auth_a, "tc_chat_read", json!({}))["isError"],
        true
    );
}
#[test]
fn stdio_negotiates_protocol_preserves_unicode_and_limits_frames() {
    use serde_json::json;
    let (mut store, a, _) = fixture();
    let auth_a = auth(&store, a.id);
    let message = Uuid::new_v4();
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tc_chat_record","arguments":{"message_id":message,"role":"user","text":"JSON exacto\n日本語 👩🏽‍💻"}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tc_chat_read","arguments":{"message_id":message}}}),
    ];
    let input = requests
        .iter()
        .map(|r| serde_json::to_string(r).unwrap() + "\n")
        .collect::<String>();
    let mut output = Vec::new();
    mcp::serve(
        &mut store,
        &auth_a,
        &mut std::io::Cursor::new(input),
        &mut output,
    )
    .unwrap();
    let responses: Vec<serde_json::Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(
        responses[2]["result"]["structuredContent"]["text"],
        "JSON exacto\n日本語 👩🏽‍💻"
    );
    assert!(mcp::serve(
        &mut store,
        &auth_a,
        &mut std::io::Cursor::new(vec![b'x'; 2 * 1024 * 1024 + 1]),
        &mut Vec::new()
    )
    .is_err());
}
#[test]
fn utf8_chunks_do_not_truncate_or_repeat_characters() {
    use serde_json::json;
    let (mut store, a, _) = fixture();
    let auth_a = auth(&store, a.id);
    let text = "日👩🏽‍💻語".repeat(2000);
    let id = Uuid::new_v4();
    store
        .record(
            Caller::User,
            id,
            a.id,
            MessageRole::Assistant,
            &text,
            "test",
        )
        .unwrap();
    let mut assembled = String::new();
    let mut offset = 0;
    loop {
        let result = mcp_call(
            &mut store,
            &auth_a,
            "tc_chat_read",
            json!({"message_id":id,"offset":offset,"limit":31}),
        );
        assert_eq!(result["isError"], false);
        let chunk = &result["structuredContent"];
        assembled.push_str(chunk["text"].as_str().unwrap());
        match chunk["next_offset"].as_u64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(assembled, text);
    assert_eq!(
        mcp_call(
            &mut store,
            &auth_a,
            "tc_chat_read",
            json!({"message_id":id,"offset":1})
        )["isError"],
        true
    );
}
#[test]
fn disconnected_or_rotated_mcp_target_never_receives_queued_input() {
    let (mut store, a, b) = fixture();
    let op = store.send(Caller::User, &request(&a, &b)).unwrap();
    let token = store.issue_credential(b.id).unwrap();
    assert!(!store.bridge_connected(b.id).unwrap());
    assert!(store.claim(op.id, b.binding_id.unwrap()).unwrap().is_none());
    let lease = Uuid::new_v4();
    store.renew_bridge(lease, b.id, &token).unwrap();
    assert!(store.bridge_connected(b.id).unwrap());
    store.remove_bridge(lease).unwrap();
    assert!(store.claim(op.id, b.binding_id.unwrap()).unwrap().is_none());
}
#[test]
fn provider_switch_keeps_the_original_message_provenance() {
    let (store, mut a, _) = fixture();
    a.native_session_id = Some("native-claude-session".into());
    store.register(&a).unwrap();
    let id = Uuid::new_v4();
    store
        .record(
            Caller::User,
            id,
            a.id,
            MessageRole::User,
            "Origen Claude",
            "test",
        )
        .unwrap();
    a.provider = Some(crate::orchestration::AgentProvider::CodexCli);
    a.native_session_id = None;
    store.register(&a).unwrap();
    let message = store.message(Caller::User, id).unwrap().unwrap();
    assert_eq!(
        message.origin.provider,
        Some(crate::orchestration::AgentProvider::ClaudeCode)
    );
    assert_eq!(
        message.origin.native_session_id.as_deref(),
        Some("native-claude-session")
    );
    assert!(store
        .conversation(a.id)
        .unwrap()
        .unwrap()
        .native_session_id
        .is_none());
}
#[test]
fn native_launch_arguments_are_structured_and_do_not_override_other_mcp_servers() {
    let config = launch::McpConfig {
        path: "C:/fixture with spaces/servers.json".into(),
        auth_path: "C:/fixture with spaces/cred.json".into(),
    };
    let executable = std::path::Path::new("C:/fixture with spaces/terminal.exe");
    let mut claude = std::process::Command::new("unused");
    launch::append_config(
        &mut claude,
        crate::orchestration::AgentProvider::ClaudeCode,
        &config,
        executable,
    )
    .unwrap();
    let args: Vec<_> = claude.get_args().collect();
    assert_eq!(args[0], "--mcp-config");
    assert_eq!(args[1], config.path.as_os_str());
    assert!(!args.iter().any(|arg| *arg == "--strict-mcp-config"));
    let mut codex = std::process::Command::new("unused");
    launch::append_config(
        &mut codex,
        crate::orchestration::AgentProvider::CodexCli,
        &config,
        executable,
    )
    .unwrap();
    let args: Vec<_> = codex.get_args().map(|arg| arg.to_str().unwrap()).collect();
    assert_eq!(args.len(), 6);
    let options = args
        .chunks(2)
        .map(|pair| pair[1])
        .collect::<Vec<_>>()
        .join("\n");
    let value: toml::Value = toml::from_str(&options).unwrap();
    assert_eq!(
        value["mcp_servers"]["terminal_canvas_chat"]["command"].as_str(),
        executable.to_str()
    );
    assert_eq!(
        value["mcp_servers"]["terminal_canvas_chat"]["env"]["TC_CHAT_AUTH_FILE"].as_str(),
        config.auth_path.to_str()
    );
}

#[test]
fn preview_is_frozen_and_rejects_changes_before_commit() {
    let (mut store, mut a, b) = fixture();
    let request = request(&a, &b);
    let context = store
        .context(Caller::User, a.id, &[], DEFAULT_CONTEXT_BYTES)
        .unwrap();
    let prepared = PreparedSend {
        request,
        context,
        target_binding: b.binding_id,
    };
    a.title = "Cambió el origen".into();
    store.register(&a).unwrap();
    assert!(store.send_prepared(Caller::User, &prepared).is_err());
    assert!(store
        .transfer(Caller::User, prepared.request.operation_id)
        .is_err());
    let mut fresh = prepared;
    fresh.context = store
        .context(Caller::User, a.id, &[], DEFAULT_CONTEXT_BYTES)
        .unwrap();
    store.send_prepared(Caller::User, &fresh).unwrap();
    assert_eq!(
        store.send_prepared(Caller::User, &fresh).unwrap().context,
        fresh.context
    );
}
#[test]
fn failed_config_file_write_does_not_revoke_the_current_credential() {
    let (store, a, _) = fixture();
    let original = store.issue_credential(a.id).unwrap();
    let directory = TestDirectory::new();
    let not_directory = directory.path().join("file.txt");
    std::fs::write(&not_directory, "fixture").unwrap();
    assert!(launch::issue_config(
        &store,
        a.id,
        std::path::Path::new("unused.db"),
        &not_directory,
        std::path::Path::new("unused.exe")
    )
    .is_err());
    assert!(store.authenticate(a.id, &original).is_ok());
}
#[test]
fn generated_mcp_config_uses_a_private_credential_file_and_revokes_old_token() {
    let (store, a, _) = fixture();
    let original = store.issue_credential(a.id).unwrap();
    let directory = TestDirectory::new();
    let path = directory.path().join("history.db");
    let config = launch::issue_config(
        &store,
        a.id,
        &path,
        &directory.path().join("mcp"),
        std::path::Path::new("C:/fixture/app.exe"),
    )
    .unwrap();
    let content = std::fs::read_to_string(&config.path).unwrap();
    let auth: launch::CredentialFile =
        serde_json::from_slice(&std::fs::read(&config.auth_path).unwrap()).unwrap();
    assert!(!content.contains(&auth.token));
    assert!(!content.contains(&original));
    assert!(store.authenticate(a.id, &original).is_err());
    assert_eq!(
        store.authenticate(a.id, &auth.token).unwrap().agent_id(),
        Some(a.id)
    );
    assert_eq!(auth.database, path);
}

#[test]
fn credential_rotation_fences_calls_authenticated_before_the_effect_transaction() {
    let directory = TestDirectory::new();
    let path = directory.path().join("rotation.sqlite3");
    let mut store = ConversationStore::open(&path).unwrap();
    let a = thread(Uuid::new_v4());
    let b = thread(a.workspace_id);
    store.register(&a).unwrap();
    store.register(&b).unwrap();
    store.link_pair(a.id, b.id, true).unwrap();
    connect(&store, b.id);
    let original = store.issue_credential(a.id).unwrap();
    store.renew_bridge(Uuid::new_v4(), a.id, &original).unwrap();
    let admitted = store.authenticate(a.id, &original).unwrap();
    let message = store
        .record(
            admitted,
            Uuid::new_v4(),
            a.id,
            MessageRole::Assistant,
            "Antes",
            "test",
        )
        .unwrap();
    let outgoing = store.send(admitted, &request(&a, &b)).unwrap();
    let incoming = store.send(Caller::User, &request(&b, &a)).unwrap();
    store
        .claim(incoming.id, a.binding_id.unwrap())
        .unwrap()
        .unwrap();
    store.delivery_result(incoming.id, Ok(())).unwrap();
    let previous_a = store.history(Caller::User, a.id, 0).unwrap();
    let previous_b = store.history(Caller::User, b.id, 0).unwrap();

    // Another MCP/config connection commits rotation between authentication
    // and BEGIN IMMEDIATE in the already admitted request.
    let other_connection = ConversationStore::open(&path).unwrap();
    let renewed = other_connection.issue_credential(a.id).unwrap();
    other_connection
        .renew_bridge(Uuid::new_v4(), a.id, &renewed)
        .unwrap();
    let rejected_message = Uuid::new_v4();
    let rejected_send = request(&a, &b);
    assert!(store
        .record(
            admitted,
            rejected_message,
            a.id,
            MessageRole::Assistant,
            "Después",
            "test"
        )
        .is_err());
    assert!(store.send(admitted, &rejected_send).is_err());
    assert!(store.cancel(admitted, outgoing.id).is_err());
    assert!(store
        .reply(admitted, incoming.id, "Respuesta obsoleta")
        .is_err());
    assert!(store.set_available(admitted, true).is_err());
    assert!(store.message(admitted, message.id).is_err());
    assert!(store.history(admitted, a.id, 0).is_err());
    assert!(store.list(admitted, a.workspace_id, None).is_err());
    assert!(store.snapshot(admitted, a.workspace_id).is_err());
    assert!(store.transfer(admitted, incoming.id).is_err());
    assert_eq!(store.history(Caller::User, a.id, 0).unwrap(), previous_a);
    assert_eq!(store.history(Caller::User, b.id, 0).unwrap(), previous_b);
    assert!(store
        .message(Caller::User, rejected_message)
        .unwrap()
        .is_none());
    assert!(store
        .transfer(Caller::User, rejected_send.operation_id)
        .is_err());
    assert_eq!(
        store.transfer(Caller::User, outgoing.id).unwrap().state,
        TransferState::Pending
    );
    assert_eq!(
        store.transfer(Caller::User, incoming.id).unwrap().state,
        TransferState::InputQueued
    );
    assert_eq!(store.bridge_availability(a.id).unwrap(), None);

    let current = store.authenticate(a.id, &renewed).unwrap();
    assert_eq!(current.agent_id(), Some(a.id));
    store
        .record(
            current,
            rejected_message,
            a.id,
            MessageRole::Assistant,
            "Después",
            "test",
        )
        .unwrap();
    store.cancel(current, outgoing.id).unwrap();
    store.set_available(current, true).unwrap();
    assert_eq!(
        store
            .reply(current, incoming.id, "Respuesta actual")
            .unwrap()
            .state,
        TransferState::Completed
    );
    // Complete the inbound operation before delegating back to its source;
    // otherwise the cycle guard correctly refuses this unrelated auth check.
    store.send(current, &rejected_send).unwrap();
    assert_eq!(store.bridge_availability(a.id).unwrap(), Some(true));
}
#[test]
fn queue_capacity_and_message_id_conflicts_leave_no_partial_history() {
    let (mut store, a, b) = fixture();
    let id = Uuid::new_v4();
    store
        .record(Caller::User, id, a.id, MessageRole::User, "Uno", "test")
        .unwrap();
    assert!(store
        .record(Caller::User, id, a.id, MessageRole::User, "Dos", "test")
        .is_err());
    for _ in 0..MAX_PENDING_TRANSFERS {
        store.send(Caller::User, &request(&a, &b)).unwrap();
    }
    let before = store.history(Caller::User, a.id, 0).unwrap();
    let extra = request(&a, &b);
    assert!(store.send(Caller::User, &extra).is_err());
    assert!(store.transfer(Caller::User, extra.operation_id).is_err());
    assert_eq!(store.history(Caller::User, a.id, 0).unwrap(), before);
}

#[test]
fn reserved_omission_references_keep_large_selections_within_budget() {
    let (store, a, _) = fixture();
    let mut ids = Vec::new();
    for _ in 0..100 {
        ids.push(
            store
                .record(
                    Caller::User,
                    Uuid::new_v4(),
                    a.id,
                    MessageRole::Assistant,
                    &"日".repeat(300),
                    "test",
                )
                .unwrap()
                .id,
        );
    }
    let context = store
        .context(Caller::User, a.id, &ids, DEFAULT_CONTEXT_BYTES)
        .unwrap();
    assert_eq!(context.messages.len() + context.omitted.len(), 100);
    assert!(!context.messages.is_empty());
    assert!(!context.omitted.is_empty());
    assert_eq!(
        serde_json::to_vec(&context).unwrap().len(),
        context.used_bytes
    );
    assert!(context.used_bytes <= DEFAULT_CONTEXT_BYTES);
}
#[test]
fn revoked_agent_queue_stays_cancelled_after_reissuing_credentials() {
    let (mut store, a, b) = fixture();
    store.issue_credential(a.id).unwrap();
    store.link(a.id, b.id, true).unwrap();
    let transfer = store.send(Caller::Agent(a.id), &request(&a, &b)).unwrap();
    store.revoke(a.id).unwrap();
    store.issue_credential(a.id).unwrap();
    assert_eq!(
        store.transfer(Caller::User, transfer.id).unwrap().state,
        TransferState::Cancelled
    );
    assert!(store
        .claim(transfer.id, b.binding_id.unwrap())
        .unwrap()
        .is_none());
}

#[test]
fn nested_delegation_retains_parent_blocks_early_reply_and_rejects_cycles() {
    let (mut store, a, b) = fixture();
    connect(&store, a.id);
    let c = thread(a.workspace_id);
    store.register(&c).unwrap();
    connect(&store, c.id);
    store.link(a.id, b.id, true).unwrap();
    store.link(b.id, c.id, true).unwrap();
    store.link(c.id, a.id, true).unwrap();
    let parent = store.send(Caller::Agent(a.id), &request(&a, &b)).unwrap();
    store
        .claim(parent.id, b.binding_id.unwrap())
        .unwrap()
        .unwrap();
    store.delivery_result(parent.id, Ok(())).unwrap();
    let child = store.send(Caller::Agent(b.id), &request(&b, &c)).unwrap();
    assert_eq!(child.parent_id, Some(parent.id));
    assert_eq!(child.root_id, parent.id);
    assert_eq!(child.depth, 2);
    assert!(store
        .reply(Caller::Agent(b.id), parent.id, "Demasiado temprano")
        .is_err());
    store
        .claim(child.id, c.binding_id.unwrap())
        .unwrap()
        .unwrap();
    store.delivery_result(child.id, Ok(())).unwrap();
    assert!(store.send(Caller::Agent(c.id), &request(&c, &a)).is_err());
    store
        .reply(Caller::Agent(c.id), child.id, "Resultado del nieto")
        .unwrap();
    assert_eq!(
        store
            .reply(Caller::Agent(b.id), parent.id, "Resultado integrado")
            .unwrap()
            .state,
        TransferState::Completed
    );
}
#[test]
fn nested_delegation_depth_has_a_hard_limit() {
    let mut store = ConversationStore::in_memory().unwrap();
    let workspace = Uuid::new_v4();
    let rows = (0..MAX_DELEGATION_DEPTH + 2)
        .map(|_| thread(workspace))
        .collect::<Vec<_>>();
    for row in &rows {
        store.register(row).unwrap();
        connect(&store, row.id);
    }
    for pair in rows.windows(2) {
        store.link(pair[0].id, pair[1].id, true).unwrap();
    }
    for index in 0..MAX_DELEGATION_DEPTH {
        let op = store
            .send(
                Caller::Agent(rows[index].id),
                &request(&rows[index], &rows[index + 1]),
            )
            .unwrap();
        assert_eq!(op.depth, index + 1);
        store
            .claim(op.id, rows[index + 1].binding_id.unwrap())
            .unwrap()
            .unwrap();
        store.delivery_result(op.id, Ok(())).unwrap();
    }
    assert!(store
        .send(
            Caller::Agent(rows[MAX_DELEGATION_DEPTH].id),
            &request(&rows[MAX_DELEGATION_DEPTH], &rows[MAX_DELEGATION_DEPTH + 1])
        )
        .is_err());
}

#[test]
fn metadata_pages_are_bounded_and_cursors_retrieve_every_authorized_chat() {
    use serde_json::json;
    let (mut store, a, _) = fixture();
    let auth_a = auth(&store, a.id);
    let mut expected = std::collections::HashSet::from([a.id]);
    for _ in 0..60 {
        let mut row = thread(a.workspace_id);
        row.cwd = Some("x".repeat(20_000));
        store.register(&row).unwrap();
        store.link(a.id, row.id, false).unwrap();
        expected.insert(row.id);
    }
    let mut found = std::collections::HashSet::new();
    let mut args = json!({});
    loop {
        let result = mcp_call(&mut store, &auth_a, "tc_chat_list", args);
        let page = &result["structuredContent"];
        assert_eq!(result["isError"], false);
        assert!(serde_json::to_vec(&result).unwrap().len() < 400 * 1024);
        for row in page["conversations"].as_array().unwrap() {
            assert!(found.insert(Uuid::parse_str(row["id"].as_str().unwrap()).unwrap()));
        }
        match page["next_after_id"].as_str() {
            Some(id) => args = json!({"after_id":id}),
            None => break,
        }
    }
    assert_eq!(found, expected);
}
