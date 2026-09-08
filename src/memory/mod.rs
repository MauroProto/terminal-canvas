//! Memory Hub local: identidad de proyecto, store SQLite y pack de contexto.
//!
//! Una sola semántica de dominio y clientes delgados: app, hooks, CLI y MCP.
//! El MVP aún deja que esos procesos compartan SQLite; `tc-memoryd` será la
//! autoridad única de escritura en la siguiente etapa.

mod cli;
mod context;
mod identity;
mod inject;
mod mcp;
mod model;
mod redaction;
mod store;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use cli::run as run_cli;
pub use context::{
    build_context_pack, build_context_pack_scoped, format_context_pack, pack_contains,
};
pub use identity::{fingerprint_remote, resolve_location};
pub use inject::{
    format_remember_status, launch_brief_with_memory, launch_brief_with_memory_scoped,
    process_store, remember_selection, respond_to_hook, respond_to_hook_scoped, PromptBoundary,
};
pub use mcp::{handle_message_json, serve_stdio as serve_mcp_stdio};
pub use model::{
    Actor, ContextPack, HandoffRecord, HandoffRequest, IdentityKind, MemoryKind, MemoryRecord,
    MemoryStatus, RememberRequest, ResolvedLocation, ResolvedProject, ScopeKind, TrustClass,
    WriteResult,
};
pub use store::{default_db_path, MemoryStore};
