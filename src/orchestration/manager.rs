use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum AgentProvider {
    ClaudeCode,
    CodexCli,
    GeminiCli,
    Aider,
    OpenCode,
    #[default]
    Unknown,
}

impl AgentProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::CodexCli => "Codex",
            Self::GeminiCli => "Gemini",
            Self::Aider => "Aider",
            Self::OpenCode => "OpenCode",
            Self::Unknown => "Terminal",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::CodexCli => "codex",
            Self::GeminiCli => "gemini",
            Self::Aider => "aider",
            Self::OpenCode => "opencode",
            Self::Unknown => "terminal",
        }
    }

    pub fn launch_command(self) -> Option<&'static str> {
        match self {
            Self::ClaudeCode => Some("claude"),
            Self::CodexCli => Some("codex"),
            Self::GeminiCli => Some("gemini"),
            Self::Aider => Some("aider"),
            Self::OpenCode => Some("opencode"),
            Self::Unknown => None,
        }
    }

    pub fn detect(text: &str) -> Option<Self> {
        let normalized = text.trim().to_ascii_lowercase();
        [
            ("openclaude", Self::OpenCode),
            ("opencode", Self::OpenCode),
            ("claude code", Self::ClaudeCode),
            ("claude-code", Self::ClaudeCode),
            ("claude", Self::ClaudeCode),
            ("codex", Self::CodexCli),
            ("gemini", Self::GeminiCli),
            ("aider", Self::Aider),
        ]
        .into_iter()
        .find_map(|(needle, provider)| normalized.contains(needle).then_some(provider))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AgentStatus {
    #[default]
    Idle,
    Running,
    WaitingApproval,
    NeedsInput,
    Reviewing,
    Failed,
    Done,
}

impl AgentStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Running => "Running",
            Self::WaitingApproval => "Waiting",
            Self::NeedsInput => "Input",
            Self::Reviewing => "Review",
            Self::Failed => "Failed",
            Self::Done => "Done",
        }
    }

    pub fn is_attention(self) -> bool {
        matches!(
            self,
            Self::WaitingApproval | Self::NeedsInput | Self::Failed
        )
    }

    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Running | Self::WaitingApproval | Self::NeedsInput | Self::Reviewing
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TaskState {
    #[default]
    Draft,
    Queued,
    Running,
    Blocked,
    ReviewReady,
    Done,
    Cancelled,
}

impl TaskState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Blocked => "Blocked",
            Self::ReviewReady => "Review",
            Self::Done => "Done",
            Self::Cancelled => "Cancelled",
        }
    }

    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Running | Self::Blocked | Self::ReviewReady
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DependencyKind {
    BlockedBy,
    DependsOn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub from_task: Uuid,
    pub to_task: Uuid,
    pub kind: DependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DiffStats {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestStatus {
    pub label: String,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReviewSummary {
    #[serde(default)]
    pub changed_files: Vec<PathBuf>,
    #[serde(default)]
    pub diff_stats: DiffStats,
    #[serde(default)]
    pub tests: Vec<TestStatus>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_success: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSummary {
    pub title: String,
    pub excerpt: String,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCard {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: String,
    #[serde(default)]
    pub brief: String,
    #[serde(default)]
    pub state: TaskState,
    #[serde(default)]
    pub provider_hint: Option<AgentProvider>,
    #[serde(default)]
    pub session_ids: Vec<Uuid>,
    #[serde(default)]
    pub conflict_risk: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionMeta {
    pub session_id: Uuid,
    #[serde(default)]
    pub panel_id: Option<Uuid>,
    #[serde(default)]
    pub runtime_session_id: Option<Uuid>,
    pub workspace_id: Uuid,
    #[serde(default)]
    pub provider: AgentProvider,
    pub label: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub worktree_path: Option<PathBuf>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub task_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    #[serde(default)]
    pub status: AgentStatus,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default)]
    pub repo_root: Option<PathBuf>,
    #[serde(default)]
    pub shared_repo_mode: bool,
    #[serde(default)]
    pub startup_command: Option<String>,
    #[serde(default)]
    pub command_summary: Option<CommandSummary>,
    #[serde(default)]
    pub review_summary: ReviewSummary,
    #[serde(default)]
    pub conflict_risk: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum InboxEventKind {
    ApprovalPending,
    NeedsInput,
    TestsFailed,
    ProcessDone,
    ConflictRisk,
    ReviewReady,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEvent {
    pub id: Uuid,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub task_id: Option<Uuid>,
    pub kind: InboxEventKind,
    pub title: String,
    pub summary: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneTemplateKind {
    Bugfix,
    FeatureParallel,
    RefactorReview,
    FrontendBackendSplit,
}

impl SceneTemplateKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Bugfix => "Bugfix",
            Self::FeatureParallel => "Feature parallel",
            Self::RefactorReview => "Refactor + review",
            Self::FrontendBackendSplit => "Frontend / backend split",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneTemplate {
    pub kind: SceneTemplateKind,
    pub name: String,
    #[serde(default)]
    pub suggested_agents: Vec<AgentProvider>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OrchestrationState {
    #[serde(default)]
    pub tasks: Vec<TaskCard>,
    #[serde(default)]
    pub sessions: Vec<AgentSessionMeta>,
    #[serde(default)]
    pub inbox: Vec<InboxEvent>,
    #[serde(default)]
    pub dependencies: Vec<DependencyEdge>,
    #[serde(default)]
    pub scene_template: Option<SceneTemplateKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorktreeMode {
    #[default]
    Auto,
    SharedRepo,
}

#[derive(Debug, Clone)]
pub struct AgentLaunchRequest {
    pub workspace_id: Uuid,
    pub task_id: Option<Uuid>,
    pub base_cwd: Option<PathBuf>,
    pub provider: AgentProvider,
    pub task_title: String,
    pub brief: String,
    pub worktree_mode: WorktreeMode,
}

#[derive(Debug, Clone)]
pub struct AgentLaunchPlan {
    pub task_id: Option<Uuid>,
    pub session_id: Uuid,
    pub panel_title: String,
    pub cwd: Option<PathBuf>,
    pub startup_command: Option<String>,
    pub startup_input: Option<String>,
    pub branch: Option<String>,
    pub worktree_path: Option<PathBuf>,
    pub shared_repo_mode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderBootstrap {
    command: Option<String>,
    initial_input: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PanelRuntimeObservation {
    pub panel_id: Uuid,
    pub runtime_session_id: Option<Uuid>,
    pub workspace_id: Uuid,
    pub title: String,
    pub visible_text: String,
    pub alive: bool,
    pub recent_output: bool,
}

#[derive(Debug, Clone)]
pub struct SessionListItem {
    pub workspace_id: Uuid,
    pub panel_id: Option<Uuid>,
    pub session_id: Uuid,
    pub title: String,
    pub provider: AgentProvider,
    pub task_title: Option<String>,
    pub branch: Option<String>,
    pub status: AgentStatus,
    pub dirty: bool,
    pub conflict_risk: bool,
    pub cwd: Option<PathBuf>,
    pub worktree_path: Option<PathBuf>,
    pub command_excerpt: Option<String>,
    pub last_error: Option<String>,
    pub last_success: Option<String>,
    pub changed_files: usize,
}

#[derive(Debug, Clone)]
pub struct PanelOverlay {
    pub provider: AgentProvider,
    pub task_title: Option<String>,
    pub status: AgentStatus,
    pub branch: Option<String>,
    pub dirty: bool,
    pub shared_repo_mode: bool,
    pub conflict_risk: bool,
    pub preview_label: String,
}

#[derive(Debug, Default)]
pub struct Orchestrator {
    state: OrchestrationState,
}

impl Orchestrator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_saved(state: Option<OrchestrationState>) -> Self {
        Self {
            state: state.unwrap_or_default(),
        }
    }

    pub fn snapshot(&self) -> OrchestrationState {
        self.state.clone()
    }

    pub fn tasks(&self) -> &[TaskCard] {
        &self.state.tasks
    }

    pub fn sessions(&self) -> &[AgentSessionMeta] {
        &self.state.sessions
    }

    pub fn inbox(&self) -> &[InboxEvent] {
        &self.state.inbox
    }

    pub fn scene_template(&self) -> Option<SceneTemplateKind> {
        self.state.scene_template
    }

    pub fn task_snapshot(&self, task_id: Uuid) -> Option<TaskCard> {
        self.state
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .cloned()
    }

    pub fn session_panel_id(&self, session_id: Uuid) -> Option<Uuid> {
        self.state
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)?
            .panel_id
    }

    pub fn session_meta(&self, session_id: Uuid) -> Option<&AgentSessionMeta> {
        self.state
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)
    }

    pub fn set_scene_template(&mut self, template: Option<SceneTemplateKind>) {
        self.state.scene_template = template;
    }

    pub fn apply_scene_template(&mut self, workspace_id: Uuid, template: SceneTemplateKind) {
        self.state.scene_template = Some(template);
        for (title, provider) in scene_template_defaults(template) {
            let task_id = Uuid::new_v4();
            let now = Utc::now();
            self.state.tasks.push(TaskCard {
                id: task_id,
                workspace_id,
                title: title.to_owned(),
                brief: String::new(),
                state: TaskState::Draft,
                provider_hint: Some(provider),
                session_ids: Vec::new(),
                conflict_risk: false,
                created_at: now,
                updated_at: now,
            });
        }
        self.sync_task_states_from_dependencies();
    }

    pub fn ensure_panel_session(
        &mut self,
        workspace_id: Uuid,
        cwd: Option<PathBuf>,
        panel_id: Uuid,
        runtime_session_id: Option<Uuid>,
        title: &str,
    ) -> Uuid {
        if let Some(existing) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.panel_id == Some(panel_id))
        {
            existing.runtime_session_id = runtime_session_id;
            existing.workspace_id = workspace_id;
            existing.label = title.to_owned();
            if existing.cwd.is_none() {
                existing.cwd = cwd;
            }
            return existing.session_id;
        }

        let now = Utc::now();
        let provider = AgentProvider::detect(title).unwrap_or_default();
        let session_id = Uuid::new_v4();
        self.state.sessions.push(AgentSessionMeta {
            session_id,
            panel_id: Some(panel_id),
            runtime_session_id,
            workspace_id,
            provider,
            label: title.to_owned(),
            cwd,
            worktree_path: None,
            branch: None,
            task_id: None,
            created_at: now,
            last_activity_at: now,
            status: AgentStatus::Idle,
            dirty: false,
            repo_root: None,
            shared_repo_mode: true,
            startup_command: None,
            command_summary: None,
            review_summary: ReviewSummary::default(),
            conflict_risk: false,
        });
        session_id
    }

    pub fn prune_missing_panels(&mut self, live_panel_ids: &HashSet<Uuid>) {
        let removed_sessions = self
            .state
            .sessions
            .iter()
            .filter_map(|session| {
                let panel_id = session.panel_id?;
                (!live_panel_ids.contains(&panel_id)).then_some(session.session_id)
            })
            .collect::<HashSet<_>>();

        self.state.sessions.retain(|session| {
            session
                .panel_id
                .map(|panel_id| live_panel_ids.contains(&panel_id))
                .unwrap_or(true)
        });
        for task in &mut self.state.tasks {
            task.session_ids
                .retain(|session_id| !removed_sessions.contains(session_id));
        }
        self.state.inbox.retain(|event| {
            event
                .session_id
                .map(|id| !removed_sessions.contains(&id))
                .unwrap_or(true)
        });
    }

    pub fn prepare_launch(
        &mut self,
        request: AgentLaunchRequest,
    ) -> anyhow::Result<AgentLaunchPlan> {
        let task_title = if request.task_title.trim().is_empty() {
            request.provider.label().to_owned()
        } else {
            request.task_title.trim().to_owned()
        };
        let task_id = request.task_id.or_else(|| {
            Some(self.create_task(
                request.workspace_id,
                &task_title,
                &request.brief,
                Some(request.provider),
            ))
        });

        let session_id = Uuid::new_v4();
        let short_id = short_uuid(session_id);
        let slug = slugify(&task_title);
        let base_cwd = request.base_cwd.clone();
        let repo_root = base_cwd
            .as_deref()
            .and_then(git_repo_root)
            .or_else(|| base_cwd.clone());
        let worktree_requested = matches!(request.worktree_mode, WorktreeMode::Auto);

        let (cwd, worktree_path, branch, shared_repo_mode) = if worktree_requested {
            if let Some(root) = repo_root.as_deref().and_then(git_repo_root) {
                let branch = format!("canvas/{}/{}-{}", request.provider.slug(), slug, short_id);
                let worktree_path = root
                    .join(".canvas")
                    .join("worktrees")
                    .join(format!("{slug}-{short_id}"));
                create_git_worktree(&root, &worktree_path, &branch)?;
                (
                    Some(worktree_path.clone()),
                    Some(worktree_path),
                    Some(branch),
                    false,
                )
            } else {
                (base_cwd.clone(), None, None, true)
            }
        } else {
            (base_cwd.clone(), None, None, true)
        };

        let label = if let Some(branch) = &branch {
            format!(
                "{} — {} — {}",
                request.provider.label(),
                task_title,
                truncate_branch(branch)
            )
        } else {
            format!("{} — {}", request.provider.label(), task_title)
        };

        let now = Utc::now();
        let bootstrap = provider_bootstrap(request.provider, &request.brief);

        self.state.sessions.push(AgentSessionMeta {
            session_id,
            panel_id: None,
            runtime_session_id: None,
            workspace_id: request.workspace_id,
            provider: request.provider,
            label: label.clone(),
            cwd: cwd.clone(),
            worktree_path: worktree_path.clone(),
            branch: branch.clone(),
            task_id,
            created_at: now,
            last_activity_at: now,
            status: AgentStatus::Idle,
            dirty: false,
            repo_root,
            shared_repo_mode,
            startup_command: bootstrap.command.clone(),
            command_summary: None,
            review_summary: ReviewSummary::default(),
            conflict_risk: false,
        });

        if let Some(task_id) = task_id {
            if let Some(task) = self.state.tasks.iter_mut().find(|task| task.id == task_id) {
                if !task.session_ids.contains(&session_id) {
                    task.session_ids.push(session_id);
                }
                task.provider_hint = Some(request.provider);
                task.state = TaskState::Queued;
                task.updated_at = now;
            }
        }

        Ok(AgentLaunchPlan {
            task_id,
            session_id,
            panel_title: label,
            cwd,
            startup_command: bootstrap.command,
            startup_input: bootstrap.initial_input,
            branch,
            worktree_path,
            shared_repo_mode,
        })
    }

    pub fn bind_launch_to_panel(
        &mut self,
        session_id: Uuid,
        panel_id: Uuid,
        runtime_session_id: Option<Uuid>,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.session_id == session_id)
        {
            session.panel_id = Some(panel_id);
            session.runtime_session_id = runtime_session_id;
            session.status = AgentStatus::Running;
            session.last_activity_at = Utc::now();
        }
    }

    pub fn mark_task_state(&mut self, task_id: Uuid, state: TaskState) {
        if let Some(task) = self.state.tasks.iter_mut().find(|task| task.id == task_id) {
            task.state = state;
            task.updated_at = Utc::now();
        }
        self.sync_task_states_from_dependencies();
    }

    pub fn dependency_targets(&self, workspace_id: Uuid, task_id: Uuid) -> Vec<TaskCard> {
        let mut tasks = self
            .state
            .tasks
            .iter()
            .filter(|task| task.workspace_id == workspace_id && task.id != task_id)
            .cloned()
            .collect::<Vec<_>>();
        tasks.sort_by(|left, right| left.title.cmp(&right.title));
        tasks
    }

    pub fn task_dependency_summary(&self, task_id: Uuid) -> Vec<String> {
        let mut labels = self
            .state
            .dependencies
            .iter()
            .filter(|edge| edge.from_task == task_id)
            .filter_map(|edge| {
                let target = self
                    .state
                    .tasks
                    .iter()
                    .find(|task| task.id == edge.to_task)?;
                let prefix = match edge.kind {
                    DependencyKind::BlockedBy => "Blocked by",
                    DependencyKind::DependsOn => "Depends on",
                };
                Some(format!("{prefix} {}", truncate_text(&target.title, 18)))
            })
            .collect::<Vec<_>>();
        labels.sort();
        labels
    }

    pub fn first_dependency(&self, task_id: Uuid) -> Option<(DependencyKind, Uuid)> {
        self.state
            .dependencies
            .iter()
            .find(|edge| edge.from_task == task_id)
            .map(|edge| (edge.kind, edge.to_task))
    }

    pub fn set_task_dependency(&mut self, from_task: Uuid, to_task: Uuid, kind: DependencyKind) {
        if from_task == to_task {
            return;
        }
        if !self
            .state
            .dependencies
            .iter()
            .any(|edge| edge.from_task == from_task && edge.to_task == to_task && edge.kind == kind)
        {
            self.state.dependencies.push(DependencyEdge {
                from_task,
                to_task,
                kind,
            });
        }
        self.sync_task_states_from_dependencies();
    }

    pub fn clear_task_dependencies(&mut self, task_id: Uuid) {
        self.state
            .dependencies
            .retain(|edge| edge.from_task != task_id);
        self.sync_task_states_from_dependencies();
    }

    pub fn duplicate_session_request(
        &self,
        session_id: Uuid,
        provider: AgentProvider,
    ) -> Option<AgentLaunchRequest> {
        let session = self
            .state
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)?;
        let task_title = session
            .task_id
            .and_then(|task_id| self.state.tasks.iter().find(|task| task.id == task_id))
            .map(|task| task.title.clone())
            .unwrap_or_else(|| session.label.clone());
        Some(AgentLaunchRequest {
            workspace_id: session.workspace_id,
            task_id: session.task_id,
            base_cwd: session.cwd.clone(),
            provider,
            task_title,
            brief: String::new(),
            worktree_mode: WorktreeMode::Auto,
        })
    }

    pub fn apply_observations(&mut self, observations: Vec<PanelRuntimeObservation>) {
        let now = Utc::now();
        let observations_by_panel = observations
            .into_iter()
            .map(|observation| (observation.panel_id, observation))
            .collect::<HashMap<_, _>>();

        for session in &mut self.state.sessions {
            let Some(panel_id) = session.panel_id else {
                continue;
            };
            let Some(observation) = observations_by_panel.get(&panel_id) else {
                continue;
            };
            session.workspace_id = observation.workspace_id;
            session.runtime_session_id = observation.runtime_session_id;
            session.last_activity_at = now;
            if session.provider == AgentProvider::Unknown {
                session.provider = AgentProvider::detect(&format!(
                    "{}\n{}",
                    observation.title, observation.visible_text
                ))
                .unwrap_or(AgentProvider::Unknown);
            }
            session.command_summary = summarize_command_output(&observation.visible_text);
            session.status = derive_status(
                observation.alive,
                observation.recent_output,
                &observation.visible_text,
                session.review_summary.last_error.as_deref(),
            );
            let git = session.cwd.as_deref().and_then(inspect_git_state);
            if let Some(git) = git {
                session.repo_root = Some(git.repo_root.clone());
                session.branch = Some(git.branch.clone());
                session.dirty = git.dirty;
                session.review_summary.changed_files = git.changed_files;
                session.review_summary.diff_stats = git.diff_stats;
            }
            apply_output_summaries(session, &observation.visible_text);
            if let Some(task_id) = session.task_id {
                if let Some(task) = self.state.tasks.iter_mut().find(|task| task.id == task_id) {
                    task.updated_at = now;
                    task.state = match session.status {
                        AgentStatus::WaitingApproval | AgentStatus::NeedsInput => {
                            TaskState::Blocked
                        }
                        AgentStatus::Reviewing => TaskState::ReviewReady,
                        AgentStatus::Done => TaskState::Done,
                        AgentStatus::Failed => TaskState::Running,
                        AgentStatus::Running => TaskState::Running,
                        AgentStatus::Idle => task.state,
                    };
                }
            }
        }

        self.refresh_inbox();
        self.refresh_conflict_risk();
        self.sync_task_states_from_dependencies();
    }

    pub fn mark_inbox_resolved(&mut self, event_id: Uuid) {
        if let Some(event) = self
            .state
            .inbox
            .iter_mut()
            .find(|event| event.id == event_id)
        {
            event.resolved = true;
        }
    }

    pub fn archive_inbox_event(&mut self, event_id: Uuid) {
        if let Some(event) = self
            .state
            .inbox
            .iter_mut()
            .find(|event| event.id == event_id)
        {
            event.archived = true;
        }
    }

    pub fn panel_overlay(&self, panel_id: Uuid) -> Option<PanelOverlay> {
        let session = self
            .state
            .sessions
            .iter()
            .find(|session| session.panel_id == Some(panel_id))?;
        let task_title = session.task_id.and_then(|task_id| {
            self.state
                .tasks
                .iter()
                .find(|task| task.id == task_id)
                .map(|task| task.title.clone())
        });
        Some(PanelOverlay {
            provider: session.provider,
            task_title: task_title.clone(),
            status: session.status,
            branch: session.branch.clone(),
            dirty: session.dirty,
            shared_repo_mode: session.shared_repo_mode,
            conflict_risk: session.conflict_risk,
            preview_label: preview_label(session.provider, task_title.as_deref(), session.status),
        })
    }

    pub fn session_items(
        &self,
        search_query: &str,
        active_only: bool,
        provider_filter: Option<AgentProvider>,
        status_filter: Option<AgentStatus>,
    ) -> Vec<SessionListItem> {
        let mut items = self
            .state
            .sessions
            .iter()
            .filter(|session| !active_only || session.status.is_active() || session.conflict_risk)
            .filter(|session| {
                provider_filter
                    .map(|provider| session.provider == provider)
                    .unwrap_or(true)
            })
            .filter(|session| {
                status_filter
                    .map(|status| session.status == status)
                    .unwrap_or(true)
            })
            .filter(|session| session_matches_query(self, session, search_query))
            .map(|session| SessionListItem {
                workspace_id: session.workspace_id,
                panel_id: session.panel_id,
                session_id: session.session_id,
                title: session.label.clone(),
                provider: session.provider,
                task_title: session.task_id.and_then(|task_id| {
                    self.state
                        .tasks
                        .iter()
                        .find(|task| task.id == task_id)
                        .map(|task| task.title.clone())
                }),
                branch: session.branch.clone(),
                status: session.status,
                dirty: session.dirty,
                conflict_risk: session.conflict_risk,
                cwd: session.cwd.clone(),
                worktree_path: session.worktree_path.clone(),
                command_excerpt: session
                    .command_summary
                    .as_ref()
                    .map(|summary| summary.excerpt.clone()),
                last_error: session.review_summary.last_error.clone(),
                last_success: session.review_summary.last_success.clone(),
                changed_files: session.review_summary.changed_files.len(),
            })
            .collect::<Vec<_>>();
        items.sort_by(|left, right| {
            right
                .status
                .is_attention()
                .cmp(&left.status.is_attention())
                .then_with(|| left.title.cmp(&right.title))
        });
        items
    }

    pub fn task_for_session(&self, session_id: Uuid) -> Option<&TaskCard> {
        let task_id = self
            .state
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)?
            .task_id?;
        self.state.tasks.iter().find(|task| task.id == task_id)
    }

    pub fn tasks_filtered(
        &self,
        workspace_id: Option<Uuid>,
        search_query: &str,
        provider_filter: Option<AgentProvider>,
        status_filter: Option<AgentStatus>,
    ) -> Vec<&TaskCard> {
        let mut tasks = self
            .state
            .tasks
            .iter()
            .filter(|task| {
                workspace_id
                    .map(|id| task.workspace_id == id)
                    .unwrap_or(true)
            })
            .filter(|task| task_matches_filters(self, task, provider_filter, status_filter))
            .filter(|task| task_matches_query(self, task, search_query))
            .collect::<Vec<_>>();
        tasks.sort_by(|left, right| {
            right
                .state
                .is_active()
                .cmp(&left.state.is_active())
                .then_with(|| left.title.cmp(&right.title))
        });
        tasks
    }

    pub fn inbox_filtered(
        &self,
        search_query: &str,
        active_only: bool,
        provider_filter: Option<AgentProvider>,
        status_filter: Option<AgentStatus>,
    ) -> Vec<&InboxEvent> {
        let mut events = self
            .state
            .inbox
            .iter()
            .filter(|event| !event.archived)
            .filter(|event| !active_only || !event.resolved)
            .filter(|event| event_matches_filters(self, event, provider_filter, status_filter))
            .filter(|event| inbox_matches_query(event, search_query))
            .collect::<Vec<_>>();
        events.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        events
    }

    pub fn create_task(
        &mut self,
        workspace_id: Uuid,
        title: &str,
        brief: &str,
        provider_hint: Option<AgentProvider>,
    ) -> Uuid {
        let now = Utc::now();
        let task_id = Uuid::new_v4();
        self.state.tasks.push(TaskCard {
            id: task_id,
            workspace_id,
            title: title.to_owned(),
            brief: brief.to_owned(),
            state: TaskState::Draft,
            provider_hint,
            session_ids: Vec::new(),
            conflict_risk: false,
            created_at: now,
            updated_at: now,
        });
        task_id
    }

    fn refresh_inbox(&mut self) {
        let mut desired = BTreeMap::new();
        for session in &self.state.sessions {
            match session.status {
                AgentStatus::WaitingApproval => {
                    let key = (InboxEventKind::ApprovalPending, session.session_id);
                    desired.insert(
                        key,
                        InboxEvent {
                            id: deterministic_event_id(key.0, session.session_id),
                            session_id: Some(session.session_id),
                            task_id: session.task_id,
                            kind: InboxEventKind::ApprovalPending,
                            title: format!("{} needs approval", session.label),
                            summary: session
                                .command_summary
                                .as_ref()
                                .map(|summary| summary.excerpt.clone())
                                .unwrap_or_else(|| {
                                    "Pending approval in terminal output".to_owned()
                                }),
                            created_at: Utc::now(),
                            resolved: false,
                            archived: false,
                        },
                    );
                }
                AgentStatus::NeedsInput => {
                    let key = (InboxEventKind::NeedsInput, session.session_id);
                    desired.insert(
                        key,
                        InboxEvent {
                            id: deterministic_event_id(key.0, session.session_id),
                            session_id: Some(session.session_id),
                            task_id: session.task_id,
                            kind: InboxEventKind::NeedsInput,
                            title: format!("{} needs input", session.label),
                            summary: session
                                .command_summary
                                .as_ref()
                                .map(|summary| summary.excerpt.clone())
                                .unwrap_or_else(|| {
                                    "Session appears to be waiting for input".to_owned()
                                }),
                            created_at: Utc::now(),
                            resolved: false,
                            archived: false,
                        },
                    );
                }
                AgentStatus::Failed => {
                    let key = (InboxEventKind::TestsFailed, session.session_id);
                    desired.insert(
                        key,
                        InboxEvent {
                            id: deterministic_event_id(key.0, session.session_id),
                            session_id: Some(session.session_id),
                            task_id: session.task_id,
                            kind: InboxEventKind::TestsFailed,
                            title: format!("{} reported a failure", session.label),
                            summary: session.review_summary.last_error.clone().unwrap_or_else(
                                || "Failure detected in terminal output".to_owned(),
                            ),
                            created_at: Utc::now(),
                            resolved: false,
                            archived: false,
                        },
                    );
                }
                AgentStatus::Done => {
                    let key = (InboxEventKind::ProcessDone, session.session_id);
                    desired.insert(
                        key,
                        InboxEvent {
                            id: deterministic_event_id(key.0, session.session_id),
                            session_id: Some(session.session_id),
                            task_id: session.task_id,
                            kind: InboxEventKind::ProcessDone,
                            title: format!("{} finished", session.label),
                            summary: session
                                .review_summary
                                .last_success
                                .clone()
                                .unwrap_or_else(|| "Process exited or completed".to_owned()),
                            created_at: Utc::now(),
                            resolved: false,
                            archived: false,
                        },
                    );
                }
                AgentStatus::Reviewing => {
                    let key = (InboxEventKind::ReviewReady, session.session_id);
                    desired.insert(
                        key,
                        InboxEvent {
                            id: deterministic_event_id(key.0, session.session_id),
                            session_id: Some(session.session_id),
                            task_id: session.task_id,
                            kind: InboxEventKind::ReviewReady,
                            title: format!("{} is ready for review", session.label),
                            summary: format!(
                                "{} changed file(s)",
                                session.review_summary.changed_files.len()
                            ),
                            created_at: Utc::now(),
                            resolved: false,
                            archived: false,
                        },
                    );
                }
                _ => {}
            }
        }

        let previous_resolution = self
            .state
            .inbox
            .iter()
            .map(|event| (event.id, (event.resolved, event.archived)))
            .collect::<HashMap<_, _>>();

        self.state.inbox = desired
            .into_values()
            .map(|mut event| {
                if let Some((resolved, archived)) = previous_resolution.get(&event.id) {
                    event.resolved = *resolved;
                    event.archived = *archived;
                }
                event
            })
            .collect();
    }

    fn refresh_conflict_risk(&mut self) {
        let mut conflicts = HashSet::new();
        for left_index in 0..self.state.sessions.len() {
            for right_index in (left_index + 1)..self.state.sessions.len() {
                let left = &self.state.sessions[left_index];
                let right = &self.state.sessions[right_index];
                if left.workspace_id != right.workspace_id {
                    continue;
                }
                if left.review_summary.changed_files.is_empty()
                    || right.review_summary.changed_files.is_empty()
                {
                    continue;
                }
                let left_set = left
                    .review_summary
                    .changed_files
                    .iter()
                    .collect::<HashSet<_>>();
                if right
                    .review_summary
                    .changed_files
                    .iter()
                    .any(|path| left_set.contains(path))
                {
                    conflicts.insert(left.session_id);
                    conflicts.insert(right.session_id);
                }
            }
        }

        for session in &mut self.state.sessions {
            session.conflict_risk = conflicts.contains(&session.session_id);
        }
        for task in &mut self.state.tasks {
            task.conflict_risk = task
                .session_ids
                .iter()
                .any(|session_id| conflicts.contains(session_id));
        }

        let mut by_id = self
            .state
            .inbox
            .drain(..)
            .map(|event| (event.id, event))
            .collect::<HashMap<_, _>>();
        for session_id in conflicts {
            let event_id = deterministic_event_id(InboxEventKind::ConflictRisk, session_id);
            let session = self
                .state
                .sessions
                .iter()
                .find(|session| session.session_id == session_id);
            let event = InboxEvent {
                id: event_id,
                session_id: Some(session_id),
                task_id: session.and_then(|session| session.task_id),
                kind: InboxEventKind::ConflictRisk,
                title: session
                    .map(|session| format!("{} has conflict risk", session.label))
                    .unwrap_or_else(|| "Conflict risk".to_owned()),
                summary: "Another active task is touching the same file set".to_owned(),
                created_at: Utc::now(),
                resolved: false,
                archived: false,
            };
            by_id.insert(event_id, event);
        }
        self.state.inbox = by_id.into_values().collect();
    }

    fn sync_task_states_from_dependencies(&mut self) {
        let dependency_states = self
            .state
            .dependencies
            .iter()
            .filter_map(|edge| {
                self.state
                    .tasks
                    .iter()
                    .find(|task| task.id == edge.to_task)
                    .map(|task| (edge.from_task, task.state))
            })
            .collect::<Vec<_>>();

        for task in &mut self.state.tasks {
            if matches!(task.state, TaskState::Done | TaskState::Cancelled) {
                continue;
            }
            let blocked = dependency_states.iter().any(|(from_task, state)| {
                *from_task == task.id && !matches!(state, TaskState::Done | TaskState::Cancelled)
            });
            if blocked {
                task.state = TaskState::Blocked;
            } else if task.state == TaskState::Blocked {
                task.state = if task.session_ids.is_empty() {
                    TaskState::Queued
                } else {
                    TaskState::Running
                };
            }
        }
    }
}

pub fn launch_presets() -> [AgentProvider; 5] {
    [
        AgentProvider::ClaudeCode,
        AgentProvider::CodexCli,
        AgentProvider::GeminiCli,
        AgentProvider::Aider,
        AgentProvider::OpenCode,
    ]
}

fn derive_status(
    alive: bool,
    recent_output: bool,
    visible_text: &str,
    last_error: Option<&str>,
) -> AgentStatus {
    let text = visible_text.to_ascii_lowercase();
    let has_error = last_error.is_some()
        || [
            "error",
            "failed",
            "traceback",
            "exception",
            "panic",
            "command failed",
        ]
        .into_iter()
        .any(|needle| text.contains(needle));
    let waiting_approval = [
        "approve",
        "approval",
        "allow this command",
        "waiting for approval",
    ]
    .into_iter()
    .any(|needle| text.contains(needle));
    let needs_input = [
        "press enter",
        "continue?",
        "select an option",
        "waiting for input",
        "[y/n]",
    ]
    .into_iter()
    .any(|needle| text.contains(needle));
    let ready_for_review = [
        "ready for review",
        "review ready",
        "tests passed",
        "done. changed files",
    ]
    .into_iter()
    .any(|needle| text.contains(needle));

    if waiting_approval {
        AgentStatus::WaitingApproval
    } else if needs_input {
        AgentStatus::NeedsInput
    } else if has_error {
        AgentStatus::Failed
    } else if ready_for_review {
        AgentStatus::Reviewing
    } else if alive && recent_output {
        AgentStatus::Running
    } else if alive {
        AgentStatus::Idle
    } else {
        AgentStatus::Done
    }
}

fn summarize_command_output(visible_text: &str) -> Option<CommandSummary> {
    let lines = visible_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let excerpt = lines.last()?.to_owned();
    let title = extract_prompt_command(visible_text).unwrap_or_else(|| excerpt.to_owned());
    let failed = excerpt.to_ascii_lowercase().contains("error")
        || excerpt.to_ascii_lowercase().contains("failed");
    Some(CommandSummary {
        title,
        excerpt: excerpt.to_owned(),
        failed,
    })
}

fn apply_output_summaries(session: &mut AgentSessionMeta, visible_text: &str) {
    let text = visible_text.to_ascii_lowercase();
    if text.contains("tests passed") || text.contains("all checks passed") {
        session.review_summary.last_success = Some("Tests passed".to_owned());
        session.review_summary.tests = vec![TestStatus {
            label: "Suite".to_owned(),
            passed: true,
        }];
    }
    if let Some(error_line) = visible_text
        .lines()
        .rev()
        .find(|line| {
            let line = line.to_ascii_lowercase();
            line.contains("error") || line.contains("failed")
        })
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        session.review_summary.last_error = Some(error_line.to_owned());
    }
}

fn preview_label(provider: AgentProvider, task_title: Option<&str>, status: AgentStatus) -> String {
    if let Some(task_title) = task_title.filter(|task_title| !task_title.trim().is_empty()) {
        format!("{} · {}", provider.label(), truncate_text(task_title, 18))
    } else if provider == AgentProvider::Unknown {
        status.label().to_owned()
    } else {
        format!("{} · {}", provider.label(), status.label())
    }
}

fn truncate_branch(branch: &str) -> String {
    branch
        .rsplit('/')
        .next()
        .map(|short| truncate_text(short, 18))
        .unwrap_or_else(|| truncate_text(branch, 18))
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        text.to_owned()
    } else {
        format!(
            "{}…",
            text.chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    }
}

fn provider_bootstrap(provider: AgentProvider, brief: &str) -> ProviderBootstrap {
    let brief = brief.trim();
    match provider {
        AgentProvider::ClaudeCode => ProviderBootstrap {
            command: Some(if brief.is_empty() {
                "claude".to_owned()
            } else {
                format!("claude {}", shell_single_quote(brief))
            }),
            initial_input: None,
        },
        AgentProvider::GeminiCli => ProviderBootstrap {
            command: Some(if brief.is_empty() {
                "gemini".to_owned()
            } else {
                format!("gemini -i {}", shell_single_quote(brief))
            }),
            initial_input: None,
        },
        AgentProvider::OpenCode => ProviderBootstrap {
            command: Some(if brief.is_empty() {
                "opencode".to_owned()
            } else {
                format!("opencode --prompt {}", shell_single_quote(brief))
            }),
            initial_input: None,
        },
        AgentProvider::CodexCli => ProviderBootstrap {
            command: provider.launch_command().map(str::to_owned),
            initial_input: (!brief.is_empty()).then(|| brief.to_owned()),
        },
        AgentProvider::Aider => ProviderBootstrap {
            command: provider.launch_command().map(str::to_owned),
            initial_input: (!brief.is_empty()).then(|| brief.to_owned()),
        },
        AgentProvider::Unknown => ProviderBootstrap {
            command: None,
            initial_input: (!brief.is_empty()).then(|| brief.to_owned()),
        },
    }
}

fn shell_single_quote(input: &str) -> String {
    format!("'{}'", input.replace('\'', "'\"'\"'"))
}

fn session_matches_query(
    orchestrator: &Orchestrator,
    session: &AgentSessionMeta,
    query: &str,
) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    let mut haystacks = vec![
        session.label.to_ascii_lowercase(),
        session.provider.label().to_ascii_lowercase(),
        session.status.label().to_ascii_lowercase(),
    ];
    if let Some(branch) = &session.branch {
        haystacks.push(branch.to_ascii_lowercase());
    }
    if let Some(task) = session.task_id.and_then(|task_id| {
        orchestrator
            .state
            .tasks
            .iter()
            .find(|task| task.id == task_id)
    }) {
        haystacks.push(task.title.to_ascii_lowercase());
        haystacks.push(task.state.label().to_ascii_lowercase());
    }
    if let Some(error) = &session.review_summary.last_error {
        haystacks.push(error.to_ascii_lowercase());
    }
    if let Some(success) = &session.review_summary.last_success {
        haystacks.push(success.to_ascii_lowercase());
    }
    if let Some(summary) = &session.command_summary {
        haystacks.push(summary.title.to_ascii_lowercase());
        haystacks.push(summary.excerpt.to_ascii_lowercase());
    }
    haystacks
        .into_iter()
        .any(|haystack| haystack.contains(&query))
}

fn task_matches_query(orchestrator: &Orchestrator, task: &TaskCard, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    if task.title.to_ascii_lowercase().contains(&query)
        || task.brief.to_ascii_lowercase().contains(&query)
        || task.state.label().to_ascii_lowercase().contains(&query)
    {
        return true;
    }
    task.session_ids.iter().any(|session_id| {
        orchestrator
            .state
            .sessions
            .iter()
            .find(|session| session.session_id == *session_id)
            .map(|session| session_matches_query(orchestrator, session, query.as_str()))
            .unwrap_or(false)
    })
}

fn task_matches_filters(
    orchestrator: &Orchestrator,
    task: &TaskCard,
    provider_filter: Option<AgentProvider>,
    status_filter: Option<AgentStatus>,
) -> bool {
    let provider_matches = provider_filter.map(|provider| {
        task.provider_hint == Some(provider)
            || task.session_ids.iter().any(|session_id| {
                orchestrator
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.session_id == *session_id)
                    .map(|session| session.provider == provider)
                    .unwrap_or(false)
            })
    });

    let status_matches = status_filter.map(|status| {
        task.session_ids.iter().any(|session_id| {
            orchestrator
                .state
                .sessions
                .iter()
                .find(|session| session.session_id == *session_id)
                .map(|session| session.status == status)
                .unwrap_or(false)
        })
    });

    provider_matches.unwrap_or(true) && status_matches.unwrap_or(true)
}

fn inbox_matches_query(event: &InboxEvent, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    event.title.to_ascii_lowercase().contains(&query)
        || event.summary.to_ascii_lowercase().contains(&query)
}

fn event_matches_filters(
    orchestrator: &Orchestrator,
    event: &InboxEvent,
    provider_filter: Option<AgentProvider>,
    status_filter: Option<AgentStatus>,
) -> bool {
    let Some(session_id) = event.session_id else {
        return provider_filter.is_none() && status_filter.is_none();
    };
    let Some(session) = orchestrator
        .state
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
    else {
        return false;
    };
    provider_filter
        .map(|provider| session.provider == provider)
        .unwrap_or(true)
        && status_filter
            .map(|status| session.status == status)
            .unwrap_or(true)
}

fn deterministic_event_id(kind: InboxEventKind, session_id: Uuid) -> Uuid {
    let kind_mask = match kind {
        InboxEventKind::ApprovalPending => 0x11u128,
        InboxEventKind::NeedsInput => 0x22u128,
        InboxEventKind::TestsFailed => 0x33u128,
        InboxEventKind::ProcessDone => 0x44u128,
        InboxEventKind::ConflictRisk => 0x55u128,
        InboxEventKind::ReviewReady => 0x66u128,
    };
    Uuid::from_u128(session_id.as_u128() ^ kind_mask)
}

fn scene_template_defaults(template: SceneTemplateKind) -> Vec<(&'static str, AgentProvider)> {
    match template {
        SceneTemplateKind::Bugfix => vec![
            ("Reproduce and isolate", AgentProvider::CodexCli),
            ("Patch and verify", AgentProvider::ClaudeCode),
        ],
        SceneTemplateKind::FeatureParallel => vec![
            ("Backend implementation", AgentProvider::ClaudeCode),
            ("Frontend implementation", AgentProvider::CodexCli),
            ("Review and tests", AgentProvider::Aider),
        ],
        SceneTemplateKind::RefactorReview => vec![
            ("Refactor pass", AgentProvider::Aider),
            ("Regression review", AgentProvider::CodexCli),
        ],
        SceneTemplateKind::FrontendBackendSplit => vec![
            ("Frontend agent", AgentProvider::ClaudeCode),
            ("Backend agent", AgentProvider::GeminiCli),
            ("Integration review", AgentProvider::CodexCli),
        ],
    }
}

fn short_uuid(id: Uuid) -> String {
    id.simple().to_string()[..6].to_owned()
}

fn slugify(input: &str) -> String {
    let slug = input
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    slug.split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(32)
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

#[derive(Debug, Clone)]
struct GitObservation {
    repo_root: PathBuf,
    branch: String,
    dirty: bool,
    changed_files: Vec<PathBuf>,
    diff_stats: DiffStats,
}

fn inspect_git_state(path: &Path) -> Option<GitObservation> {
    let repo_root = git_repo_root(path)?;
    let branch = git_stdout(&repo_root, &["branch", "--show-current"])
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "detached".to_owned());
    let status = git_stdout(&repo_root, &["status", "--porcelain"])?;
    let changed_files = status
        .lines()
        .filter_map(|line| line.get(3..).map(str::trim))
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let dirty = !changed_files.is_empty();
    let diff_stats = parse_diff_stats(git_stdout(&repo_root, &["diff", "--shortstat", "HEAD"]));
    Some(GitObservation {
        repo_root,
        branch,
        dirty,
        changed_files,
        diff_stats,
    })
}

fn git_repo_root(path: &Path) -> Option<PathBuf> {
    git_stdout(path, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

fn git_stdout(path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_owned())
}

fn parse_diff_stats(raw: Option<String>) -> DiffStats {
    let Some(raw) = raw else {
        return DiffStats::default();
    };
    let mut stats = DiffStats::default();
    for segment in raw.split(',') {
        let segment = segment.trim();
        if let Some(value) = segment.split_whitespace().next() {
            if segment.contains("file changed") || segment.contains("files changed") {
                stats.files_changed = value.parse().unwrap_or(0);
            } else if segment.contains("insertion") {
                stats.insertions = value.parse().unwrap_or(0);
            } else if segment.contains("deletion") {
                stats.deletions = value.parse().unwrap_or(0);
            }
        }
    }
    stats
}

fn create_git_worktree(repo_root: &Path, worktree_path: &Path, branch: &str) -> anyhow::Result<()> {
    if worktree_path.exists() {
        return Ok(());
    }
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "add", "-b", branch])
        .arg(worktree_path)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn extract_prompt_command(visible_text: &str) -> Option<String> {
    for line in visible_text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        for marker in [" % ", " $ ", "> "] {
            if let Some(index) = line.rfind(marker) {
                let tail = line[index + marker.len()..].trim();
                if tail.is_empty() {
                    continue;
                }
                return tail
                    .split_whitespace()
                    .next()
                    .map(|part| part.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`')))
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{
        derive_status, launch_presets, parse_diff_stats, preview_label, provider_bootstrap,
        short_uuid, slugify, AgentProvider, AgentStatus, DependencyKind, DiffStats, Orchestrator,
        PanelRuntimeObservation, TaskState,
    };

    #[test]
    fn slugify_normalizes_branch_names() {
        assert_eq!(
            slugify("Frontend / Backend Review!"),
            "frontend-backend-review"
        );
    }

    #[test]
    fn diff_stats_parser_extracts_counts() {
        let stats = parse_diff_stats(Some(
            "3 files changed, 18 insertions(+), 4 deletions(-)".to_owned(),
        ));

        assert_eq!(
            stats,
            DiffStats {
                files_changed: 3,
                insertions: 18,
                deletions: 4,
            }
        );
    }

    #[test]
    fn provider_presets_include_supported_clis() {
        let presets = launch_presets();

        assert!(presets.contains(&AgentProvider::ClaudeCode));
        assert!(presets.contains(&AgentProvider::CodexCli));
        assert!(presets.contains(&AgentProvider::GeminiCli));
        assert!(presets.contains(&AgentProvider::Aider));
        assert!(presets.contains(&AgentProvider::OpenCode));
    }

    #[test]
    fn status_heuristics_detect_attention_states() {
        assert_eq!(
            derive_status(true, true, "Waiting for approval to run command", None),
            AgentStatus::WaitingApproval
        );
        assert_eq!(
            derive_status(true, true, "Select an option to continue", None),
            AgentStatus::NeedsInput
        );
        assert_eq!(
            derive_status(true, true, "Tests passed. Ready for review.", None),
            AgentStatus::Reviewing
        );
        assert_eq!(derive_status(false, false, "Done", None), AgentStatus::Done);
    }

    #[test]
    fn preview_label_prioritizes_provider_and_task() {
        assert_eq!(
            preview_label(
                AgentProvider::ClaudeCode,
                Some("Big feature"),
                AgentStatus::Running
            ),
            "Claude Code · Big feature"
        );
    }

    #[test]
    fn ensure_panel_session_reuses_existing_panel_binding() {
        let mut orchestrator = Orchestrator::new();
        let workspace_id = uuid::Uuid::new_v4();
        let panel_id = uuid::Uuid::new_v4();
        let first = orchestrator.ensure_panel_session(
            workspace_id,
            None,
            panel_id,
            Some(uuid::Uuid::new_v4()),
            "Terminal",
        );
        let second = orchestrator.ensure_panel_session(
            workspace_id,
            None,
            panel_id,
            Some(uuid::Uuid::new_v4()),
            "Terminal",
        );

        assert_eq!(first, second);
        assert_eq!(orchestrator.sessions().len(), 1);
    }

    #[test]
    fn scene_template_creates_draft_tasks() {
        let mut orchestrator = Orchestrator::new();
        let workspace_id = uuid::Uuid::new_v4();

        orchestrator.apply_scene_template(workspace_id, super::SceneTemplateKind::Bugfix);

        assert_eq!(orchestrator.tasks().len(), 2);
        assert!(orchestrator
            .tasks()
            .iter()
            .all(|task| task.state == TaskState::Draft));
    }

    #[test]
    fn apply_observations_updates_status_and_command_summary() {
        let mut orchestrator = Orchestrator::new();
        let workspace_id = uuid::Uuid::new_v4();
        let panel_id = uuid::Uuid::new_v4();
        let session_id = orchestrator.ensure_panel_session(
            workspace_id,
            None,
            panel_id,
            Some(uuid::Uuid::new_v4()),
            "claude",
        );

        orchestrator.apply_observations(vec![PanelRuntimeObservation {
            panel_id,
            runtime_session_id: Some(uuid::Uuid::new_v4()),
            workspace_id,
            title: "claude".to_owned(),
            visible_text: "mauro % claude\nTests passed. Ready for review.".to_owned(),
            alive: true,
            recent_output: true,
        }]);

        let session = orchestrator
            .sessions()
            .iter()
            .find(|session| session.session_id == session_id)
            .unwrap();
        assert_eq!(session.provider, AgentProvider::ClaudeCode);
        assert_eq!(session.status, AgentStatus::Reviewing);
        assert!(session.command_summary.is_some());
        assert_eq!(
            session.last_activity_at.date_naive(),
            Utc::now().date_naive()
        );
    }

    #[test]
    fn short_uuid_uses_six_chars() {
        assert_eq!(short_uuid(uuid::Uuid::nil()).len(), 6);
    }

    #[test]
    fn dependency_blocks_task_until_target_is_done() {
        let mut orchestrator = Orchestrator::new();
        let workspace_id = uuid::Uuid::new_v4();
        let blocked_task = orchestrator.create_task(
            workspace_id,
            "Implement feature",
            "",
            Some(AgentProvider::ClaudeCode),
        );
        let dependency_task = orchestrator.create_task(
            workspace_id,
            "Database migration",
            "",
            Some(AgentProvider::CodexCli),
        );

        orchestrator.set_task_dependency(blocked_task, dependency_task, DependencyKind::BlockedBy);

        assert_eq!(
            orchestrator.task_snapshot(blocked_task).unwrap().state,
            TaskState::Blocked
        );

        orchestrator.mark_task_state(dependency_task, TaskState::Done);

        assert_eq!(
            orchestrator.task_snapshot(blocked_task).unwrap().state,
            TaskState::Queued
        );
    }

    #[test]
    fn session_filters_respect_provider_and_status() {
        let mut orchestrator = Orchestrator::new();
        let workspace_id = uuid::Uuid::new_v4();
        let panel_id = uuid::Uuid::new_v4();

        let session_id = orchestrator.ensure_panel_session(
            workspace_id,
            None,
            panel_id,
            None,
            "Claude Code — Build feature",
        );
        orchestrator.apply_observations(vec![PanelRuntimeObservation {
            panel_id,
            runtime_session_id: None,
            workspace_id,
            title: "Claude Code — Build feature".to_owned(),
            visible_text: "Waiting for approval to run command".to_owned(),
            alive: true,
            recent_output: true,
        }]);

        let matching = orchestrator.session_items(
            "approval",
            true,
            Some(AgentProvider::ClaudeCode),
            Some(AgentStatus::WaitingApproval),
        );
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].session_id, session_id);

        let non_matching = orchestrator.session_items(
            "",
            false,
            Some(AgentProvider::CodexCli),
            Some(AgentStatus::Running),
        );
        assert!(non_matching.is_empty());
    }

    #[test]
    fn provider_bootstrap_embeds_prompt_when_cli_supports_it() {
        let bootstrap = provider_bootstrap(AgentProvider::ClaudeCode, "Fix auth race");

        assert_eq!(bootstrap.command.as_deref(), Some("claude 'Fix auth race'"));
        assert_eq!(bootstrap.initial_input, None);
    }

    #[test]
    fn provider_bootstrap_falls_back_to_initial_input_for_unknown_prompt_support() {
        let bootstrap = provider_bootstrap(AgentProvider::CodexCli, "Review backend");

        assert_eq!(bootstrap.command.as_deref(), Some("codex"));
        assert_eq!(bootstrap.initial_input.as_deref(), Some("Review backend"));
    }
}
