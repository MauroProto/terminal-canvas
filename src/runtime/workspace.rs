use std::path::PathBuf;

use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeWorkspace {
    id: Uuid,
    name: String,
    cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeWorkspaceSnapshot {
    pub id: Uuid,
    pub name: String,
    pub cwd: Option<PathBuf>,
}

impl RuntimeWorkspace {
    pub fn new(name: impl Into<String>, cwd: Option<PathBuf>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            cwd,
        }
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn snapshot(&self) -> RuntimeWorkspaceSnapshot {
        RuntimeWorkspaceSnapshot {
            id: self.id,
            name: self.name.clone(),
            cwd: self.cwd.clone(),
        }
    }
}

impl RuntimeWorkspaceSnapshot {
    pub fn new(id: Uuid, name: impl Into<String>, cwd: Option<PathBuf>) -> Self {
        Self {
            id,
            name: name.into(),
            cwd,
        }
    }
}
