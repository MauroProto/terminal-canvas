mod agent_sessions;
mod code_diff;
mod git;
mod manager;
mod matching;

#[allow(unused_imports)]
pub use agent_sessions::{list_claude_sessions, AgentSessionEntry};
#[allow(unused_imports)]
pub use code_diff::{
    list_git_worktrees, parse_unified_diff, remove_git_worktree, DiffLine, DiffLineKind,
    DiffLoader, FileDiff, RepoDiff, WorktreeInfo, WorktreeJob, WorktreeOps,
};
#[allow(unused_imports)]
pub use manager::{
    launch_presets, resume_command, AgentLaunchPlan, AgentLaunchRequest, AgentProvider,
    AgentSessionMeta, AgentStatus, CommandSummary, DependencyEdge, DependencyKind, DiffStats,
    InboxEvent, InboxEventKind, LaunchOutcome, LaunchPreparation, OrchestrationState, Orchestrator,
    PanelOverlay, PanelRuntimeObservation, ReviewSummary, SceneTemplate, SceneTemplateKind,
    SessionListItem, TaskCard, TaskState, TestStatus, WorktreeMode,
};
