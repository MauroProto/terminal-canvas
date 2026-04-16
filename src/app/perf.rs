use super::*;

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct FramePerfSnapshot {
    pub(super) frame_time: Duration,
    pub(super) visible_panels: usize,
    pub(super) attached_sessions: usize,
    pub(super) detached_sessions: usize,
    pub(super) full_renders: usize,
    pub(super) reduced_live_renders: usize,
    pub(super) preview_renders: usize,
    pub(super) hidden_renders: usize,
    pub(super) cache_hits: usize,
    pub(super) cache_misses: usize,
    pub(super) runtime_repaint: bool,
}

impl FramePerfSnapshot {
    pub(super) fn note_render(&mut self, tier: Option<RenderTier>, cache_hit: bool) {
        match tier.unwrap_or(RenderTier::Hidden) {
            RenderTier::Full => self.full_renders += 1,
            RenderTier::ReducedLive => self.reduced_live_renders += 1,
            RenderTier::Preview => self.preview_renders += 1,
            RenderTier::Hidden => self.hidden_renders += 1,
        }
        if cache_hit {
            self.cache_hits += 1;
        } else if !matches!(tier, Some(RenderTier::Hidden)) {
            self.cache_misses += 1;
        }
    }
}
