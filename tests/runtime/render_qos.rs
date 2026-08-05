mod terminal {
    #![allow(dead_code)]

    #[path = "../../../src/terminal/agent_status.rs"]
    pub mod agent_status;
    #[path = "../../../src/terminal/backend.rs"]
    pub mod backend;
    #[path = "../../../src/terminal/colors.rs"]
    pub mod colors;
    #[cfg(feature = "ghostty-vt")]
    #[path = "../../../src/terminal/ghostty_backend.rs"]
    pub mod ghostty_backend;
    #[path = "../../../src/terminal/input.rs"]
    pub mod input;
    #[path = "../../../src/terminal/metrics.rs"]
    pub mod metrics;
    #[path = "../../../src/terminal/pty.rs"]
    pub mod pty;
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

use runtime::{RenderInputs, RenderQos, RenderTier};

#[test]
fn qos_gives_full_render_to_focused_terminal() {
    let qos = RenderQos::decide(RenderInputs {
        visible: true,
        focused: true,
        screen_area: 120_000.0,
        streaming: false,
        fast_path: false,
        renderable: true,
    });

    assert_eq!(qos, RenderTier::Full);
}

#[test]
fn qos_uses_reduced_live_for_visible_small_terminal() {
    let qos = RenderQos::decide(RenderInputs {
        visible: true,
        focused: false,
        screen_area: 24_000.0,
        streaming: false,
        fast_path: false,
        renderable: true,
    });

    assert_eq!(qos, RenderTier::ReducedLive);
}

#[test]
fn qos_hides_offscreen_terminal() {
    let qos = RenderQos::decide(RenderInputs {
        visible: false,
        focused: false,
        screen_area: 80_000.0,
        streaming: false,
        fast_path: false,
        renderable: true,
    });

    assert_eq!(qos, RenderTier::Hidden);
}

#[test]
fn qos_downgrades_background_terminal() {
    let qos = RenderQos::decide(RenderInputs {
        visible: true,
        focused: false,
        screen_area: 12_000.0,
        streaming: true,
        fast_path: false,
        renderable: true,
    });

    assert_eq!(qos, RenderTier::Preview);
}
