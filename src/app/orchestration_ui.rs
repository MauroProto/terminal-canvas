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
        self.prune_panel_keyed_state(&live_panel_ids);
    }

    /// Descarta el estado por panel de los paneles que ya no existen.
    ///
    /// Estos dos mapas se llenaban al abrir un panel y no se vaciaban nunca:
    /// en una sesión larga, cada terminal abierto y cerrado dejaba su entrada
    /// para siempre. Es poca memoria por entrada, pero crecimiento sin cota al
    /// fin y al cabo.
    fn prune_panel_keyed_state(&mut self, live_panel_ids: &HashSet<Uuid>) {
        self.scrollback_restored
            .retain(|panel_id| live_panel_ids.contains(panel_id));
        // `agent_status_seen` va por sesión de runtime, no por panel.
        let live_sessions: HashSet<Uuid> = self
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.panels.iter())
            .filter_map(|panel| panel.runtime_session_id())
            .collect();
        retain_seen_sessions(&mut self.agent_status_seen, &live_sessions);
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
        self.notify_agent_attention_transitions();
        self.last_orchestration_refresh = Instant::now();
    }

    /// Notifica al SO cuando una sesión de agente pasa a un estado de atención
    /// (esperando aprobación, input, o falló). Solo en la transición, no se
    /// repite mientras siga en el mismo estado.
    fn notify_agent_attention_transitions(&mut self) {
        if !crate::config::runtime_config().agent_notifications {
            return;
        }
        let sessions: Vec<(
            Uuid,
            crate::orchestration::AgentStatus,
            &'static str,
            String,
        )> = self
            .orchestrator
            .sessions()
            .iter()
            .map(|session| {
                (
                    session.session_id,
                    session.status,
                    session.provider.label(),
                    session.label.clone(),
                )
            })
            .collect();
        for (title, body) in attention_transitions(&mut self.agent_status_seen, &sessions) {
            crate::utils::platform::notify(&title, &body);
        }
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

/// Dada la historia de estados vistos y los estados actuales, devuelve
/// `(title, body)` para cada sesión que TRANSICIONÓ a un estado de atención.
/// Actualiza `seen` como efecto lateral. Pura y testeable (sin notificar).
/// Descarta el estado de sesiones que ya no existen.
///
/// `attention_transitions` inserta una entrada por cada sesión que observa y
/// nunca borra: sin esta poda el mapa crece durante toda la vida del proceso.
fn retain_seen_sessions(
    seen: &mut std::collections::HashMap<Uuid, crate::orchestration::AgentStatus>,
    live_sessions: &HashSet<Uuid>,
) {
    seen.retain(|session_id, _| live_sessions.contains(session_id));
}

fn attention_transitions(
    seen: &mut std::collections::HashMap<Uuid, crate::orchestration::AgentStatus>,
    sessions: &[(
        Uuid,
        crate::orchestration::AgentStatus,
        &'static str,
        String,
    )],
) -> Vec<(String, String)> {
    use crate::orchestration::AgentStatus;
    let mut out = Vec::new();
    for (session_id, status, provider_label, label) in sessions {
        let previous = seen.get(session_id).copied();
        seen.insert(*session_id, *status);
        if previous == Some(*status) {
            continue;
        }
        let attention = matches!(
            status,
            AgentStatus::WaitingApproval | AgentStatus::NeedsInput | AgentStatus::Failed
        );
        if !attention {
            continue;
        }
        let title = format!("Agente: {provider_label}");
        let body = if label.trim().is_empty() {
            status.label().to_owned()
        } else {
            format!("{} — {}", label, status.label())
        };
        out.push((title, body));
    }
    out
}

#[cfg(test)]
mod attention_tests {
    use std::collections::HashMap;

    use uuid::Uuid;

    use super::attention_transitions;
    use crate::orchestration::AgentStatus;

    fn session(
        id: Uuid,
        status: AgentStatus,
        label: &str,
    ) -> (Uuid, AgentStatus, &'static str, String) {
        (id, status, "Claude Code", label.to_owned())
    }

    #[test]
    fn notifies_on_transition_into_attention() {
        let mut seen = HashMap::new();
        let id = Uuid::new_v4();
        let notifications = attention_transitions(
            &mut seen,
            &[session(id, AgentStatus::WaitingApproval, "Fix bug")],
        );
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].0, "Agente: Claude Code");
        assert!(notifications[0].1.contains("Fix bug"));
    }

    #[test]
    fn does_not_repeat_while_in_same_attention_state() {
        let mut seen = HashMap::new();
        let id = Uuid::new_v4();
        let first =
            attention_transitions(&mut seen, &[session(id, AgentStatus::NeedsInput, "Task")]);
        assert_eq!(first.len(), 1);
        let second =
            attention_transitions(&mut seen, &[session(id, AgentStatus::NeedsInput, "Task")]);
        assert!(second.is_empty(), "no debe repetir en el mismo estado");
    }

    #[test]
    fn ignores_non_attention_states() {
        let mut seen = HashMap::new();
        let id = Uuid::new_v4();
        let notifications =
            attention_transitions(&mut seen, &[session(id, AgentStatus::Running, "Task")]);
        assert!(notifications.is_empty());
    }

    #[test]
    fn notifies_again_on_transition_to_different_attention_state() {
        let mut seen = HashMap::new();
        let id = Uuid::new_v4();
        let _ = attention_transitions(&mut seen, &[session(id, AgentStatus::WaitingApproval, "T")]);
        let again = attention_transitions(&mut seen, &[session(id, AgentStatus::Failed, "T")]);
        assert_eq!(
            again.len(),
            1,
            "transición a otro estado de atención notifica"
        );
    }
    #[test]
    fn seen_state_stays_bounded_when_sessions_come_and_go() {
        // Regresión de fuga: sin poda, cada sesión observada dejaba su entrada
        // para siempre. Simulamos 500 sesiones efímeras con una sola viva.
        use std::collections::HashSet;

        let survivor = Uuid::new_v4();
        let mut seen = HashMap::new();

        for _ in 0..500 {
            let ephemeral = Uuid::new_v4();
            let batch = vec![
                session(survivor, AgentStatus::Running, "vive"),
                session(ephemeral, AgentStatus::Running, "efimera"),
            ];
            attention_transitions(&mut seen, &batch);

            // La sesión efímera se cierra; sólo sobrevive la otra.
            let live: HashSet<Uuid> = [survivor].into_iter().collect();
            super::retain_seen_sessions(&mut seen, &live);
        }

        assert_eq!(
            seen.len(),
            1,
            "the map must not grow with sessions that already died"
        );
        assert!(seen.contains_key(&survivor));
    }

    #[test]
    fn pruning_keeps_every_live_session() {
        use std::collections::HashSet;

        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut seen = HashMap::new();
        attention_transitions(
            &mut seen,
            &[
                session(a, AgentStatus::Running, "a"),
                session(b, AgentStatus::Idle, "b"),
            ],
        );

        let live: HashSet<Uuid> = [a, b].into_iter().collect();
        super::retain_seen_sessions(&mut seen, &live);
        assert_eq!(seen.len(), 2, "live sessions must never be dropped");
    }
}
