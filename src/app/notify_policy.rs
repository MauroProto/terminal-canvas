//! Política de notificaciones (P1.8): un gate por workspace con cooldown para
//! no ametrallar al usuario con notificaciones del SO cuando varios eventos de
//! atención (bell, agente esperando/terminó) caen juntos.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use uuid::Uuid;

/// Cooldown entre notificaciones del SO por workspace.
pub const NOTIFICATION_COOLDOWN: Duration = Duration::from_secs(5);

/// Gate que deja pasar a lo sumo una notificación por workspace cada
/// `cooldown`. Puro y testeable.
#[derive(Default)]
pub struct NotificationGate {
    last_by_workspace: HashMap<Uuid, Instant>,
    cooldown: Duration,
}

impl NotificationGate {
    pub fn new(cooldown: Duration) -> Self {
        Self {
            last_by_workspace: HashMap::new(),
            cooldown,
        }
    }

    /// ¿Puede notificarse ahora en este workspace? Si sí, registra el momento.
    pub fn allow(&mut self, workspace_id: Uuid, now: Instant) -> bool {
        match self.last_by_workspace.get(&workspace_id) {
            Some(last) if now.duration_since(*last) < self.cooldown => false,
            _ => {
                self.last_by_workspace.insert(workspace_id, now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NotificationGate, NOTIFICATION_COOLDOWN};
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    #[test]
    fn first_event_is_allowed() {
        let mut gate = NotificationGate::new(NOTIFICATION_COOLDOWN);
        let ws = Uuid::new_v4();
        let now = Instant::now();
        assert!(gate.allow(ws, now));
    }

    #[test]
    fn two_events_within_cooldown_yield_one_notification() {
        let mut gate = NotificationGate::new(NOTIFICATION_COOLDOWN);
        let ws = Uuid::new_v4();
        let now = Instant::now();
        assert!(gate.allow(ws, now));
        assert!(
            !gate.allow(ws, now + Duration::from_secs(2)),
            "dentro del cooldown"
        );
        assert!(
            !gate.allow(ws, now + Duration::from_secs(4)),
            "dentro del cooldown"
        );
    }

    #[test]
    fn after_cooldown_a_new_event_is_allowed() {
        let mut gate = NotificationGate::new(NOTIFICATION_COOLDOWN);
        let ws = Uuid::new_v4();
        let now = Instant::now();
        assert!(gate.allow(ws, now));
        assert!(
            gate.allow(ws, now + Duration::from_secs(6)),
            "pasó el cooldown"
        );
    }

    #[test]
    fn distinct_workspaces_are_independent() {
        let mut gate = NotificationGate::new(NOTIFICATION_COOLDOWN);
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let now = Instant::now();
        assert!(gate.allow(a, now));
        assert!(gate.allow(b, now), "otro workspace no comparte el cooldown");
    }
}
