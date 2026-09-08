use std::path::PathBuf;

use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    pub title: String,
    pub cwd: Option<PathBuf>,
    pub startup_command: Option<String>,
    pub startup_input: Option<String>,
    /// Panel dueño de la sesión, exportado como `TC_PANEL_ID` para que los
    /// hooks del agente puedan decir de dónde vienen (P2.12).
    pub panel_id: Option<Uuid>,
    /// Workspace dueño de la sesión, exportado como `TC_WORKSPACE_ID`.
    pub workspace_id: Option<Uuid>,
    /// Hoja estable dentro del panel. Permite que el daemon persista varias
    /// sesiones de un split sin sobrescribirlas bajo el mismo `panel_id`.
    pub leaf_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSession {
    id: Uuid,
    workspace_id: Uuid,
    title: String,
    cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSessionSnapshot {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: String,
    pub cwd: Option<PathBuf>,
}

impl Default for SessionSpec {
    fn default() -> Self {
        Self {
            title: "Terminal".to_owned(),
            cwd: None,
            startup_command: None,
            startup_input: None,
            panel_id: None,
            workspace_id: None,
            leaf_id: None,
        }
    }
}

impl RuntimeSession {
    pub fn new(workspace_id: Uuid, spec: SessionSpec) -> Self {
        Self {
            id: Uuid::new_v4(),
            workspace_id,
            title: spec.title,
            cwd: spec.cwd,
        }
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn snapshot(&self) -> RuntimeSessionSnapshot {
        RuntimeSessionSnapshot {
            id: self.id,
            workspace_id: self.workspace_id,
            title: self.title.clone(),
            cwd: self.cwd.clone(),
        }
    }
}
