//! Tipos públicos del Memory Hub. Viven acá para que store, pack, CLI y UI
//! hablen el mismo idioma.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    GitCommonDir,
    WorkspaceRoot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    User,
    Project,
    Workspace,
    Task,
}

impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Workspace => "workspace",
            Self::Task => "task",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "user" => Some(Self::User),
            "project" => Some(Self::Project),
            "workspace" => Some(Self::Workspace),
            "task" => Some(Self::Task),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Preference,
    Constraint,
    Decision,
    Fact,
    Procedure,
    Handoff,
    Episode,
    Instruction,
}

impl MemoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::Constraint => "constraint",
            Self::Decision => "decision",
            Self::Fact => "fact",
            Self::Procedure => "procedure",
            Self::Handoff => "handoff",
            Self::Episode => "episode",
            Self::Instruction => "instruction",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "preference" => Some(Self::Preference),
            "constraint" => Some(Self::Constraint),
            "decision" => Some(Self::Decision),
            "fact" => Some(Self::Fact),
            "procedure" => Some(Self::Procedure),
            "handoff" => Some(Self::Handoff),
            "episode" => Some(Self::Episode),
            "instruction" => Some(Self::Instruction),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Candidate,
    Active,
    Rejected,
    Superseded,
    Tombstoned,
}

impl MemoryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Active => "active",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
            Self::Tombstoned => "tombstoned",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "candidate" => Some(Self::Candidate),
            "active" => Some(Self::Active),
            "rejected" => Some(Self::Rejected),
            "superseded" => Some(Self::Superseded),
            "tombstoned" => Some(Self::Tombstoned),
            _ => None,
        }
    }

    pub fn is_retrievable(self) -> bool {
        matches!(self, Self::Active)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    Owner,
    Agent,
    Tool,
    External,
    Recalled,
    System,
}

impl TrustClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Agent => "agent",
            Self::Tool => "tool",
            Self::External => "external",
            Self::Recalled => "recalled",
            Self::System => "system",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "owner" => Some(Self::Owner),
            "agent" => Some(Self::Agent),
            "tool" => Some(Self::Tool),
            "external" => Some(Self::External),
            "recalled" => Some(Self::Recalled),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Actor {
    Human {
        id: String,
    },
    Agent {
        provider: String,
        session_id: Option<String>,
    },
}

impl Actor {
    pub fn human() -> Self {
        Self::Human {
            id: "local-user".to_owned(),
        }
    }

    pub fn agent(provider: impl Into<String>) -> Self {
        Self::Agent {
            provider: provider.into(),
            session_id: None,
        }
    }

    pub fn is_human(&self) -> bool {
        matches!(self, Self::Human { .. })
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Human { .. } => "human",
            Self::Agent { .. } => "agent",
        }
    }

    pub fn id(&self) -> String {
        match self {
            Self::Human { id } => id.clone(),
            Self::Agent {
                provider,
                session_id,
            } => session_id
                .as_deref()
                .map(|session| format!("{provider}:{session}"))
                .unwrap_or_else(|| provider.clone()),
        }
    }

    pub fn trust(&self) -> TrustClass {
        if self.is_human() {
            TrustClass::Owner
        } else {
            TrustClass::Agent
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLocation {
    pub cwd: PathBuf,
    pub identity_kind: IdentityKind,
    pub identity_value: String,
    pub worktree_root: Option<PathBuf>,
    pub remote_fingerprint: Option<String>,
}

impl ResolvedLocation {
    pub fn task_identity(&self) -> String {
        self.worktree_root
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.cwd.to_string_lossy().into_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedProject {
    pub id: String,
    pub canonical_id: String,
    pub identity_kind: IdentityKind,
    pub identity_value: String,
    pub remote_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub scope_kind: ScopeKind,
    pub scope_id: String,
    pub kind: MemoryKind,
    pub stable_key: String,
    pub status: MemoryStatus,
    pub trust_class: TrustClass,
    pub content: String,
    pub current_revision: u32,
    pub valid_until: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffRecord {
    pub id: String,
    pub project_id: String,
    pub task_id: String,
    pub summary: String,
    pub provider: Option<String>,
    pub session_id: Option<String>,
    pub expires_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextItem {
    pub memory_id: String,
    pub kind: MemoryKind,
    pub scope: ScopeKind,
    pub key: String,
    pub content: String,
    pub citation: String,
    pub source: String,
    pub reason: String,
    pub trust: TrustClass,
    pub revision: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPack {
    pub protocol_version: u32,
    pub project_id: String,
    pub task_id: String,
    pub revision_cursor: u32,
    pub budget_requested: usize,
    pub budget_used: usize,
    pub items: Vec<ContextItem>,
    pub handoff: Option<HandoffRecord>,
}

impl ContextPack {
    pub fn empty(project_id: impl Into<String>, task_id: impl Into<String>) -> Self {
        Self {
            protocol_version: 1,
            project_id: project_id.into(),
            task_id: task_id.into(),
            revision_cursor: 0,
            budget_requested: DEFAULT_TOKEN_BUDGET,
            budget_used: 0,
            items: Vec::new(),
            handoff: None,
        }
    }
}

pub const DEFAULT_TOKEN_BUDGET: usize = 2400;
pub const CORE_TOKEN_BUDGET: usize = 800;
pub const HANDOFF_TOKEN_BUDGET: usize = 400;
pub const MAX_CONTEXT_ITEMS: usize = 32;
pub const MAX_MEMORY_KEY_CHARS: usize = 256;
pub const MAX_MEMORY_CONTENT_CHARS: usize = 65_536;
pub const MAX_HANDOFF_CHARS: usize = 32_768;
pub const MAX_CLIENT_ID_CHARS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberRequest {
    pub client_id: String,
    pub request_id: String,
    pub cwd: PathBuf,
    pub scope: ScopeKind,
    pub kind: MemoryKind,
    pub key: String,
    pub content: String,
    pub actor: Actor,
    pub expected_revision: Option<u32>,
    pub orchestrator_task_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
}

impl RememberRequest {
    pub fn human(cwd: PathBuf, key: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            client_id: "test".to_owned(),
            request_id: Uuid::new_v4().to_string(),
            cwd,
            scope: ScopeKind::Project,
            kind: MemoryKind::Decision,
            key: key.into(),
            content: content.into(),
            actor: Actor::human(),
            expected_revision: None,
            orchestrator_task_id: None,
            workspace_id: None,
        }
    }

    pub fn agent(cwd: PathBuf, key: impl Into<String>, content: impl Into<String>) -> Self {
        let mut request = Self::human(cwd, key, content);
        request.actor = Actor::agent("test-agent");
        request
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedWrite {
    pub memory: MemoryRecord,
    pub receipt_id: String,
    pub committed_revision: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictWrite {
    pub current: MemoryRecord,
    pub incoming_content: String,
    pub candidate_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum WriteResult {
    Committed(CommittedWrite),
    Conflict(ConflictWrite),
    Deduped(CommittedWrite),
}

impl WriteResult {
    pub fn memory(&self) -> &MemoryRecord {
        match self {
            Self::Committed(write) | Self::Deduped(write) => &write.memory,
            Self::Conflict(write) => &write.current,
        }
    }

    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffRequest {
    pub cwd: PathBuf,
    pub summary: String,
    pub provider: Option<String>,
    pub session_id: Option<String>,
    pub orchestrator_task_id: Option<Uuid>,
    pub ttl_secs: Option<i64>,
}

pub fn contents_equivalent(left: &str, right: &str) -> bool {
    // These values have already passed content preparation. Case, spaces,
    // and line boundaries may be meaningful code or configuration.
    left == right
}

pub fn normalize_key(key: &str) -> String {
    key.trim()
        .split(|ch: char| ch.is_whitespace() || ch == '\\')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

pub fn new_prefixed_id(prefix: &str) -> String {
    format!("{prefix}{}", Uuid::new_v4().as_simple())
}
