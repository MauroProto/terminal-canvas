mod agent_sessions;
mod code_diff;
mod diff_notes;
mod git;
mod manager;
mod matching;
mod worktree_removal_safety;
mod worktree_trash;

#[allow(unused_imports)]
pub use agent_sessions::{list_claude_sessions, sanitize_session_id, AgentSessionEntry};
#[allow(unused_imports)]
pub use code_diff::{
    list_git_worktrees, parse_unified_diff, remove_git_worktree, DiffLine, DiffLineKind,
    DiffLoader, FileDiff, RepoDiff, WorktreeInfo, WorktreeJob, WorktreeOps,
};
#[allow(unused_imports)]
pub use diff_notes::{format_note, load_notes, save_notes, DiffNote, DiffNotes};
#[allow(unused_imports)]
pub use manager::{
    launch_presets, resume_command, resume_invocation, AgentLaunchPlan, AgentLaunchRequest,
    AgentProvider, AgentSessionMeta, AgentStatus, CommandSummary, DependencyEdge, DependencyKind,
    DiffStats, InboxEvent, InboxEventKind, LaunchOutcome, LaunchPreparation, OrchestrationState,
    Orchestrator, PanelOverlay, PanelRuntimeObservation, ReviewSummary, SceneTemplate,
    SceneTemplateKind, SessionListItem, TaskCard, TaskState, TestStatus, WorktreeMode,
};
#[allow(unused_imports)]
pub use worktree_removal_safety::{check_recursive_delete, RemovalGuard};
#[allow(unused_imports)]
pub use worktree_trash::{move_to_trash, sweep_stale_trash, trash_dir};
