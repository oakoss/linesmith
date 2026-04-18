//! Effort segment: renders the current `/effort` level. Hidden when the
//! payload doesn't carry it.
//!
//! Claude Code does not currently re-emit the effort level when the user
//! runs `/effort max` or similar
//! ([ccstatusline#239](https://github.com/sirmalloc/ccstatusline/issues/239)).
//! Until that ships upstream, this segment will be hidden in practice.
//! Segment is shipped now so it lights up automatically when the payload
//! arrives.

use super::{RenderedSegment, Segment, SegmentDefaults};
use crate::input::StatusContext;

pub struct EffortSegment;

/// Between rate-limit (96) and cost (192): informational; drops before
/// cost but after the time-sensitive health metrics.
const PRIORITY: u8 = 160;

impl Segment for EffortSegment {
    fn render(&self, ctx: &StatusContext) -> Option<RenderedSegment> {
        let effort = ctx.effort?;
        Some(RenderedSegment::new(effort.as_str()))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{EffortLevel, ModelInfo, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn ctx(effort: Option<EffortLevel>) -> StatusContext {
        StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "X".into(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            },
            context_window: None,
            cost: None,
            rate_limits: None,
            effort,
            raw: Arc::new(serde_json::Value::Null),
        }
    }

    #[test]
    fn renders_each_level() {
        for (level, expected) in [
            (EffortLevel::Low, "low"),
            (EffortLevel::Medium, "medium"),
            (EffortLevel::High, "high"),
            (EffortLevel::Max, "max"),
            (EffortLevel::XHigh, "xhigh"),
        ] {
            assert_eq!(
                EffortSegment.render(&ctx(Some(level))),
                Some(RenderedSegment::new(expected))
            );
        }
    }

    #[test]
    fn hidden_when_effort_absent() {
        assert_eq!(EffortSegment.render(&ctx(None)), None);
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(EffortSegment.defaults().priority, PRIORITY);
    }
}
