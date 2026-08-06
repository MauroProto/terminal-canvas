//! Flow control del lector de PTY (P3.16, T1).
//!
//! Si la app no drena el buffer pendiente de una sesión (por ejemplo, un
//! `yes` escupiendo megabytes mientras el autosave todavía no corrió), el
//! lector **deja de leer del fd**: el kernel llena el pipe y bloquea al hijo,
//! que es exactamente la contrapresión que queremos. Se reanuda con histéresis
//! (bajar a 32 KB, no apenas se afloja de 256 KB) para no oscilar.
//!
//! Failsafe: nunca más de 5 s en pausa. Un resume perdido (porque la app se
//! colgó, o el drenado se atrasó) no puede dejar un shell colgado para siempre.

use std::time::{Duration, Instant};

/// A partir de acá se corta la lectura.
pub const HIGH_WATER: usize = 256 * 1024;
/// Recién acá se reanuda (histéresis).
pub const LOW_WATER: usize = 32 * 1024;
/// Tope duro de tiempo en pausa.
pub const MAX_PAUSE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowState {
    /// Seguir leyendo del fd.
    Read,
    /// No leer: el buffer pendiente está muy grande.
    Pause,
}

/// Máquina de estados del gate. Pura: recibe el tamaño pendiente y el reloj.
#[derive(Debug, Default)]
pub struct FlowGate {
    paused_since: Option<Instant>,
}

impl FlowGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// ¿Está en pausa ahora mismo?
    pub fn is_paused(&self) -> bool {
        self.paused_since.is_some()
    }

    /// Decide si hay que leer o pausar, dado el buffer pendiente y el reloj.
    pub fn update(&mut self, pending_bytes: usize, now: Instant) -> FlowState {
        match self.paused_since {
            None => {
                if pending_bytes >= HIGH_WATER {
                    self.paused_since = Some(now);
                    FlowState::Pause
                } else {
                    FlowState::Read
                }
            }
            Some(since) => {
                // Histéresis: se reanuda al bajar del low water, no del high.
                if pending_bytes <= LOW_WATER {
                    self.paused_since = None;
                    return FlowState::Read;
                }
                // Failsafe: pasado el tope, se reanuda igual. Se limpia el
                // sello para que pueda volver a pausar si sigue desbordado.
                if now.duration_since(since) >= MAX_PAUSE {
                    self.paused_since = None;
                    return FlowState::Read;
                }
                FlowState::Pause
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FlowGate, FlowState, HIGH_WATER, LOW_WATER, MAX_PAUSE};
    use std::time::{Duration, Instant};

    #[test]
    fn reads_while_the_buffer_is_small() {
        let mut gate = FlowGate::new();
        let now = Instant::now();
        assert_eq!(gate.update(0, now), FlowState::Read);
        assert_eq!(gate.update(LOW_WATER, now), FlowState::Read);
        assert_eq!(gate.update(HIGH_WATER - 1, now), FlowState::Read);
        assert!(!gate.is_paused());
    }

    #[test]
    fn pauses_at_the_high_water_mark() {
        let mut gate = FlowGate::new();
        let now = Instant::now();
        assert_eq!(gate.update(HIGH_WATER, now), FlowState::Pause);
        assert!(gate.is_paused());
    }

    #[test]
    fn hysteresis_keeps_it_paused_between_the_two_marks() {
        let mut gate = FlowGate::new();
        let now = Instant::now();
        gate.update(HIGH_WATER, now);
        // Aflojó del high water pero sigue muy por encima del low: no reanuda.
        assert_eq!(gate.update(HIGH_WATER - 1, now), FlowState::Pause);
        assert_eq!(gate.update(LOW_WATER + 1, now), FlowState::Pause);
    }

    #[test]
    fn resumes_at_the_low_water_mark() {
        let mut gate = FlowGate::new();
        let now = Instant::now();
        gate.update(HIGH_WATER, now);
        assert_eq!(gate.update(LOW_WATER, now), FlowState::Read);
        assert!(!gate.is_paused());
    }

    #[test]
    fn the_failsafe_resumes_even_if_the_buffer_never_drains() {
        let mut gate = FlowGate::new();
        let start = Instant::now();
        gate.update(HIGH_WATER * 4, start);
        // Sigue desbordado, pero pasó el tope de tiempo: se lee igual.
        assert_eq!(
            gate.update(HIGH_WATER * 4, start + MAX_PAUSE),
            FlowState::Read,
            "un resume perdido no puede colgar el shell"
        );
    }

    #[test]
    fn it_can_pause_again_after_the_failsafe_resume() {
        let mut gate = FlowGate::new();
        let start = Instant::now();
        gate.update(HIGH_WATER * 4, start);
        let after = start + MAX_PAUSE;
        assert_eq!(gate.update(HIGH_WATER * 4, after), FlowState::Read);
        // Vuelve a pausar en el siguiente tick si sigue desbordado.
        assert_eq!(gate.update(HIGH_WATER * 4, after), FlowState::Pause);
    }

    #[test]
    fn a_writer_faster_than_the_drain_gets_throttled_but_never_stuck() {
        // Generador que escribe más rápido de lo que se drena: el gate tiene
        // que pausar, pero el failsafe garantiza que siempre vuelve a leer.
        let mut gate = FlowGate::new();
        let mut pending: usize = 0;
        let mut now = Instant::now();
        let mut pauses = 0;
        let mut reads = 0;
        for _ in 0..200 {
            match gate.update(pending, now) {
                FlowState::Read => {
                    reads += 1;
                    pending += 64 * 1024; // el hijo escribe
                }
                FlowState::Pause => {
                    pauses += 1;
                    // La app drena de a poco (más lento que la escritura).
                    pending = pending.saturating_sub(8 * 1024);
                }
            }
            now += Duration::from_millis(100);
        }
        assert!(pauses > 0, "tiene que haber contrapresión");
        assert!(reads > 1, "nunca puede quedar pausado para siempre");
    }
}
