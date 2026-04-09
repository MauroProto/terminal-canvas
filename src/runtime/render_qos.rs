#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderInputs {
    pub visible: bool,
    pub focused: bool,
    pub screen_area: f32,
    pub streaming: bool,
    pub fast_path: bool,
    pub renderable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTier {
    Full,
    ReducedLive,
    Preview,
    Hidden,
}

pub struct RenderQos;

impl RenderQos {
    pub fn decide(inputs: RenderInputs) -> RenderTier {
        if !inputs.visible || !inputs.renderable || inputs.screen_area <= 0.0 {
            return RenderTier::Hidden;
        }
        if inputs.fast_path {
            return RenderTier::Preview;
        }
        if inputs.focused {
            return RenderTier::Full;
        }
        if inputs.streaming {
            return RenderTier::Preview;
        }
        if inputs.screen_area >= 10_000.0 {
            return RenderTier::ReducedLive;
        }
        RenderTier::Preview
    }
}

#[cfg(test)]
mod tests {
    use super::{RenderInputs, RenderQos, RenderTier};

    #[test]
    fn focused_renderable_panel_gets_full_tier() {
        assert_eq!(
            RenderQos::decide(RenderInputs {
                visible: true,
                focused: true,
                screen_area: 24_000.0,
                streaming: false,
                fast_path: false,
                renderable: true,
            }),
            RenderTier::Full
        );
    }

    #[test]
    fn large_background_panel_gets_reduced_live_tier() {
        assert_eq!(
            RenderQos::decide(RenderInputs {
                visible: true,
                focused: false,
                screen_area: 24_000.0,
                streaming: false,
                fast_path: false,
                renderable: true,
            }),
            RenderTier::ReducedLive
        );
    }

    #[test]
    fn fast_path_for_background_panel_drops_to_preview() {
        assert_eq!(
            RenderQos::decide(RenderInputs {
                visible: true,
                focused: false,
                screen_area: 24_000.0,
                streaming: false,
                fast_path: true,
                renderable: true,
            }),
            RenderTier::Preview
        );
    }

    #[test]
    fn non_renderable_panel_is_hidden() {
        assert_eq!(
            RenderQos::decide(RenderInputs {
                visible: true,
                focused: false,
                screen_area: 24_000.0,
                streaming: false,
                fast_path: false,
                renderable: false,
            }),
            RenderTier::Hidden
        );
    }
}
