//! Exact baseline v2 migrate/configure bodies extracted from the pinned input.
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::time::Duration;
const SCHEMA_VERSION: i64 = 2;
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // Memoria curada es estado de usuario, no caché. FULL evita confirmar una
    // transacción que todavía podría perderse ante corte de energía.
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

pub(super) fn migrate(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let conn = &tx;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY
        );",
    )?;
    let current: Option<i64> = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .optional()?
        .flatten();
    if current.is_some_and(|version| version > SCHEMA_VERSION) {
        anyhow::bail!(
            "memory.db usa schema {}, pero esta versión sólo soporta {}",
            current.unwrap_or_default(),
            SCHEMA_VERSION
        );
    }
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY,
            identity_kind TEXT NOT NULL,
            identity_value TEXT NOT NULL,
            root_hint TEXT,
            remote_fingerprint TEXT,
            linked_project_id TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(identity_kind, identity_value)
        );
        CREATE TABLE IF NOT EXISTS tasks (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            identity_value TEXT NOT NULL,
            orchestrator_task_id TEXT,
            created_at INTEGER NOT NULL,
            UNIQUE(project_id, identity_value)
        );
        CREATE INDEX IF NOT EXISTS tasks_project_orchestrator
            ON tasks(project_id, orchestrator_task_id, created_at, id);
        CREATE TABLE IF NOT EXISTS memories (
            id TEXT PRIMARY KEY,
            scope_kind TEXT NOT NULL,
            scope_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            stable_key TEXT NOT NULL,
            status TEXT NOT NULL,
            trust_class TEXT NOT NULL,
            content TEXT NOT NULL,
            current_revision INTEGER NOT NULL,
            valid_until INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS memories_active_key
            ON memories(scope_kind, scope_id, stable_key)
            WHERE status = 'active';
        CREATE INDEX IF NOT EXISTS memories_scope_status
            ON memories(scope_kind, scope_id, status, kind);
        CREATE TABLE IF NOT EXISTS memory_revisions (
            id TEXT PRIMARY KEY,
            memory_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            operation TEXT NOT NULL,
            content TEXT NOT NULL,
            actor_kind TEXT NOT NULL,
            actor_id TEXT,
            created_at INTEGER NOT NULL,
            UNIQUE(memory_id, revision)
        );
        CREATE TABLE IF NOT EXISTS handoffs (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            summary TEXT NOT NULL,
            provider TEXT,
            session_id TEXT,
            expires_at INTEGER,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS handoffs_task_created
            ON handoffs(task_id, created_at DESC);
        CREATE TABLE IF NOT EXISTS mutation_receipts (
            client_id TEXT NOT NULL,
            request_id TEXT NOT NULL,
            response_json TEXT NOT NULL,
            committed_at INTEGER NOT NULL,
            PRIMARY KEY (client_id, request_id)
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            memory_id UNINDEXED,
            content,
            stable_key,
            tokenize = 'porter unicode61'
        );
        ",
    )?;
    if current != Some(SCHEMA_VERSION) {
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(version) VALUES (?1)",
            params![SCHEMA_VERSION],
        )?;
    }
    tx.commit()?;
    Ok(())
}
