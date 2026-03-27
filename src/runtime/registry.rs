use std::path::PathBuf;

use uuid::Uuid;

use super::{
    RuntimeSession, RuntimeSessionSnapshot, RuntimeWorkspace, RuntimeWorkspaceSnapshot, SessionSpec,
};

#[derive(Debug, Default)]
pub struct RuntimeRegistry {
    workspaces: Vec<RuntimeWorkspace>,
    sessions: Vec<RuntimeSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeSnapshot {
    pub workspaces: Vec<RuntimeWorkspaceSnapshot>,
    pub sessions: Vec<RuntimeSessionSnapshot>,
}

impl RuntimeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_workspace(&mut self, name: impl Into<String>, cwd: Option<PathBuf>) -> Uuid {
        let workspace = RuntimeWorkspace::new(name, cwd);
        let id = workspace.id();
        self.workspaces.push(workspace);
        id
    }

    pub fn create_session(&mut self, workspace_id: Uuid) -> Uuid {
        self.create_session_with_spec(workspace_id, SessionSpec::default())
    }

    pub fn create_session_with_spec(&mut self, workspace_id: Uuid, spec: SessionSpec) -> Uuid {
        assert!(
            self.workspaces
                .iter()
                .any(|workspace| workspace.id() == workspace_id),
            "workspace must exist before creating a session"
        );
        let session = RuntimeSession::new(workspace_id, spec);
        let id = session.id();
        self.sessions.push(session);
        id
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            workspaces: self
                .workspaces
                .iter()
                .map(RuntimeWorkspace::snapshot)
                .collect(),
            sessions: self.sessions.iter().map(RuntimeSession::snapshot).collect(),
        }
    }
}

impl RuntimeSnapshot {
    pub fn sessions_by_workspace(&self, workspace_id: &Uuid) -> Vec<&RuntimeSessionSnapshot> {
        self.sessions
            .iter()
            .filter(|session| &session.workspace_id == workspace_id)
            .collect()
    }
}
