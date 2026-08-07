mod terminal {
    #![allow(dead_code)]

    #[path = "../../../src/terminal/agent_status.rs"]
    pub mod agent_status;
    #[path = "../../../src/terminal/backend.rs"]
    pub mod backend;
    #[path = "../../../src/terminal/colors.rs"]
    pub mod colors;
    #[path = "../../../src/terminal/flow_control.rs"]
    pub mod flow_control;
    #[cfg(feature = "ghostty-vt")]
    #[path = "../../../src/terminal/ghostty_backend.rs"]
    pub mod ghostty_backend;
    #[path = "../../../src/terminal/input.rs"]
    pub mod input;
    #[path = "../../../src/terminal/metrics.rs"]
    pub mod metrics;
    #[path = "../../../src/terminal/pty.rs"]
    pub mod pty;
    #[cfg(all(unix, feature = "daemon"))]
    #[path = "../../../src/terminal/remote_session.rs"]
    pub mod remote_session;
}

#[cfg(all(unix, feature = "daemon"))]
#[path = "../../src/daemon/protocol.rs"]
pub mod daemon_protocol_impl;
#[cfg(all(unix, feature = "daemon"))]
mod daemon {
    #![allow(dead_code)]

    pub use super::daemon_protocol_impl as protocol;
}
#[path = "../../src/state/durable_write.rs"]
pub mod durable_write_impl;
#[path = "../../src/state/scrollback_log.rs"]
pub mod scrollback_log_impl;
#[path = "../../src/state/scrollback_store.rs"]
pub mod scrollback_store_impl;

mod state {
    #![allow(dead_code)]

    pub use super::durable_write_impl as durable_write;
    pub use super::scrollback_log_impl as scrollback_log;
    pub use super::scrollback_store_impl as scrollback_store;
}

#[allow(dead_code)]
#[path = "../../src/config.rs"]
mod config;

mod utils {
    #![allow(dead_code)]

    #[path = "../../../src/utils/platform.rs"]
    pub mod platform;
}

#[allow(dead_code)]
#[path = "../../src/runtime/mod.rs"]
mod runtime;

#[path = "fixtures/mod.rs"]
mod fixtures;

use fixtures::RuntimeHarness;
use std::time::{Duration, Instant};

#[test]
fn smoke_budget_single_visible_terminal_stays_focused_and_stable() {
    let mut harness = RuntimeHarness::new();
    harness.seed_budget(1, 1, 0);
    harness.emit_output_bursts();
    harness.step();

    assert_eq!(harness.session_count(), 1);
    assert!(harness.no_deadlocks());
    assert!(harness.snapshot_is_consistent());
    assert_eq!(harness.render_tier_counts(), (1, 0, 0, 0));
}

#[test]
fn smoke_budget_four_open_two_visible_one_streaming_uses_preview_and_hidden_tiers() {
    let mut harness = RuntimeHarness::new();
    harness.seed_budget(4, 2, 1);
    harness.emit_output_bursts();
    harness.step();

    assert_eq!(harness.session_count(), 4);
    assert!(harness.no_deadlocks());
    assert!(harness.snapshot_is_consistent());
    assert_eq!(harness.render_tier_counts(), (1, 0, 1, 2));
}

#[test]
fn smoke_budget_twenty_open_six_visible_three_streaming_hits_target_shape() {
    let mut harness = RuntimeHarness::new();
    harness.seed_budget(20, 6, 3);
    harness.emit_output_bursts();
    harness.step();

    assert_eq!(harness.session_count(), 20);
    assert!(harness.no_deadlocks());
    assert!(harness.snapshot_is_consistent());
    assert_eq!(harness.render_tier_counts(), (1, 2, 3, 14));
}

// --- Presupuestos duros (Ship-it 7.3, T1) ---

/// Tope del p95 de un "frame" simulado con 20 sesiones. Es el costo de la
/// coordinación (scheduler + registry + snapshots), no del render con GPU.
const FRAME_P95_BUDGET: Duration = Duration::from_millis(8);

fn percentile(mut samples: Vec<Duration>, percentile: f64) -> Duration {
    samples.sort();
    let index = ((samples.len() as f64 - 1.0) * percentile).round() as usize;
    samples[index.min(samples.len() - 1)]
}

#[test]
fn frame_p95_stays_under_budget_with_twenty_sessions() {
    let mut harness = RuntimeHarness::new();
    harness.seed_budget(20, 6, 3);

    // Calentamiento: la primera pasada paga allocs que no representan el
    // estado estable.
    for _ in 0..10 {
        harness.emit_output_bursts();
        harness.step();
    }

    let mut samples = Vec::with_capacity(200);
    for _ in 0..200 {
        let started = Instant::now();
        harness.emit_output_bursts();
        harness.step();
        samples.push(started.elapsed());
    }

    let p95 = percentile(samples, 0.95);
    assert!(
        p95 < FRAME_P95_BUDGET,
        "frame p95 {p95:?} supera el presupuesto de {FRAME_P95_BUDGET:?}"
    );
}

#[test]
fn an_idle_scheduler_asks_for_no_repaints() {
    // Sin output nuevo, el scheduler no puede pedir repaints: si los pidiera,
    // la app estaría dibujando para siempre sin motivo.
    let mut harness = RuntimeHarness::new();
    harness.seed_budget(20, 6, 3);
    harness.emit_output_bursts();
    harness.step();

    // Un step sin emitir nada tiene que quedar idle en la primera vuelta.
    harness.step();
    assert!(
        harness.is_idle_after_step(),
        "el scheduler pide repaints sin tener nada pendiente"
    );
}

#[test]
fn draining_is_bounded_per_frame() {
    // Aunque llueva output, el drenado por frame está acotado: nunca puede
    // convertirse en un loop que se coma el frame entero.
    let mut harness = RuntimeHarness::new();
    harness.seed_budget(20, 20, 19);
    for _ in 0..50 {
        harness.emit_output_bursts();
    }
    harness.step();
    assert!(
        harness.drained_batches() <= 8,
        "el drenado no está acotado: {} batches",
        harness.drained_batches()
    );
}
