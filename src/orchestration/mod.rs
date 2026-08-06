mod agent_sessions;
mod claude_hooks;
mod code_diff;
mod diff_notes;
mod gh_client;
mod git;
mod hook_server;
mod linear_client;
mod manager;
mod matching;
mod worktree_removal_safety;
mod worktree_trash;

#[allow(unused_imports)]
pub use agent_sessions::{list_claude_sessions, sanitize_session_id, AgentSessionEntry};
#[allow(unused_imports)]
pub use claude_hooks::{
    install_to_disk as install_claude_hooks, uninstall_from_disk as uninstall_claude_hooks,
};
#[allow(unused_imports)]
pub use code_diff::{
    list_git_worktrees, parse_unified_diff, remove_git_worktree, DiffLine, DiffLineKind,
    DiffLoader, FileDiff, RepoDiff, WorktreeInfo, WorktreeJob, WorktreeOps,
};
#[allow(unused_imports)]
pub use diff_notes::{format_note, load_notes, save_notes, DiffNote, DiffNotes};
#[allow(unused_imports)]
pub use gh_client::{
    issue_branch_name, issue_prompt, GhAvailability, GhClient, GhResult, GhSnapshot, Issue,
    PullRequest,
};
#[allow(unused_imports)]
pub use hook_server::{format_design_capture, DesignCapture, HookEvent, HookKind, HookServer};
#[allow(unused_imports)]
pub use linear_client::{
    linear_branch_name, linear_prompt, LinearAvailability, LinearClient, LinearIssue,
    LinearSnapshot,
};
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
