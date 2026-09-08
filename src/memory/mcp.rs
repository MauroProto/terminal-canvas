//! Bridge MCP stdio: herramientas neutrales sobre el mismo store.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use super::context::{build_context_pack, format_context_pack};
use super::identity::resolve_location;
use super::model::{Actor, HandoffRequest, MemoryKind, RememberRequest, ScopeKind, WriteResult};
use super::store::MemoryStore;

const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    "2024-11-05",
    "2025-03-26",
    "2025-06-18",
    LATEST_PROTOCOL_VERSION,
];

/// Frontera de confianza del proceso MCP. El agente puede elegir herramientas
/// y queries, pero no puede ampliar su proyecto/tarea mintiendo sobre `cwd`.
#[derive(Debug, Clone)]
struct McpScope {
    location: super::model::ResolvedLocation,
    task_id: Option<uuid::Uuid>,
}

impl McpScope {
    fn from_root(root: &Path) -> Self {
        Self {
            location: resolve_location(root),
            task_id: None,
        }
    }

    fn validate(&self, cwd: &Path) -> Result<()> {
        let candidate = resolve_location(cwd);
        let allowed = match self.location.identity_kind {
            super::model::IdentityKind::GitCommonDir => {
                candidate.identity_kind == super::model::IdentityKind::GitCommonDir
                    && candidate.identity_value == self.location.identity_value
                    && candidate.task_identity() == self.location.task_identity()
            }
            super::model::IdentityKind::WorkspaceRoot => {
                candidate.identity_kind == super::model::IdentityKind::WorkspaceRoot
                    && candidate.cwd.starts_with(&self.location.cwd)
            }
        };
        if !allowed {
            anyhow::bail!("cwd fuera del proyecto o worktree autorizado para este servidor MCP");
        }
        Ok(())
    }
}

pub fn serve_stdio() -> Result<()> {
    let db = super::store::default_db_path().unwrap_or_else(|| PathBuf::from("memory.db"));
    let store = MemoryStore::open(db)?;
    let root = std::env::var_os("TC_MEMORY_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)?;
    let mut scope = McpScope::from_root(&root);
    scope.task_id = super::identity::task_id_from_env()?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut reader = stdin.lock();
    loop {
        let Some(message) = read_message(&mut reader)? else {
            break;
        };
        let response = dispatch(&store, &message, Some(&scope));
        if let Some(response) = response {
            write_message(&mut stdout, &response)?;
        }
    }
    Ok(())
}

fn handle(store: &MemoryStore, message: &Value, scope: Option<&McpScope>) -> Option<Value> {
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(error_response(None, -32600, "JSON-RPC inválido".to_owned()));
    }
    let id = message.get("id").cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Some(error_response(id, -32600, "falta method".to_owned()));
    };
    let result = match method {
        "initialize" => json!({
            "protocolVersion": negotiated_protocol_version(message),
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "tc-memory",
                "title": "TerminalCanvas Memory Hub",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "Use memory_context for relevant active memory. memory_propose creates a pending candidate and never activates it automatically."
        }),
        "notifications/initialized" | "initialized" => return None,
        "tools/list" => json!({ "tools": tool_list() }),
        "tools/call" => {
            match call_tool(store, message.get("params").unwrap_or(&Value::Null), scope) {
                Ok(value) => value,
                Err(err) => json!({
                    "content": [{ "type": "text", "text": err.to_string() }],
                    "isError": true
                }),
            }
        }
        "ping" => json!({}),
        other => {
            // Las notificaciones JSON-RPC nunca reciben respuesta.
            id.as_ref()?;
            return Some(error_response(
                id,
                -32601,
                format!("método desconocido: {other}"),
            ));
        }
    };
    id.map(|id| json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn dispatch(store: &MemoryStore, message: &Value, scope: Option<&McpScope>) -> Option<Value> {
    let Some(batch) = message.as_array() else {
        return handle(store, message, scope);
    };
    if batch.is_empty() {
        return Some(error_response(None, -32600, "batch vacío".to_owned()));
    }
    let responses: Vec<_> = batch
        .iter()
        .filter_map(|message| handle(store, message, scope))
        .collect();
    (!responses.is_empty()).then_some(Value::Array(responses))
}

fn negotiated_protocol_version(message: &Value) -> &str {
    let requested = message
        .get("params")
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str);
    requested
        .filter(|version| SUPPORTED_PROTOCOL_VERSIONS.contains(version))
        .unwrap_or(LATEST_PROTOCOL_VERSION)
}

fn tool_list() -> Value {
    json!([
        {
            "name": "memory_context",
            "description": "Paquete acotado de memoria compartida para el cwd actual",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "query": { "type": "string" }
                },
                "required": ["cwd"],
                "additionalProperties": false
            }
        },
        {
            "name": "memory_search",
            "description": "Busca memoria activa visible para un cwd (hasta 100 resultados y 512 KiB; refine query para consultas amplias)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "query": { "type": "string" }
                },
                "required": ["cwd", "query"],
                "additionalProperties": false
            }
        },
        {
            "name": "memory_get",
            "description": "Recupera una entrada activa visible para el cwd por id",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "id": { "type": "string" }
                },
                "required": ["cwd", "id"],
                "additionalProperties": false
            }
        },
        {
            "name": "memory_propose",
            "description": "Propone una memoria candidata (no se activa sola)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "key": { "type": "string" },
                    "content": { "type": "string" },
                    "scope": {
                        "type": "string",
                        "enum": ["project", "workspace", "task"]
                    }
                },
                "required": ["cwd", "key", "content"],
                "additionalProperties": false
            }
        },
        {
            "name": "handoff_create",
            "description": "Registra un handoff episódico de la tarea actual",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "summary": { "type": "string" }
                },
                "required": ["cwd", "summary"],
                "additionalProperties": false
            }
        },
        {
            "name": "handoff_get",
            "description": "Devuelve el último handoff vigente de la tarea",
            "inputSchema": {
                "type": "object",
                "properties": { "cwd": { "type": "string" } },
                "required": ["cwd"],
                "additionalProperties": false
            }
        }
    ])
}

fn call_tool(store: &MemoryStore, params: &Value, scope: Option<&McpScope>) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("falta name"))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    // Task identity belongs to this process, never to tool-call arguments.
    let task_id = scope.and_then(|scope| scope.task_id);
    let text = match name {
        "memory_context" => {
            let cwd = arg_path(&args, "cwd", scope)?;
            let query = args.get("query").and_then(Value::as_str);
            let pack = build_context_pack(store, &cwd, query, task_id)?;
            let mut rendered = format_context_pack(&pack);
            if rendered.is_empty() {
                rendered = serde_json::to_string_pretty(&pack)?;
            }
            rendered
        }
        "memory_search" => {
            let cwd = arg_path(&args, "cwd", scope)?;
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("falta query"))?;
            serde_json::to_string_pretty(&store.search_scoped(&cwd, query, task_id)?)?
        }
        "memory_get" => {
            let cwd = arg_path(&args, "cwd", scope)?;
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("falta id"))?;
            serde_json::to_string_pretty(&store.get_visible_scoped(&cwd, id, task_id)?)?
        }
        "memory_propose" => {
            let cwd = arg_path(&args, "cwd", scope)?;
            let result = store.propose(RememberRequest {
                client_id: "tc-memory-mcp".to_owned(),
                request_id: uuid::Uuid::new_v4().to_string(),
                cwd,
                scope: proposed_scope(&args)?,
                kind: MemoryKind::Fact,
                key: args
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                content: args
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                actor: Actor::agent("mcp"),
                expected_revision: None,
                orchestrator_task_id: task_id,
                workspace_id: None,
            })?;
            serde_json::to_string_pretty(&proposal_response(&result))?
        }
        "handoff_create" => {
            let cwd = arg_path(&args, "cwd", scope)?;
            let summary = args
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            serde_json::to_string_pretty(&store.create_handoff(HandoffRequest {
                cwd,
                summary,
                provider: Some("mcp".to_owned()),
                session_id: None,
                orchestrator_task_id: task_id,
                ttl_secs: Some(72 * 3600),
            })?)?
        }
        "handoff_get" => {
            let cwd = arg_path(&args, "cwd", scope)?;
            serde_json::to_string_pretty(&store.latest_handoff(&cwd, task_id)?)?
        }
        other => anyhow::bail!("herramienta desconocida: {other}"),
    };
    Ok(json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

fn arg_path(args: &Value, key: &str, scope: Option<&McpScope>) -> Result<PathBuf> {
    let path = args
        .get(key)
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("falta {key}"))?;
    if let Some(scope) = scope {
        scope.validate(&path)?;
    }
    Ok(path)
}

fn proposed_scope(args: &Value) -> Result<ScopeKind> {
    match args.get("scope") {
        None => Ok(ScopeKind::Project),
        Some(Value::String(scope)) => ScopeKind::parse(scope)
            .filter(|scope| !matches!(scope, ScopeKind::User))
            .ok_or_else(|| anyhow::anyhow!("scope inválido para una propuesta MCP")),
        Some(_) => anyhow::bail!("scope debe ser un string"),
    }
}

/// MCP mutation replies describe the submitted proposal, never another
/// session's pending content. Operator-facing WriteResult remains richer.
fn proposal_response(result: &WriteResult) -> Value {
    match result {
        WriteResult::Committed(write) | WriteResult::Deduped(write) => json!({
            "result": if matches!(result, WriteResult::Committed(_)) {
                "committed"
            } else {
                "deduped"
            },
            "memory_id": write.memory.id,
            "status": write.memory.status.as_str(),
            "revision": write.committed_revision,
        }),
        WriteResult::Conflict(conflict) => json!({
            "result": "conflict",
            "candidate_id": conflict.candidate_id,
        }),
    }
}

fn error_response(id: Option<Value>, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let first = loop {
        let Some(line) = read_limited_line(reader, MAX_MESSAGE_BYTES)? else {
            return Ok(None);
        };
        if !line.trim().is_empty() {
            break line;
        }
    };
    let first_trim = first.trim();
    if first_trim
        .to_ascii_lowercase()
        .starts_with("content-length:")
    {
        let len: usize = first_trim
            .split(':')
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("content-length"))?
            .trim()
            .parse()?;
        if len > MAX_MESSAGE_BYTES {
            anyhow::bail!("mensaje MCP demasiado grande");
        }
        // Acepta headers adicionales (por ejemplo Content-Type) hasta la
        // línea vacía que separa el payload.
        loop {
            let header = read_limited_line(reader, 8 * 1024)?
                .ok_or_else(|| anyhow::anyhow!("headers MCP truncados"))?;
            if header.trim().is_empty() {
                break;
            }
        }
        let mut buf = vec![0u8; len];
        Read::read_exact(reader, &mut buf)?;
        return Ok(Some(serde_json::from_slice(&buf)?));
    }
    Ok(Some(serde_json::from_str(first_trim)?))
}

fn read_limited_line(reader: &mut impl BufRead, limit: usize) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    let read = (&mut *reader)
        .take((limit + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.len() > limit
        || (!bytes.ends_with(b"\n") && bytes.len() == limit && !reader.fill_buf()?.is_empty())
    {
        anyhow::bail!("mensaje MCP demasiado grande");
    }
    Ok(Some(String::from_utf8(bytes)?))
}

fn write_message(writer: &mut impl Write, value: &Value) -> Result<()> {
    // MCP stdio usa un JSON-RPC compacto por línea. Content-Length pertenece
    // a otros protocolos (por ejemplo LSP) y rompe clientes MCP reales.
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Entrada testeable del dispatch MCP (sin stdio).
pub fn handle_message_json(store: &MemoryStore, message: &Value) -> Option<Value> {
    dispatch(store, message, None)
}

#[cfg(test)]
mod tests {
    use super::{dispatch, write_message, McpScope};
    use crate::memory::test_support::{init_git_repo, temp_dir};
    use crate::memory::MemoryStore;

    #[test]
    fn process_task_scope_is_used_by_every_tool_and_cannot_be_overridden() {
        use crate::memory::{RememberRequest, ScopeKind};
        let root = temp_dir();
        let repo = root.join("repo");
        init_git_repo(&repo);
        let store = MemoryStore::open(root.join("memory.db")).unwrap();
        let own_task = uuid::Uuid::new_v4();
        let other_task = uuid::Uuid::new_v4();
        let mut scope = McpScope::from_root(&repo);
        scope.task_id = Some(own_task);
        let mut other_id = String::new();
        for (task, value) in [(own_task, "own-marker"), (other_task, "foreign-marker")] {
            let mut request = RememberRequest::human(repo.clone(), "task-value", value);
            request.scope = ScopeKind::Task;
            request.orchestrator_task_id = Some(task);
            let write = store.remember(request).unwrap();
            if task == other_task {
                other_id = write.memory().id.clone();
            }
        }
        let call = |name: &str, mut args: serde_json::Value| {
            args["cwd"] = serde_json::json!(repo);
            args["task_id"] = serde_json::json!(other_task);
            super::call_tool(
                &store,
                &serde_json::json!({"name":name,"arguments":args}),
                Some(&scope),
            )
            .unwrap()["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        for name in ["memory_context", "memory_search"] {
            let text = call(name, serde_json::json!({"query":""}));
            assert!(text.contains("own-marker"));
            assert!(!text.contains("foreign-marker"));
        }
        assert_eq!(
            call("memory_get", serde_json::json!({"id":other_id})),
            "null"
        );
        call(
            "handoff_create",
            serde_json::json!({"summary":"own-handoff"}),
        );
        assert!(call("handoff_get", serde_json::json!({})).contains("own-handoff"));
        assert!(store
            .latest_handoff(&repo, Some(other_task))
            .unwrap()
            .is_none());
        let proposal: serde_json::Value = serde_json::from_str(&call(
            "memory_propose",
            serde_json::json!({"scope":"task","key":"new","content":"proposed"}),
        ))
        .unwrap();
        store
            .approve(
                proposal["memory_id"].as_str().unwrap(),
                &crate::memory::Actor::human(),
            )
            .unwrap();
        assert_eq!(
            store
                .search_scoped(&repo, "proposed", Some(own_task))
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .search_scoped(&repo, "proposed", Some(other_task))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn stdio_output_is_one_compact_json_line() {
        let mut output = Vec::new();
        write_message(
            &mut output,
            &serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}}),
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.ends_with('\n'));
        assert!(!text.starts_with("Content-Length"));
    }

    #[test]
    fn initialize_negotiates_supported_versions() {
        let root = temp_dir();
        let store = MemoryStore::open(root.join("memory.db")).unwrap();
        for version in ["2024-11-05", "2025-11-25"] {
            let response = dispatch(
                &store,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": { "protocolVersion": version }
                }),
                None,
            )
            .unwrap();
            assert_eq!(response["result"]["protocolVersion"], version);
        }
    }

    #[test]
    fn batches_return_only_responses_not_notifications() {
        let root = temp_dir();
        let store = MemoryStore::open(root.join("memory.db")).unwrap();
        let response = dispatch(
            &store,
            &serde_json::json!([
                { "jsonrpc": "2.0", "method": "notifications/initialized" },
                { "jsonrpc": "2.0", "id": 2, "method": "ping" }
            ]),
            None,
        )
        .unwrap();
        let batch = response.as_array().unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0]["id"], 2);
    }

    #[test]
    fn invalid_request_without_method_gets_a_json_rpc_error() {
        let root = temp_dir();
        let store = MemoryStore::open(root.join("memory.db")).unwrap();
        let response = dispatch(
            &store,
            &serde_json::json!({ "jsonrpc": "2.0", "id": 7 }),
            None,
        )
        .unwrap();
        assert_eq!(response["id"], 7);
        assert_eq!(response["error"]["code"], -32600);
    }

    #[test]
    fn scoped_server_rejects_a_cwd_from_another_project() {
        let root = temp_dir();
        let one = root.join("one");
        let two = root.join("two");
        init_git_repo(&one);
        init_git_repo(&two);
        let store = MemoryStore::open(root.join("memory.db")).unwrap();
        let scope = McpScope::from_root(&one);
        let response = dispatch(
            &store,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/call",
                "params": {
                    "name": "memory_context",
                    "arguments": { "cwd": two }
                }
            }),
            Some(&scope),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("fuera del proyecto"));
    }

    #[test]
    fn invalid_proposal_scope_does_not_fall_back_to_project() {
        let root = temp_dir();
        let repo = root.join("repo");
        init_git_repo(&repo);
        let store = MemoryStore::open(root.join("memory.db")).unwrap();
        let response = dispatch(
            &store,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "tools/call",
                "params": {
                    "name": "memory_propose",
                    "arguments": {
                        "cwd": repo,
                        "key": "decision/x",
                        "content": "valor",
                        "scope": "projet"
                    }
                }
            }),
            None,
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert!(store
            .list_visible(&repo, crate::memory::MemoryStatus::Candidate)
            .unwrap()
            .is_empty());
    }
}
