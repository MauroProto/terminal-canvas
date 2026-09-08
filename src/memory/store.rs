//! Store transaccional de memoria: SQLite WAL + FTS5, recibos e idempotencia.
//! En el MVP varios procesos abren este store; SQLite serializa las escrituras
//! hasta que `tc-memoryd` concentre la autoridad en una fase posterior.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rusqlite::{
    params, Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::identity::resolve_location;
use super::model::{
    contents_equivalent, new_prefixed_id, normalize_key, Actor, CommittedWrite, ConflictWrite,
    HandoffRecord, HandoffRequest, IdentityKind, MemoryKind, MemoryRecord, MemoryStatus,
    RememberRequest, ResolvedProject, ScopeKind, TrustClass, WriteResult, MAX_CLIENT_ID_CHARS,
    MAX_HANDOFF_CHARS, MAX_MEMORY_CONTENT_CHARS, MAX_MEMORY_KEY_CHARS,
};
use super::redaction::prepare_content;

const SCHEMA_VERSION: i64 = 2;
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const BUSY_RETRY_ATTEMPTS: usize = 6;
pub(crate) const MAX_SEARCH_RESULTS: usize = 100;
pub(crate) const MAX_SEARCH_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_SEARCH_QUERY_CHARS: usize = 512;
const MAX_FTS_TERMS: usize = 32;
const MAX_CONTEXT_CANDIDATES_PER_SCOPE: usize = 32;

#[derive(Debug, Clone)]
pub struct MemoryStore {
    path: PathBuf,
}

impl MemoryStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("crear {}", parent.display()))?;
            if platform_default_db_path().as_deref() == Some(path.as_path()) {
                protect_private_directory(parent)?;
            }
        }
        prepare_private_db_file(&path)?;
        let store = Self { path };
        retry_busy(|| {
            store.with_conn(|conn| {
                // `journal_mode` persiste en el archivo. Ejecutarlo aquí evita
                // que cada operación concurrente intente reconfigurarlo.
                conn.pragma_update(None, "journal_mode", "WAL")?;
                migrate(conn)?;
                Ok(())
            })
        })?;
        Ok(store)
    }

    pub fn open_default() -> Result<Self> {
        Self::open(default_db_path().ok_or_else(|| anyhow!("no hay data dir"))?)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn with_conn<T>(&self, body: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut conn = Connection::open(&self.path)
            .with_context(|| format!("abrir {}", self.path.display()))?;
        configure(&conn)?;
        body(&mut conn)
    }

    /// Una operación de memoria puede competir con la UI, la CLI y MCP. Un
    /// BUSY_SNAPSHOT exige abandonar el snapshot y repetir la transacción
    /// completa; esperar o reintentar sólo el statement no puede resolverlo.
    fn with_immediate_tx<T>(
        &self,
        mut body: impl FnMut(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        retry_busy(|| {
            self.with_conn(|conn| {
                let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let value = body(&tx)?;
                tx.commit()?;
                Ok(value)
            })
        })
    }

    pub fn resolve_project(&self, cwd: &Path) -> Result<ResolvedProject> {
        let location = resolve_location(cwd);
        self.with_immediate_tx(|tx| upsert_project(tx, &location))
    }

    pub fn link_projects(&self, cwd_a: &Path, cwd_b: &Path) -> Result<ResolvedProject> {
        let location_a = resolve_location(cwd_a);
        let location_b = resolve_location(cwd_b);
        self.with_immediate_tx(|tx| {
            let a = upsert_project(tx, &location_a)?;
            let b = upsert_project(tx, &location_b)?;
            let canonical = a.canonical_id.clone();
            if b.canonical_id != canonical {
                merge_project_state(tx, &b.canonical_id, &canonical)?;
                tx.execute(
                    "UPDATE projects SET linked_project_id = ?1, updated_at = ?2 WHERE id = ?3",
                    params![canonical, now_secs(), b.canonical_id],
                )?;
            }
            load_project(tx, &a.id)?.ok_or_else(|| anyhow!("proyecto perdido tras vincular"))
        })
    }

    pub fn remember(&self, request: RememberRequest) -> Result<WriteResult> {
        validate_client_identifier("client_id", &request.client_id)?;
        validate_client_identifier("request_id", &request.request_id)?;
        let request_fingerprint = fingerprint_request(&request)?;
        let location = resolve_location(&request.cwd);
        self.with_immediate_tx(|tx| {
            if let Some(cached) = load_receipt(
                tx,
                &request.client_id,
                &request.request_id,
                &request_fingerprint,
            )? {
                return Ok(cached);
            }
            let result = remember_inner(tx, request.clone(), &location)?;
            save_receipt(
                tx,
                request.client_id.clone(),
                request.request_id.clone(),
                request_fingerprint.clone(),
                &result,
            )?;
            Ok(result)
        })
    }

    pub fn propose(&self, mut request: RememberRequest) -> Result<WriteResult> {
        if request.actor.is_human() {
            request.actor = Actor::agent("unspecified");
        }
        self.remember(request)
    }

    pub fn approve(&self, memory_id: &str, actor: &Actor) -> Result<MemoryRecord> {
        require_human(actor, "aprobar")?;
        self.with_immediate_tx(|tx| {
            let mut memory =
                load_memory(tx, memory_id)?.ok_or_else(|| anyhow!("memoria no encontrada"))?;
            if memory.status != MemoryStatus::Candidate {
                anyhow::bail!("solo se aprueban candidatas");
            }
            let mut next_revision = revision_after(memory.current_revision)?;
            if let Some(mut existing) =
                find_active_key(tx, memory.scope_kind, &memory.scope_id, &memory.stable_key)?
            {
                if existing.id != memory.id {
                    // La candidata aprobada es la nueva versión. No se crea
                    // una tercera fila activa: eso dejaría dos decisiones
                    // contradictorias y puede violar la unicidad lógica.
                    existing.status = MemoryStatus::Superseded;
                    existing.current_revision = revision_after(existing.current_revision)?;
                    existing.updated_at = now_secs();
                    next_revision = next_revision.max(revision_after(existing.current_revision)?);
                    persist_memory(tx, &existing)?;
                    append_revision(tx, &existing, "supersede_on_approve", actor)?;
                    delete_fts(tx, &existing.id)?;
                }
            }
            memory.status = MemoryStatus::Active;
            memory.current_revision = next_revision;
            memory.updated_at = now_secs();
            persist_memory(tx, &memory)?;
            append_revision(tx, &memory, "approve", actor)?;
            upsert_fts(tx, &memory)?;
            Ok(memory)
        })
    }

    pub fn reject(&self, memory_id: &str, actor: &Actor) -> Result<MemoryRecord> {
        require_human(actor, "rechazar")?;
        self.set_status(
            memory_id,
            MemoryStatus::Rejected,
            actor,
            "reject",
            false,
            Some(MemoryStatus::Candidate),
        )
    }

    pub fn forget(&self, memory_id: &str, actor: &Actor) -> Result<MemoryRecord> {
        require_human(actor, "olvidar")?;
        self.set_status(
            memory_id,
            MemoryStatus::Tombstoned,
            actor,
            "forget",
            true,
            None,
        )
    }

    fn set_status(
        &self,
        memory_id: &str,
        status: MemoryStatus,
        actor: &Actor,
        operation: &str,
        drop_fts: bool,
        expected_status: Option<MemoryStatus>,
    ) -> Result<MemoryRecord> {
        self.with_immediate_tx(|tx| {
            let mut memory =
                load_memory(tx, memory_id)?.ok_or_else(|| anyhow!("memoria no encontrada"))?;
            if expected_status.is_some_and(|expected| memory.status != expected) {
                anyhow::bail!("estado de memoria inválido para {operation}");
            }
            memory.status = status;
            memory.current_revision = revision_after(memory.current_revision)?;
            memory.updated_at = now_secs();
            persist_memory(tx, &memory)?;
            append_revision(tx, &memory, operation, actor)?;
            if drop_fts {
                delete_fts(tx, &memory.id)?;
            }
            Ok(memory)
        })
    }

    pub fn get(&self, memory_id: &str) -> Result<Option<MemoryRecord>> {
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            load_memory(&tx, memory_id)
        })
    }

    /// Recupera una memoria activa sólo si pertenece a un scope visible desde
    /// el cwd. Es la variante segura para clientes MCP y otros agentes.
    pub fn get_visible(&self, cwd: &Path, memory_id: &str) -> Result<Option<MemoryRecord>> {
        self.get_visible_scoped(cwd, memory_id, None)
    }

    pub fn get_visible_scoped(
        &self,
        cwd: &Path,
        memory_id: &str,
        orchestrator_task_id: Option<Uuid>,
    ) -> Result<Option<MemoryRecord>> {
        let location = resolve_location(cwd);
        self.with_immediate_tx(|tx| {
            let scopes = visible_scopes(tx, &location, orchestrator_task_id, None)?;
            let memory = load_memory(tx, memory_id)?;
            let visible = memory.filter(|memory| {
                memory.status.is_retrievable()
                    && !is_expired(memory.valid_until)
                    && scopes
                        .iter()
                        .any(|scope| scope.kind == memory.scope_kind && scope.id == memory.scope_id)
            });
            Ok(visible)
        })
    }

    pub fn list_visible(&self, cwd: &Path, status: MemoryStatus) -> Result<Vec<MemoryRecord>> {
        self.list_visible_for_workspace(cwd, status, None)
    }

    pub fn list_visible_for_workspace(
        &self,
        cwd: &Path,
        status: MemoryStatus,
        workspace_id: Option<Uuid>,
    ) -> Result<Vec<MemoryRecord>> {
        self.list_visible_scoped(cwd, status, None, workspace_id)
    }

    pub fn list_visible_scoped(
        &self,
        cwd: &Path,
        status: MemoryStatus,
        orchestrator_task_id: Option<Uuid>,
        workspace_id: Option<Uuid>,
    ) -> Result<Vec<MemoryRecord>> {
        let location = resolve_location(cwd);
        self.with_immediate_tx(|tx| {
            let scopes = visible_scopes(tx, &location, orchestrator_task_id, workspace_id)?;
            list_by_scopes(tx, &scopes, status)
        })
    }

    pub fn search(&self, cwd: &Path, query: &str) -> Result<Vec<MemoryRecord>> {
        self.search_scoped(cwd, query, None)
    }

    pub fn search_scoped(
        &self,
        cwd: &Path,
        query: &str,
        orchestrator_task_id: Option<Uuid>,
    ) -> Result<Vec<MemoryRecord>> {
        let location = resolve_location(cwd);
        self.with_immediate_tx(|tx| {
            let scopes = visible_scopes(tx, &location, orchestrator_task_id, None)?;
            search_visible(tx, &scopes, query)
        })
    }

    pub fn create_handoff(&self, request: HandoffRequest) -> Result<HandoffRecord> {
        let location = resolve_location(&request.cwd);
        self.with_immediate_tx(|tx| {
            let request = request.clone();
            let project = upsert_project(tx, &location)?;
            let task = upsert_task(
                tx,
                &project.canonical_id,
                &location,
                request.orchestrator_task_id,
            )?;
            let now = now_secs();
            let summary = prepare_content(&request.summary);
            if summary.is_empty() {
                anyhow::bail!("el handoff no puede estar vacío");
            }
            if summary.chars().count() > MAX_HANDOFF_CHARS {
                anyhow::bail!("el handoff supera el límite de {MAX_HANDOFF_CHARS} caracteres");
            }
            let expires_at = match request.ttl_secs {
                Some(ttl) if ttl <= 0 => anyhow::bail!("el TTL del handoff debe ser positivo"),
                Some(ttl) => Some(
                    now.checked_add(ttl)
                        .ok_or_else(|| anyhow!("el TTL del handoff es demasiado grande"))?,
                ),
                None => None,
            };
            let record = HandoffRecord {
                id: new_prefixed_id("hnd_"),
                project_id: project.canonical_id,
                task_id: task,
                summary,
                provider: request.provider,
                session_id: request.session_id,
                expires_at,
                created_at: now,
            };
            tx.execute(
                "INSERT INTO handoffs(
                    id, project_id, task_id, summary, provider, session_id, expires_at, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    record.id,
                    record.project_id,
                    record.task_id,
                    record.summary,
                    record.provider,
                    record.session_id,
                    record.expires_at,
                    record.created_at
                ],
            )?;
            Ok(record)
        })
    }

    pub fn latest_handoff(
        &self,
        cwd: &Path,
        orchestrator_task_id: Option<Uuid>,
    ) -> Result<Option<HandoffRecord>> {
        let location = resolve_location(cwd);
        self.with_immediate_tx(|tx| {
            let project = upsert_project(tx, &location)?;
            let task = upsert_task(tx, &project.canonical_id, &location, orchestrator_task_id)?;
            load_latest_handoff(tx, &task)
        })
    }

    pub fn export_markdown(&self, cwd: &Path) -> Result<String> {
        self.export_markdown_scoped(cwd, None)
    }

    pub fn export_markdown_scoped(
        &self,
        cwd: &Path,
        orchestrator_task_id: Option<Uuid>,
    ) -> Result<String> {
        let project = self.resolve_project(cwd)?;
        let active =
            self.list_visible_scoped(cwd, MemoryStatus::Active, orchestrator_task_id, None)?;
        let pending =
            self.list_visible_scoped(cwd, MemoryStatus::Candidate, orchestrator_task_id, None)?;
        let mut out = String::new();
        out.push_str("# TerminalCanvas shared memory\n\n");
        out.push_str(&format!(
            "project: `{}` ({:?})\n\n",
            project.canonical_id, project.identity_kind
        ));
        out.push_str("## Active\n\n");
        for memory in &active {
            out.push_str(&format_memory_md(memory));
        }
        if active.is_empty() {
            out.push_str("_none_\n\n");
        }
        out.push_str("## Pending\n\n");
        for memory in &pending {
            out.push_str(&format_memory_md(memory));
        }
        if pending.is_empty() {
            out.push_str("_none_\n");
        }
        Ok(out)
    }

    pub(crate) fn load_visible_active(
        &self,
        cwd: &Path,
        orchestrator_task_id: Option<Uuid>,
        workspace_id: Option<Uuid>,
    ) -> Result<(
        ResolvedProject,
        String,
        Vec<MemoryRecord>,
        Option<HandoffRecord>,
    )> {
        let location = resolve_location(cwd);
        self.with_immediate_tx(|tx| {
            let project = upsert_project(tx, &location)?;
            let task = upsert_task(tx, &project.canonical_id, &location, orchestrator_task_id)?;
            let scopes = scopes_for(&project, &task, location.cwd.as_path(), workspace_id);
            let memories = list_by_scopes_with_limit(
                tx,
                &scopes,
                MemoryStatus::Active,
                Some(MAX_CONTEXT_CANDIDATES_PER_SCOPE),
            )?;
            let handoff = load_latest_handoff(tx, &task)?;
            Ok((project, task, memories, handoff))
        })
    }

    pub(crate) fn search_for_context(
        &self,
        cwd: &Path,
        query: &str,
        orchestrator_task_id: Option<Uuid>,
        workspace_id: Option<Uuid>,
    ) -> Result<Vec<MemoryRecord>> {
        let location = resolve_location(cwd);
        self.with_immediate_tx(|tx| {
            let scopes = visible_scopes(tx, &location, orchestrator_task_id, workspace_id)?;
            search_visible(tx, &scopes, query)
        })
    }
}

/// Migra el estado compartible del proyecto fuente antes de enlazarlo. Las
/// claves activas que colisionan no se pisan: pasan a candidatas para revisión.
fn merge_project_state(tx: &Transaction<'_>, source: &str, target: &str) -> Result<()> {
    merge_memory_scope(tx, ScopeKind::Project, source, target)?;
    let tasks = {
        let mut stmt = tx.prepare(
            "SELECT id, identity_value, orchestrator_task_id FROM tasks WHERE project_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![source], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (id, identity, orchestrator_id) in tasks {
        let matching: Option<String> = tx
            .query_row(
                "SELECT id FROM tasks WHERE project_id = ?1
                 AND (identity_value = ?2 OR orchestrator_task_id = ?3)
                 ORDER BY created_at, id LIMIT 1",
                params![target, identity, orchestrator_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = matching {
            merge_memory_scope(tx, ScopeKind::Task, &id, &existing)?;
            tx.execute(
                "UPDATE handoffs SET task_id = ?1 WHERE task_id = ?2",
                params![existing, id],
            )?;
            tx.execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
        } else {
            tx.execute(
                "UPDATE tasks SET project_id = ?1 WHERE id = ?2",
                params![target, id],
            )?;
        }
    }
    tx.execute(
        "UPDATE handoffs SET project_id = ?1 WHERE project_id = ?2",
        params![target, source],
    )?;
    Ok(())
}

fn merge_memory_scope(
    tx: &Transaction<'_>,
    kind: ScopeKind,
    source: &str,
    target: &str,
) -> Result<()> {
    let memories = {
        let mut stmt = tx.prepare(
            "SELECT id, scope_kind, scope_id, kind, stable_key, status, trust_class, content,
                    current_revision, valid_until, created_at, updated_at
             FROM memories WHERE scope_kind = ?1 AND scope_id = ?2",
        )?;
        let rows = stmt
            .query_map(params![kind.as_str(), source], row_to_memory)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let actor = Actor::human();
    for mut memory in memories {
        let collision = memory.status == MemoryStatus::Active
            && find_active_key(tx, kind, target, &memory.stable_key)?.is_some();
        if collision {
            memory.status = MemoryStatus::Candidate;
            delete_fts(tx, &memory.id)?;
        }
        memory.scope_id = target.to_owned();
        memory.current_revision = revision_after(memory.current_revision)?;
        memory.updated_at = now_secs();
        tx.execute(
            "UPDATE memories
             SET scope_id = ?1, status = ?2, current_revision = ?3, updated_at = ?4
             WHERE id = ?5",
            params![
                memory.scope_id,
                memory.status.as_str(),
                memory.current_revision,
                memory.updated_at,
                memory.id
            ],
        )?;
        append_revision(
            tx,
            &memory,
            if collision {
                "link_project_conflict"
            } else {
                "link_project"
            },
            &actor,
        )?;
    }

    Ok(())
}

pub fn default_db_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("TC_MEMORY_DB") {
        if !path.trim().is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    platform_default_db_path()
}

fn platform_default_db_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "terminal-app")
        .map(|dirs| dirs.data_dir().join("memory").join("memory.db"))
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // Memoria curada es estado de usuario, no caché. FULL evita confirmar una
    // transacción que todavía podría perderse ante corte de energía.
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn retry_busy<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    for attempt in 0..BUSY_RETRY_ATTEMPTS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if is_busy_error(&error) && attempt + 1 < BUSY_RETRY_ATTEMPTS => {
                let delay_ms = 10_u64.saturating_mul(1_u64 << attempt.min(6));
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("el último intento siempre retorna")
}

fn is_busy_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .and_then(rusqlite::Error::sqlite_error_code)
            .is_some_and(|code| matches!(code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked))
    })
}

fn migrate(conn: &mut Connection) -> Result<()> {
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

fn remember_inner(
    tx: &Transaction<'_>,
    request: RememberRequest,
    loc: &super::model::ResolvedLocation,
) -> Result<WriteResult> {
    let project = upsert_project(tx, loc)?;
    let task = upsert_task(tx, &project.canonical_id, loc, request.orchestrator_task_id)?;
    if matches!(request.scope, ScopeKind::User) && !request.actor.is_human() {
        anyhow::bail!("solo una acción humana puede crear memoria de usuario");
    }
    let scope_id = match request.scope {
        ScopeKind::Project => project.canonical_id.clone(),
        ScopeKind::Task => task,
        ScopeKind::Workspace => request
            .workspace_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| loc.cwd.to_string_lossy().into_owned()),
        ScopeKind::User => "user".to_owned(),
    };
    let key = normalize_key(&super::redaction::strip_invisible(&request.key));
    if key.is_empty() {
        anyhow::bail!("la clave no puede estar vacía");
    }
    if key.chars().count() > MAX_MEMORY_KEY_CHARS {
        anyhow::bail!("la clave supera el límite de {MAX_MEMORY_KEY_CHARS} caracteres");
    }
    let content = prepare_content(&request.content);
    if content.is_empty() {
        anyhow::bail!("el contenido no puede estar vacío");
    }
    if content.chars().count() > MAX_MEMORY_CONTENT_CHARS {
        anyhow::bail!("el contenido supera el límite de {MAX_MEMORY_CONTENT_CHARS} caracteres");
    }
    let existing = find_latest_key(tx, request.scope, &scope_id, &key)?;
    let desired_status = if request.actor.is_human() {
        MemoryStatus::Active
    } else {
        MemoryStatus::Candidate
    };

    if let Some(existing) = existing {
        if existing.status == MemoryStatus::Tombstoned || existing.status == MemoryStatus::Rejected
        {
            return insert_fresh(tx, request, scope_id, key, content, desired_status);
        }
        let equivalent = contents_equivalent(&existing.content, &content);
        if equivalent && existing.status == desired_status {
            let write = CommittedWrite {
                receipt_id: new_prefixed_id("rcp_"),
                committed_revision: existing.current_revision,
                memory: existing,
            };
            return Ok(WriteResult::Deduped(write));
        }
        // Conocer la revisión actual evita un lost update, pero no concede a
        // un agente autoridad para retirar una memoria ya aprobada.
        if !request.actor.is_human() && existing.status == MemoryStatus::Active {
            if equivalent {
                let write = CommittedWrite {
                    receipt_id: new_prefixed_id("rcp_"),
                    committed_revision: existing.current_revision,
                    memory: existing,
                };
                return Ok(WriteResult::Deduped(write));
            }
            let inserted = insert_conflict_candidate(
                tx,
                &scope_id,
                request.scope,
                request.kind,
                &key,
                &content,
                &request.actor,
            )?;
            return Ok(WriteResult::Conflict(ConflictWrite {
                current: existing,
                incoming_content: content,
                candidate_id: Some(inserted.id),
            }));
        }
        let revision_ok = request
            .expected_revision
            .is_some_and(|expected| expected == existing.current_revision);
        if !revision_ok {
            let candidate_id = if !equivalent {
                let inserted = insert_conflict_candidate(
                    tx,
                    &scope_id,
                    request.scope,
                    request.kind,
                    &key,
                    &content,
                    &request.actor,
                )?;
                Some(inserted.id)
            } else {
                None
            };
            return Ok(WriteResult::Conflict(ConflictWrite {
                current: existing,
                incoming_content: content,
                candidate_id,
            }));
        }
        let updated = supersede_row(tx, &existing, &content, &request.actor)?;
        let write = CommittedWrite {
            receipt_id: new_prefixed_id("rcp_"),
            committed_revision: updated.current_revision,
            memory: updated,
        };
        return Ok(WriteResult::Committed(write));
    }

    insert_fresh(tx, request, scope_id, key, content, desired_status)
}

fn insert_fresh(
    tx: &Transaction<'_>,
    request: RememberRequest,
    scope_id: String,
    key: String,
    content: String,
    status: MemoryStatus,
) -> Result<WriteResult> {
    let now = now_secs();
    let memory = MemoryRecord {
        id: new_prefixed_id("mem_"),
        scope_kind: request.scope,
        scope_id,
        kind: request.kind,
        stable_key: key,
        status,
        trust_class: request.actor.trust(),
        content,
        current_revision: 1,
        valid_until: None,
        created_at: now,
        updated_at: now,
    };
    persist_memory(tx, &memory)?;
    append_revision(tx, &memory, "remember", &request.actor)?;
    if memory.status.is_retrievable() {
        upsert_fts(tx, &memory)?;
    }
    let write = CommittedWrite {
        receipt_id: new_prefixed_id("rcp_"),
        committed_revision: memory.current_revision,
        memory,
    };
    Ok(WriteResult::Committed(write))
}

fn insert_conflict_candidate(
    tx: &Transaction<'_>,
    scope_id: &str,
    scope: ScopeKind,
    kind: MemoryKind,
    key: &str,
    content: &str,
    actor: &Actor,
) -> Result<MemoryRecord> {
    let now = now_secs();
    let memory = MemoryRecord {
        id: new_prefixed_id("mem_"),
        scope_kind: scope,
        scope_id: scope_id.to_owned(),
        kind,
        // El status candidate permite varias filas con la misma clave; el
        // índice único sólo cubre activas. Conservar la clave original hace
        // que approve pueda reemplazar exactamente la decisión en conflicto.
        stable_key: key.to_owned(),
        status: MemoryStatus::Candidate,
        trust_class: actor.trust(),
        content: content.to_owned(),
        current_revision: 1,
        valid_until: None,
        created_at: now,
        updated_at: now,
    };
    persist_memory(tx, &memory)?;
    append_revision(tx, &memory, "conflict", actor)?;
    Ok(memory)
}

fn supersede_row(
    tx: &Transaction<'_>,
    existing: &MemoryRecord,
    content: &str,
    actor: &Actor,
) -> Result<MemoryRecord> {
    let now = now_secs();
    let next_revision = revision_after(existing.current_revision)?;
    let mut previous = existing.clone();
    previous.status = MemoryStatus::Superseded;
    previous.updated_at = now;
    persist_memory(tx, &previous)?;
    delete_fts(tx, &previous.id)?;

    let mut next = existing.clone();
    next.id = new_prefixed_id("mem_");
    next.content = content.to_owned();
    next.status = if actor.is_human() {
        MemoryStatus::Active
    } else {
        MemoryStatus::Candidate
    };
    next.trust_class = actor.trust();
    next.current_revision = next_revision;
    next.created_at = now;
    next.updated_at = now;
    persist_memory(tx, &next)?;
    append_revision(tx, &next, "supersede", actor)?;
    if next.status.is_retrievable() {
        upsert_fts(tx, &next)?;
    }
    Ok(next)
}

fn revision_after(current: u32) -> Result<u32> {
    current
        .checked_add(1)
        .ok_or_else(|| anyhow!("la memoria alcanzó el máximo de revisiones"))
}

fn upsert_project(
    tx: &Transaction<'_>,
    loc: &super::model::ResolvedLocation,
) -> Result<ResolvedProject> {
    let kind = match loc.identity_kind {
        IdentityKind::GitCommonDir => "git_common_dir",
        IdentityKind::WorkspaceRoot => "workspace_root",
    };
    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM projects WHERE identity_kind = ?1 AND identity_value = ?2",
            params![kind, loc.identity_value],
            |row| row.get(0),
        )
        .optional()?;
    let now = now_secs();
    let id = if let Some(id) = existing {
        tx.execute(
            "UPDATE projects SET remote_fingerprint = COALESCE(?1, remote_fingerprint),
                 root_hint = ?2, updated_at = ?3 WHERE id = ?4",
            params![
                loc.remote_fingerprint,
                loc.worktree_root
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                now,
                id
            ],
        )?;
        id
    } else {
        let id = new_prefixed_id("prj_");
        tx.execute(
            "INSERT INTO projects(
                id, identity_kind, identity_value, root_hint, remote_fingerprint,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                kind,
                loc.identity_value,
                loc.worktree_root
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                loc.remote_fingerprint,
                now,
                now
            ],
        )?;
        id
    };
    load_project(tx, &id)?.ok_or_else(|| anyhow!("proyecto recién escrito no se pudo leer"))
}

fn load_project(tx: &Transaction<'_>, id: &str) -> Result<Option<ResolvedProject>> {
    let row = tx
        .query_row(
            "SELECT id, identity_kind, identity_value, remote_fingerprint, linked_project_id
             FROM projects WHERE id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((id, kind, value, remote, linked)) = row else {
        return Ok(None);
    };
    let identity_kind = match kind.as_str() {
        "git_common_dir" => IdentityKind::GitCommonDir,
        _ => IdentityKind::WorkspaceRoot,
    };
    let canonical_id = follow_links(tx, linked.as_deref().unwrap_or(&id), &id)?;
    Ok(Some(ResolvedProject {
        id,
        canonical_id,
        identity_kind,
        identity_value: value,
        remote_fingerprint: remote,
    }))
}

fn follow_links(tx: &Transaction<'_>, start: &str, fallback: &str) -> Result<String> {
    let mut current = start.to_owned();
    let mut seen = std::collections::HashSet::new();
    seen.insert(fallback.to_owned());
    loop {
        if !seen.insert(current.clone()) {
            return Ok(current);
        }
        let linked: Option<Option<String>> = tx
            .query_row(
                "SELECT linked_project_id FROM projects WHERE id = ?1",
                params![current],
                |row| row.get(0),
            )
            .optional()?;
        match linked {
            Some(Some(next)) if !next.is_empty() => current = next,
            _ => return Ok(current),
        }
    }
}

fn upsert_task(
    tx: &Transaction<'_>,
    project_id: &str,
    loc: &super::model::ResolvedLocation,
    orchestrator_task_id: Option<Uuid>,
) -> Result<String> {
    // Explicit task IDs take precedence over the cwd fallback. Existing v1
    // bindings retain their row and history; ambiguous legacy duplicates use
    // the earliest binding deterministically rather than changing with cwd.
    if let Some(orch) = orchestrator_task_id {
        let by_orch: Option<String> = tx
            .query_row(
                "SELECT id FROM tasks WHERE project_id = ?1 AND orchestrator_task_id = ?2
                 ORDER BY created_at, id LIMIT 1",
                params![project_id, orch.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = by_orch {
            return Ok(id);
        }
    }
    let identity = orchestrator_task_id
        .map(|id| format!("orchestrator:{id}"))
        .unwrap_or_else(|| loc.task_identity());
    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM tasks WHERE project_id = ?1 AND identity_value = ?2",
            params![project_id, identity],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = new_prefixed_id("tsk_");
    tx.execute(
        "INSERT INTO tasks(id, project_id, identity_value, orchestrator_task_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            id,
            project_id,
            identity,
            orchestrator_task_id.map(|id| id.to_string()),
            now_secs()
        ],
    )?;
    Ok(id)
}

fn persist_memory(tx: &Transaction<'_>, memory: &MemoryRecord) -> Result<()> {
    tx.execute(
        "INSERT INTO memories(
            id, scope_kind, scope_id, kind, stable_key, status, trust_class, content,
            current_revision, valid_until, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(id) DO UPDATE SET
            status = excluded.status,
            trust_class = excluded.trust_class,
            content = excluded.content,
            current_revision = excluded.current_revision,
            valid_until = excluded.valid_until,
            updated_at = excluded.updated_at",
        params![
            memory.id,
            memory.scope_kind.as_str(),
            memory.scope_id,
            memory.kind.as_str(),
            memory.stable_key,
            memory.status.as_str(),
            memory.trust_class.as_str(),
            memory.content,
            memory.current_revision,
            memory.valid_until,
            memory.created_at,
            memory.updated_at
        ],
    )?;
    Ok(())
}

fn append_revision(
    tx: &Transaction<'_>,
    memory: &MemoryRecord,
    operation: &str,
    actor: &Actor,
) -> Result<()> {
    tx.execute(
        "INSERT INTO memory_revisions(
            id, memory_id, revision, operation, content, actor_kind, actor_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            new_prefixed_id("rev_"),
            memory.id,
            memory.current_revision,
            operation,
            memory.content,
            actor.kind(),
            actor.id(),
            now_secs()
        ],
    )?;
    Ok(())
}

fn upsert_fts(tx: &Transaction<'_>, memory: &MemoryRecord) -> Result<()> {
    delete_fts(tx, &memory.id)?;
    tx.execute(
        "INSERT INTO memories_fts(memory_id, content, stable_key) VALUES (?1, ?2, ?3)",
        params![memory.id, memory.content, memory.stable_key],
    )?;
    Ok(())
}

fn delete_fts(tx: &Transaction<'_>, memory_id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM memories_fts WHERE memory_id = ?1",
        params![memory_id],
    )?;
    Ok(())
}

fn load_memory(tx: &Transaction<'_>, id: &str) -> Result<Option<MemoryRecord>> {
    tx.query_row(
        "SELECT id, scope_kind, scope_id, kind, stable_key, status, trust_class, content,
                current_revision, valid_until, created_at, updated_at
         FROM memories WHERE id = ?1",
        params![id],
        row_to_memory,
    )
    .optional()
    .map_err(Into::into)
}

fn find_active_key(
    tx: &Transaction<'_>,
    scope: ScopeKind,
    scope_id: &str,
    key: &str,
) -> Result<Option<MemoryRecord>> {
    tx.query_row(
        "SELECT id, scope_kind, scope_id, kind, stable_key, status, trust_class, content,
                current_revision, valid_until, created_at, updated_at
         FROM memories
         WHERE scope_kind = ?1 AND scope_id = ?2 AND stable_key = ?3 AND status = 'active'",
        params![scope.as_str(), scope_id, key],
        row_to_memory,
    )
    .optional()
    .map_err(Into::into)
}

fn find_latest_key(
    tx: &Transaction<'_>,
    scope: ScopeKind,
    scope_id: &str,
    key: &str,
) -> Result<Option<MemoryRecord>> {
    tx.query_row(
        "SELECT id, scope_kind, scope_id, kind, stable_key, status, trust_class, content,
                current_revision, valid_until, created_at, updated_at
         FROM memories
         WHERE scope_kind = ?1 AND scope_id = ?2 AND stable_key = ?3
           AND status IN ('active', 'candidate')
         ORDER BY CASE status WHEN 'active' THEN 0 ELSE 1 END, updated_at DESC
         LIMIT 1",
        params![scope.as_str(), scope_id, key],
        row_to_memory,
    )
    .optional()
    .map_err(Into::into)
}

fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecord> {
    Ok(MemoryRecord {
        id: row.get(0)?,
        scope_kind: ScopeKind::parse(&row.get::<_, String>(1)?).unwrap_or(ScopeKind::Project),
        scope_id: row.get(2)?,
        kind: MemoryKind::parse(&row.get::<_, String>(3)?).unwrap_or(MemoryKind::Fact),
        stable_key: row.get(4)?,
        status: MemoryStatus::parse(&row.get::<_, String>(5)?).unwrap_or(MemoryStatus::Candidate),
        trust_class: TrustClass::parse(&row.get::<_, String>(6)?).unwrap_or(TrustClass::Agent),
        content: row.get(7)?,
        current_revision: row.get(8)?,
        valid_until: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

struct VisibleScope {
    kind: ScopeKind,
    id: String,
}

fn visible_scopes(
    tx: &Transaction<'_>,
    loc: &super::model::ResolvedLocation,
    orchestrator_task_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
) -> Result<Vec<VisibleScope>> {
    let project = upsert_project(tx, loc)?;
    let task = upsert_task(tx, &project.canonical_id, loc, orchestrator_task_id)?;
    Ok(scopes_for(&project, &task, loc.cwd.as_path(), workspace_id))
}

fn scopes_for(
    project: &ResolvedProject,
    task_id: &str,
    cwd: &Path,
    workspace_id: Option<Uuid>,
) -> Vec<VisibleScope> {
    let mut scopes = vec![
        VisibleScope {
            kind: ScopeKind::Project,
            id: project.canonical_id.clone(),
        },
        VisibleScope {
            kind: ScopeKind::Task,
            id: task_id.to_owned(),
        },
        VisibleScope {
            kind: ScopeKind::User,
            id: "user".to_owned(),
        },
    ];
    // La ruta canónica es el fallback interoperable para CLI/MCP. Cuando la
    // app conoce un workspace UUID agrega además ese scope, sin volver
    // invisibles las entradas creadas por clientes neutrales.
    scopes.push(VisibleScope {
        kind: ScopeKind::Workspace,
        id: cwd.to_string_lossy().into_owned(),
    });
    if let Some(workspace) = workspace_id {
        scopes.push(VisibleScope {
            kind: ScopeKind::Workspace,
            id: workspace.to_string(),
        });
    }
    scopes
}

fn list_by_scopes(
    tx: &Transaction<'_>,
    scopes: &[VisibleScope],
    status: MemoryStatus,
) -> Result<Vec<MemoryRecord>> {
    list_by_scopes_with_limit(tx, scopes, status, None)
}

fn list_by_scopes_with_limit(
    tx: &Transaction<'_>,
    scopes: &[VisibleScope],
    status: MemoryStatus,
    per_scope_limit: Option<usize>,
) -> Result<Vec<MemoryRecord>> {
    let mut out = Vec::new();
    for scope in scopes {
        let mut stmt = tx.prepare(
            "SELECT id, scope_kind, scope_id, kind, stable_key, status, trust_class, content,
                    current_revision, valid_until, created_at, updated_at
             FROM memories
             WHERE scope_kind = ?1 AND scope_id = ?2 AND status = ?3
               AND (valid_until IS NULL OR valid_until > ?4)
             ORDER BY updated_at DESC, rowid DESC LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![
                scope.kind.as_str(),
                scope.id,
                status.as_str(),
                now_secs(),
                per_scope_limit.map(|limit| limit as i64).unwrap_or(-1)
            ],
            row_to_memory,
        )?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

fn search_visible(
    tx: &Transaction<'_>,
    scopes: &[VisibleScope],
    query: &str,
) -> Result<Vec<MemoryRecord>> {
    let mut out = Vec::new();
    let trimmed = query.trim();
    if trimmed.chars().count() > MAX_SEARCH_QUERY_CHARS {
        anyhow::bail!("la consulta supera el límite de {MAX_SEARCH_QUERY_CHARS} caracteres");
    }
    let mut response_bytes = 4; // Array brackets and newlines.
    let mut scopes: Vec<_> = scopes.iter().collect();
    scopes.sort_by_key(|scope| match scope.kind {
        ScopeKind::Task => 0,
        ScopeKind::Project => 1,
        ScopeKind::Workspace => 2,
        ScopeKind::User => 3,
    });
    for scope in &scopes {
        let mut stmt = tx.prepare(
            "SELECT id, scope_kind, scope_id, kind, stable_key, status, trust_class, content,
                    current_revision, valid_until, created_at, updated_at
             FROM memories
             WHERE scope_kind = ?1 AND scope_id = ?2 AND status = 'active'
               AND (valid_until IS NULL OR valid_until > ?4)
               AND (?3 = '' OR stable_key = ?3 OR instr(lower(content), lower(?3)) > 0
                    OR instr(lower(stable_key), lower(?3)) > 0)
             ORDER BY CASE WHEN stable_key = ?3 THEN 0 ELSE 1 END,
                      updated_at DESC, rowid DESC LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![
                scope.kind.as_str(),
                scope.id,
                trimmed,
                now_secs(),
                MAX_SEARCH_RESULTS as i64
            ],
            row_to_memory,
        )?;
        for row in rows {
            append_search_result(&mut out, &mut response_bytes, row?)?;
            if out.len() == MAX_SEARCH_RESULTS {
                return Ok(out);
            }
        }
    }
    if let Ok(fts_query) = fts_query(trimmed) {
        for scope in &scopes {
            let mut stmt = tx.prepare(
            "SELECT m.id, m.scope_kind, m.scope_id, m.kind, m.stable_key, m.status, m.trust_class,
                    m.content, m.current_revision, m.valid_until, m.created_at, m.updated_at
             FROM memories m
             JOIN memories_fts f ON f.memory_id = m.id
             WHERE memories_fts MATCH ?1 AND m.status = 'active'
               AND (m.valid_until IS NULL OR m.valid_until > ?2)
               AND m.scope_kind = ?3 AND m.scope_id = ?4
             ORDER BY bm25(memories_fts), m.updated_at DESC, m.rowid DESC LIMIT ?5",
        )?;
            let rows = stmt.query_map(
                params![
                    fts_query,
                    now_secs(),
                    scope.kind.as_str(),
                    scope.id,
                    MAX_SEARCH_RESULTS as i64
                ],
                row_to_memory,
            )?;
            for row in rows {
                append_search_result(&mut out, &mut response_bytes, row?)?;
                if out.len() == MAX_SEARCH_RESULTS {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

fn append_search_result(
    out: &mut Vec<MemoryRecord>,
    response_bytes: &mut usize,
    memory: MemoryRecord,
) -> Result<()> {
    if out.iter().any(|existing| existing.id == memory.id) {
        return Ok(());
    }
    let json = serde_json::to_string_pretty(&memory)?;
    // Pretty-printed records gain two spaces per line inside their array.
    let cost = json.len() + json.lines().count() * 2 + 2;
    if *response_bytes + cost <= MAX_SEARCH_RESPONSE_BYTES {
        *response_bytes += cost;
        out.push(memory);
    }
    Ok(())
}

fn fts_query(raw: &str) -> Result<String> {
    let terms: Vec<String> = raw
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '/'))
        .filter(|term| !term.is_empty())
        .take(MAX_FTS_TERMS)
        .map(|term| format!("\"{}\"", term.replace('"', "")))
        .collect();
    if terms.is_empty() {
        anyhow::bail!("query vacía");
    }
    Ok(terms.join(" OR "))
}

fn load_latest_handoff(tx: &Transaction<'_>, task_id: &str) -> Result<Option<HandoffRecord>> {
    let now = now_secs();
    tx.query_row(
        "SELECT id, project_id, task_id, summary, provider, session_id, expires_at, created_at
         FROM handoffs
         WHERE task_id = ?1 AND (expires_at IS NULL OR expires_at > ?2)
         ORDER BY created_at DESC, rowid DESC
         LIMIT 1",
        params![task_id, now],
        |row| {
            Ok(HandoffRecord {
                id: row.get(0)?,
                project_id: row.get(1)?,
                task_id: row.get(2)?,
                summary: row.get(3)?,
                provider: row.get(4)?,
                session_id: row.get(5)?,
                expires_at: row.get(6)?,
                created_at: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn load_receipt(
    tx: &Transaction<'_>,
    client_id: &str,
    request_id: &str,
    expected_fingerprint: &str,
) -> Result<Option<WriteResult>> {
    let json: Option<String> = tx
        .query_row(
            "SELECT response_json FROM mutation_receipts
             WHERE client_id = ?1 AND request_id = ?2",
            params![client_id, request_id],
            |row| row.get(0),
        )
        .optional()?;
    match json {
        Some(json) => {
            let receipt: StoredReceipt = serde_json::from_str(&json)
                .context("recibo de idempotencia sin fingerprint verificable")?;
            if receipt.request_fingerprint != expected_fingerprint {
                anyhow::bail!(
                    "request_id reutilizado con un payload diferente; no se ejecutó la mutación"
                );
            }
            Ok(Some(receipt.result))
        }
        None => Ok(None),
    }
}

fn save_receipt(
    tx: &Transaction<'_>,
    client_id: String,
    request_id: String,
    request_fingerprint: String,
    result: &WriteResult,
) -> Result<()> {
    let json = serde_json::to_string(&StoredReceipt {
        request_fingerprint,
        result: result.clone(),
    })?;
    tx.execute(
        "INSERT OR IGNORE INTO mutation_receipts(client_id, request_id, response_json, committed_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![client_id, request_id, json, now_secs()],
    )?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredReceipt {
    request_fingerprint: String,
    result: WriteResult,
}

fn fingerprint_request(request: &RememberRequest) -> Result<String> {
    let encoded = serde_json::to_vec(request)?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn format_memory_md(memory: &MemoryRecord) -> String {
    format!(
        "### `{}`\n\n- id: `{}`\n- scope: {}\n- kind: {}\n- status: {}\n- rev: {}\n\n{}\n\n",
        memory.stable_key,
        memory.id,
        memory.scope_kind.as_str(),
        memory.kind.as_str(),
        memory.status.as_str(),
        memory.current_revision,
        memory.content
    )
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn is_expired(valid_until: Option<i64>) -> bool {
    valid_until.is_some_and(|deadline| deadline <= now_secs())
}

fn require_human(actor: &Actor, operation: &str) -> Result<()> {
    if !actor.is_human() {
        anyhow::bail!("sólo una acción humana puede {operation} memoria");
    }
    Ok(())
}

fn validate_client_identifier(label: &str, value: &str) -> Result<()> {
    let len = value.chars().count();
    if value.trim().is_empty() || len > MAX_CLIENT_ID_CHARS || value.chars().any(char::is_control) {
        anyhow::bail!("{label} debe tener entre 1 y {MAX_CLIENT_ID_CHARS} caracteres");
    }
    Ok(())
}

fn prepare_private_db_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("crear {}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("proteger {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn protect_private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("proteger {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
