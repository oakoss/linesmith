//! Segment trait. Current shape is `render` only; see
//! `docs/specs/segment-system.md` for the full trait (layout intent,
//! cache policy, sub-composition) that grows as segments mature.

use crate::input::StatusContext;

pub mod workspace;

/// Output of a successful segment render. Carries only `text` today;
/// width hints, styled runs, and per-segment separator preferences
/// are added per `docs/specs/segment-system.md`. `#[non_exhaustive]`
/// keeps those additions SemVer-compatible.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderedSegment {
    pub text: String,
}

impl RenderedSegment {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

pub trait Segment: Send {
    /// Render this segment for the given context, or `None` to hide.
    #[must_use]
    fn render(&self, ctx: &StatusContext) -> Option<RenderedSegment>;
}
