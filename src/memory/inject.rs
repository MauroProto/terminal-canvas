//! Inyección en fronteras seguras: launch pack y respuesta de hook.
//!
//! Si el writer no está, el launch y el hook siguen siendo éxito con pack vacío.

use std::path::Path;

use serde_json::{json, Value};
use uuid::Uuid;

use super::context::{build_context_pack_scoped, format_context_pack};
use super::model::{HandoffRequest, ScopeKind};
use super::store::MemoryStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptBoundary {
    SessionStart,
    UserPromptSubmit,
    Stop,
    SessionEnd,
    Other,
}

impl PromptBoundary {
    pub fn from_event_name(name: &str) -> Self {
        match name.trim() {
            "SessionStart" | "session_start" => Self::SessionStart,
            "UserPromptSubmit" | "user_prompt_submit" => Self::UserPromptSubmit,
            "Stop" | "SubagentStop" | "stop" => Self::Stop,
            "SessionEnd" | "session_end" => Self::SessionEnd,
            _ => Self::Other,
        }
    }

    pub fn injects_context(self) -> bool {
        matches!(self, Self::SessionStart | Self::UserPromptSubmit)
    }

    pub fn records_handoff(self) -> bool {
        matches!(self, Self::Stop)
    }
}

/// Antepone el pack de memoria al brief de lanzamiento. `store = None` es fail-open.
pub fn launch_brief_with_memory(
    brief: &str,
    cwd: Option<&Path>,
    store: Option<&MemoryStore>,
    orchestrator_task_id: Option<Uuid>,
) -> String {
    launch_brief_with_memory_scoped(brief, cwd, store, orchestrator_task_id, None)
}

pub fn launch_brief_with_memory_scoped(
    brief: &str,
    cwd: Option<&Path>,
    store: Option<&MemoryStore>,
    orchestrator_task_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
) -> String {
    let extra = match (store, cwd) {
        (Some(store), Some(cwd)) => {
            match build_context_pack_scoped(store, cwd, None, orchestrator_task_id, workspace_id) {
                Ok(pack) => format_context_pack(&pack),
                Err(err) => {
                    log::warn!("memory launch pack unavailable: {err}");
                    String::new()
                }
            }
        }
        _ => String::new(),
    };
    if extra.is_empty() {
        return brief.to_owned();
    }
    if brief.trim().is_empty() {
        extra
    } else {
        format!("{brief}\n\n{extra}")
    }
}

/// Cuerpo JSON que el hook de Claude/Codex debe imprimir en stdout.
pub fn respond_to_hook(
    store: Option<&MemoryStore>,
    boundary: PromptBoundary,
    event_name: &str,
    cwd: Option<&Path>,
    handoff_summary: Option<&str>,
    orchestrator_task_id: Option<Uuid>,
) -> Option<Value> {
    respond_to_hook_scoped(
        store,
        boundary,
        event_name,
        cwd,
        handoff_summary,
        orchestrator_task_id,
        None,
    )
}

pub fn respond_to_hook_scoped(
    store: Option<&MemoryStore>,
    boundary: PromptBoundary,
    event_name: &str,
    cwd: Option<&Path>,
    handoff_summary: Option<&str>,
    orchestrator_task_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
) -> Option<Value> {
    let store = store?;
    if boundary.records_handoff() {
        if let Some(cwd) = cwd {
            if let Some(summary) = nonempty_handoff_summary(handoff_summary) {
                if let Err(err) = store.create_handoff(HandoffRequest {
                    cwd: cwd.to_path_buf(),
                    summary: summary.to_owned(),
                    provider: None,
                    session_id: None,
                    orchestrator_task_id,
                    ttl_secs: Some(72 * 3600),
                }) {
                    log::warn!("memory handoff on stop failed: {err}");
                }
            }
        }
        // Un hook de observación exitoso debe quedar en silencio. Tanto Claude
        // como Codex validan cualquier JSON de stdout como una salida de hook.
        return None;
    }
    if !boundary.injects_context() {
        return None;
    }
    let pack = cwd.and_then(|cwd| {
        build_context_pack_scoped(store, cwd, None, orchestrator_task_id, workspace_id).ok()
    });
    let text = pack.as_ref().map(format_context_pack).unwrap_or_default();
    if text.is_empty() {
        return None;
    }
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": event_name,
            "additionalContext": text
        }
    }))
}

/// Un Stop típico de Claude no trae resumen: no inventar un handoff genérico
/// que tape uno explícito.
fn nonempty_handoff_summary(handoff_summary: Option<&str>) -> Option<&str> {
    handoff_summary
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// Texto que muestra la UI para un remember: distingue commit, dedupe y conflicto.
pub fn format_remember_status(result: &super::model::WriteResult) -> String {
    match result {
        super::model::WriteResult::Committed(write) => {
            format!(
                "Guardado como memoria {} (rev {}).",
                write.memory.status.as_str(),
                write.committed_revision
            )
        }
        super::model::WriteResult::Deduped(write) => {
            format!(
                "Ya existía el mismo contenido en `{}` (rev {}); no se reescribió.",
                write.memory.stable_key, write.committed_revision
            )
        }
        super::model::WriteResult::Conflict(write) => {
            format!(
                "Conflicto: `{}` ya tiene otra versión (rev {}). No se pisó.",
                write.current.stable_key, write.current.current_revision
            )
        }
    }
}

pub fn remember_selection(
    store: &MemoryStore,
    cwd: &Path,
    key: &str,
    content: &str,
    scope: ScopeKind,
) -> anyhow::Result<super::model::WriteResult> {
    store.remember(super::model::RememberRequest {
        client_id: "terminalcanvas-ui".to_owned(),
        request_id: uuid::Uuid::new_v4().to_string(),
        cwd: cwd.to_path_buf(),
        scope,
        kind: super::model::MemoryKind::Decision,
        key: key.to_owned(),
        content: content.to_owned(),
        actor: super::model::Actor::human(),
        expected_revision: None,
        orchestrator_task_id: None,
        workspace_id: None,
    })
}

/// Store de proceso para launch/hooks reales. En tests queda `None` para no
/// tocar la base del usuario ni acoplar pruebas paralelas.
pub fn process_store() -> Option<MemoryStore> {
    if cfg!(test) {
        return None;
    }
    static STORE: std::sync::OnceLock<MemoryStore> = std::sync::OnceLock::new();
    if let Some(store) = STORE.get() {
        return Some(store.clone());
    }
    match MemoryStore::open_default() {
        Ok(store) => {
            let _ = STORE.set(store);
            STORE.get().cloned()
        }
        Err(err) => {
            log::warn!("memory store unavailable: {err}");
            None
        }
    }
}
