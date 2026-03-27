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
