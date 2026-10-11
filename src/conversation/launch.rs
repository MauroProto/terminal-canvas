//! Per-launch MCP configuration. Never edits a provider's user/project settings.
use super::*;
use crate::orchestration::AgentProvider;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
pub struct CredentialFile {
    pub database: PathBuf,
    pub conversation_id: Uuid,
    pub token: String,
}

pub struct McpConfig {
    pub path: PathBuf,
    pub auth_path: PathBuf,
}

fn private_json(directory: &Path, value: &impl Serialize) -> Result<PathBuf> {
    let mut dir = std::fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        dir.mode(0o700);
    }
    dir.create(directory)?;
    anyhow::ensure!(
        !std::fs::symlink_metadata(directory)?
            .file_type()
            .is_symlink(),
        "El directorio MCP no puede ser un enlace"
    );
    let path = directory.join(format!("{}.json", Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    Ok(path)
}

pub fn issue_config(
    store: &ConversationStore,
    id: Uuid,
    database: &Path,
    directory: &Path,
    executable: &Path,
) -> Result<McpConfig> {
    store.authorize(Caller::User, id)?;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let auth_path = private_json(
        directory,
        &CredentialFile {
            database: database.to_owned(),
            conversation_id: id,
            token: token.clone(),
        },
    )?;
    let profile = crate::utils::app_paths::get()?.isolated_root.clone();
    let mut env = serde_json::json!({"TC_CHAT_AUTH_FILE":auth_path});
    if let Some(profile) = profile {
        env["TERMINAL_CANVAS_HOME"] = serde_json::json!(profile);
    }
    let config = serde_json::json!({"mcpServers":{"terminal_canvas_chat":{
        "command":executable,"args":["--chat-mcp"],"env":env
    }}});
    let path = private_json(directory, &config)?;
    store.activate_credential(id, &token)?;
    Ok(McpConfig { path, auth_path })
}

pub fn append_config(
    command: &mut Command,
    provider: AgentProvider,
    config: &McpConfig,
    executable: &Path,
) -> Result<()> {
    match provider {
        AgentProvider::ClaudeCode => {
            command.arg("--mcp-config").arg(&config.path);
        }
        AgentProvider::CodexCli => {
            command.arg("-c").arg(format!(
                "mcp_servers.terminal_canvas_chat.command={}",
                serde_json::to_string(
                    executable
                        .to_str()
                        .context("Ruta del ejecutable no UTF-8")?
                )?
            ));
            command.args([
                "-c",
                "mcp_servers.terminal_canvas_chat.args=[\"--chat-mcp\"]",
            ]);
            command.arg("-c").arg(format!(
                "mcp_servers.terminal_canvas_chat.env.TC_CHAT_AUTH_FILE={}",
                serde_json::to_string(config.auth_path.to_str().context("Ruta MCP no UTF-8")?)?
            ));
        }
        _ => anyhow::bail!("El proveedor requiere importar la configuración MCP manualmente"),
    }
    Ok(())
}

/// Only new app-owned launches carry all three stable identity variables.
pub fn configure_app_launch(
    command: &mut Command,
    provider: AgentProvider,
    brief: &str,
) -> Result<()> {
    if !matches!(
        provider,
        AgentProvider::ClaudeCode | AgentProvider::CodexCli
    ) {
        return Ok(());
    }
    let identity = |key: &str| {
        std::env::var(key)
            .ok()
            .and_then(|s| Uuid::parse_str(&s).ok())
    };
    let (Some(id), Some(workspace_id), Some(panel_id)) = (
        identity("TC_LEAF_ID"),
        identity("TC_WORKSPACE_ID"),
        identity("TC_PANEL_ID"),
    ) else {
        return Ok(());
    };
    let database = database_path()?;
    let store = ConversationStore::open(&database)?;
    let mut conversation = store.conversation(id)?.unwrap_or(Conversation {
        id,
        workspace_id,
        panel_id,
        title: provider.label().to_owned(),
        provider: Some(provider),
        cwd: None,
        memory_task_id: identity("TC_MEMORY_TASK_ID"),
        native_session_id: None,
        runtime_session_id: None,
        binding_id: None,
    });
    anyhow::ensure!(
        conversation.workspace_id == workspace_id && conversation.panel_id == panel_id,
        "El lanzamiento no coincide con la conversación registrada"
    );
    conversation.provider = Some(provider);
    conversation.native_session_id = None;
    conversation.cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    store.register(&conversation)?;
    if !brief.trim().is_empty() {
        store.record(
            Caller::User,
            Uuid::new_v4(),
            id,
            MessageRole::User,
            brief,
            "launch_brief",
        )?;
    }
    let executable = std::env::current_exe()?;
    let config = issue_config(
        &store,
        id,
        &database,
        &crate::utils::app_paths::get()?.data.join("chat-mcp"),
        &executable,
    )?;
    append_config(command, provider, &config, &executable)
}
