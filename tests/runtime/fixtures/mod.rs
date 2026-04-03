use std::collections::HashSet;

use uuid::Uuid;

use crate::runtime::{
    RenderInputs, RenderQos, RenderTier, RuntimeRegistry, RuntimeScheduler, SessionSpec,
};

#[derive(Debug, Clone)]
struct HarnessSession {
    id: Uuid,
    workspace_id: Uuid,
    visible: bool,
    focused: bool,
    streaming: bool,
    screen_area: f32,
}

#[derive(Default)]
pub struct RuntimeHarness {
    registry: RuntimeRegistry,
    scheduler: RuntimeScheduler,
    sessions: Vec<HarnessSession>,
    drained_batches: usize,
    idle_after_step: bool,
}

impl RuntimeHarness {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn seed(&mut self, workspace_count: usize, session_count: usize) {
        assert!(
            workspace_count > 0,
            "workspace_count must be greater than zero"
        );
        assert!(
            session_count >= workspace_count,
            "session_count must cover all workspaces"
        );

        self.registry = RuntimeRegistry::new();
        self.scheduler = RuntimeScheduler::new_for_tests();
        self.sessions.clear();
        self.drained_batches = 0;
        self.idle_after_step = false;

        let workspace_ids = (0..workspace_count)
            .map(|index| {
                self.registry.create_workspace(
                    format!("workspace-{index}"),
                    Some(format!("/tmp/workspace-{index}").into()),
                )
            })
            .collect::<Vec<_>>();

        for index in 0..session_count {
            let workspace_id = workspace_ids[index % workspace_ids.len()];
            let session_id = self.registry.create_session_with_spec(
                workspace_id,
                SessionSpec {
                    title: format!("session-{index}"),
                    cwd: Some(
                        format!("/tmp/workspace-{}/{index}", index % workspace_ids.len()).into(),
                    ),
                    startup_command: None,
                    startup_input: None,
                },
            );

            self.sessions.push(HarnessSession {
                id: session_id,
                workspace_id,
                visible: index < 6 || index % 9 == 0,
                focused: index == 0,
                streaming: index < 3 || index % 5 == 0,
                screen_area: match index {
                    0 => 180_000.0,
                    1..=5 => 42_000.0 - index as f32 * 2_000.0,
                    _ if index % 2 == 0 => 8_000.0,
                    _ => 0.0,
                },
            });
        }
    }

    pub fn emit_output_bursts(&mut self) {
        for session in &self.sessions {
            if session.streaming {
                for _ in 0..6 {
                    self.scheduler.record_output(session.id);
                }
            } else if session.visible {
                self.scheduler.record_render(session.id);
            }
        }
    }

    pub fn step(&mut self) {
        self.drained_batches = 0;
        self.idle_after_step = false;

        loop {
            let batch = self.scheduler.drain_ui_updates();
            if batch.session_updates.is_empty() && !batch.repaint_requested {
                self.idle_after_step = true;
                break;
            }

            self.drained_batches += 1;
            if self.drained_batches > 8 {
                self.idle_after_step = false;
                break;
            }
        }
    }

    pub fn session_count(&self) -> usize {
        self.registry.snapshot().sessions.len()
    }

    pub fn no_deadlocks(&self) -> bool {
        self.idle_after_step && self.drained_batches > 0 && self.drained_batches <= 8
    }

    pub fn snapshot_is_consistent(&self) -> bool {
        let snapshot = self.registry.snapshot();
        let workspace_ids = snapshot
            .workspaces
            .iter()
            .map(|workspace| workspace.id)
            .collect::<HashSet<_>>();
        let unique_session_ids = snapshot
            .sessions
            .iter()
            .map(|session| session.id)
            .collect::<HashSet<_>>();

        snapshot.sessions.len() == self.sessions.len()
            && unique_session_ids.len() == snapshot.sessions.len()
            && snapshot
                .sessions
                .iter()
                .all(|session| workspace_ids.contains(&session.workspace_id))
            && self
                .sessions
                .iter()
                .filter(|session| session.focused)
                .count()
                == 1
            && self.sessions.iter().any(|session| session.visible)
            && self.sessions.iter().any(|session| !session.visible)
            && self.sessions.iter().any(|session| session.streaming)
    }

    pub fn has_mixed_render_tiers(&self) -> bool {
        let mut has_full = false;
        let mut has_reduced = false;
        let mut has_preview = false;
        let mut has_hidden = false;

        for session in &self.sessions {
            match RenderQos::decide(RenderInputs {
                visible: session.visible,
                focused: session.focused,
                screen_area: session.screen_area,
                streaming: session.streaming,
                fast_path: false,
                renderable: true,
            }) {
                RenderTier::Full => has_full = true,
                RenderTier::ReducedLive => has_reduced = true,
                RenderTier::Preview => has_preview = true,
                RenderTier::Hidden => has_hidden = true,
            }
        }

        has_full && has_reduced && has_preview && has_hidden
    }

    pub fn every_session_is_registered_in_seeded_workspace(&self) -> bool {
        let seeded_workspaces = self
            .registry
            .snapshot()
            .workspaces
            .into_iter()
            .map(|workspace| workspace.id)
            .collect::<HashSet<_>>();

        self.sessions
            .iter()
            .all(|session| seeded_workspaces.contains(&session.workspace_id))
    }
}
