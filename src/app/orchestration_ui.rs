//! Glue de orquestación de la app: reconciliación de sesiones de agentes con
//! los paneles vivos, refresh periódico de observaciones y el flujo de
//! lanzamiento de agentes (con worktree asíncrono).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::orchestration::{
    AgentLaunchPlan, AgentLaunchRequest, AgentProvider, LaunchOutcome, LaunchPreparation,
    PanelRuntimeObservation, WorktreeMode,
};
use crate::state::{TerminalSpawnRequest, Workspace};

use super::TerminalApp;

pub(super) const ORCHESTRATION_REFRESH_INTERVAL: Duration = Duration::from_millis(750);
#[derive(Clone)]
pub(super) struct LaunchAgentDraft {
    pub(super) workspace_id: Uuid,
    pub(super) provider: AgentProvider,
    pub(super) task_title: String,
    pub(super) brief: String,
    pub(super) worktree_mode: WorktreeMode,
    pub(super) error: Option<String>,
    // El worktree del agente se crea en un worker; mientras tanto el diálogo
    // queda abierto mostrando progreso.
    pub(super) pending_session: Option<Uuid>,
}

impl TerminalApp {
    pub(super) fn reconcile_orchestration(&mut self) {
        let mut live_panel_ids = HashSet::new();
        for workspace in &self.workspaces {
            for panel in &workspace.panels {
                live_panel_ids.insert(panel.id());
                self.orchestrator.ensure_panel_session(
                    workspace.id,
                    workspace.cwd.clone(),
                    panel.id(),
                    panel.runtime_session_id(),
                    panel.title(),
                );
            }
        }
        self.orchestrator.prune_missing_panels(&live_panel_ids);
    }

    pub(super) fn collect_observations(&self) -> Vec<PanelRuntimeObservation> {
        self.workspaces
            .iter()
            .flat_map(Workspace::orchestration_observations)
            .collect()
    }

    pub(super) fn refresh_orchestration(&mut self) {
        let observations = self.collect_observations();
        self.orchestrator.apply_observations(observations);
        self.last_orchestration_refresh = Instant::now();
    }

    pub(super) fn maybe_refresh_orchestration(&mut self) {
        if self.panel_gesture.is_some() {
            return;
        }
        if self.last_orchestration_refresh.elapsed() >= ORCHESTRATION_REFRESH_INTERVAL {
            self.reconcile_orchestration();
            let started_at = Instant::now();
            self.refresh_orchestration();
            self.last_orchestration_scan_duration = started_at.elapsed();
        }
    }
    pub(super) fn open_launch_agent_dialog(&mut self) {
        let workspace_id = self.ws().id;
        self.launch_agent = Some(LaunchAgentDraft {
            workspace_id,
            provider: AgentProvider::ClaudeCode,
            task_title: "".to_owned(),
            brief: "".to_owned(),
            worktree_mode: WorktreeMode::Auto,
            error: None,
            pending_session: None,
        });
    }
    pub(super) fn submit_launch_agent(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.launch_agent.clone() else {
            return;
        };
        let request = AgentLaunchRequest {
            workspace_id: draft.workspace_id,
            task_id: None,
            base_cwd: self
                .workspace_index_by_id(draft.workspace_id)
                .and_then(|index| self.workspaces.get(index))
                .and_then(|workspace| workspace.cwd.clone()),
            provider: draft.provider,
            task_title: draft.task_title.clone(),
            brief: draft.brief.clone(),
            worktree_mode: draft.worktree_mode,
        };
        let preparation = match self.orchestrator.prepare_launch(request) {
            Ok(preparation) => preparation,
            Err(err) => {
                if let Some(current) = self.launch_agent.as_mut() {
                    current.error = Some(err.to_string());
                }
                return;
            }
        };
        match preparation {
            LaunchPreparation::Ready(plan) => {
                if self.spawn_agent_panel(ctx, &plan) {
                    self.launch_agent = None;
                }
            }
            LaunchPreparation::PendingWorktree { session_id } => {
                // El worktree se está creando en el worker; el diálogo queda
                // abierto con progreso y poll_pending_launches lo cierra.
                if let Some(current) = self.launch_agent.as_mut() {
                    current.error = None;
                    current.pending_session = Some(session_id);
                }
            }
        }
    }

    pub(super) fn poll_pending_launches(&mut self, ctx: &egui::Context) {
        if self.orchestrator.has_pending_launches() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        for outcome in self.orchestrator.poll_ready_launches() {
            match outcome {
                LaunchOutcome::Ready(plan) => {
                    let spawned = self.spawn_agent_panel(ctx, &plan);
                    let matches_draft = self
                        .launch_agent
                        .as_ref()
                        .is_some_and(|draft| draft.pending_session == Some(plan.session_id));
                    if matches_draft {
                        if spawned {
                            self.launch_agent = None;
                        } else if let Some(draft) = self.launch_agent.as_mut() {
                            draft.pending_session = None;
                        }
                    }
                }
                LaunchOutcome::Failed { session_id, error } => {
                    log::warn!("agent worktree creation failed: {error}");
                    if let Some(draft) = self.launch_agent.as_mut() {
                        if draft.pending_session == Some(session_id) {
                            draft.pending_session = None;
                            draft.error = Some(format!("No se pudo crear el worktree: {error}"));
                        }
                    }
                }
            }
        }
    }

    /// Crea el panel de terminal para un plan de agente listo. Devuelve false
    /// si el workspace destino ya no existe.
    pub(super) fn spawn_agent_panel(
        &mut self,
        ctx: &egui::Context,
        plan: &AgentLaunchPlan,
    ) -> bool {
        let Some(workspace_index) = self.workspace_index_by_id(plan.workspace_id) else {
            if let Some(current) = self.launch_agent.as_mut() {
                current.error = Some("Workspace not found".to_owned());
            }
            return false;
        };
        let spawned = {
            let workspace = &mut self.workspaces[workspace_index];
            workspace.spawn_terminal_with_request(
                ctx,
                TerminalSpawnRequest {
                    title: Some(plan.panel_title.clone()),
                    cwd: plan.cwd.clone(),
                    startup_command: plan.startup_command.clone(),
                    startup_input: plan.startup_input.clone(),
                },
            )
        };
        self.orchestrator.bind_launch_to_panel(
            plan.session_id,
            spawned.panel_id,
            spawned.runtime_session_id,
        );
        self.switch_workspace(workspace_index);
        self.ws_mut().bring_to_front(spawned.panel_id);
        self.reconcile_orchestration();
        self.refresh_orchestration();
        true
    }
}
