//! App-owned conversations, transferable history, and durable delivery records.

pub mod launch;
pub mod mcp;
mod model;
mod store;
pub mod worker;

pub use model::*;
pub use store::ConversationStore;
#[cfg(test)]
mod tests;

pub fn database_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(crate::utils::app_paths::get()?
        .data
        .join("conversations.sqlite3"))
}
