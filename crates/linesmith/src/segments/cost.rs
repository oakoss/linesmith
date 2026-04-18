//! Cost segment: renders session cost in USD. Hidden when the payload
//! doesn't carry cost metrics (currently always present in Claude Code).

use super::{RenderedSegment, Segment, SegmentDefaults};
use crate::input::StatusContext;

pub struct CostSegment;

/// Highest droppable priority in the built-in set: cost is useful but
/// least time-sensitive, so it yields first under width pressure.
const PRIORITY: u8 = 192;

impl Segment for CostSegment {
    fn render(&self, ctx: &StatusContext) -> Option<RenderedSegment> {
        let cost = ctx.cost.as_ref()?;
        Some(RenderedSegment::new(format!("${:.2}", cost.total_cost_usd)))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{CostMetrics, ModelInfo, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn ctx(cost: Option<CostMetrics>) -> StatusContext {
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
            cost,
            rate_limits: None,
            effort: None,
            raw: Arc::new(serde_json::Value::Null),
        }
    }

    fn cost_of(usd: f64) -> CostMetrics {
        CostMetrics {
            total_cost_usd: usd,
            total_duration_ms: 0,
            total_api_duration_ms: 0,
            total_lines_added: 0,
            total_lines_removed: 0,
        }
    }

    #[test]
    fn renders_two_decimal_places() {
        assert_eq!(
            CostSegment.render(&ctx(Some(cost_of(1.234)))),
            Some(RenderedSegment::new("$1.23"))
        );
    }

    #[test]
    fn renders_zero_cost() {
        assert_eq!(
            CostSegment.render(&ctx(Some(cost_of(0.0)))),
            Some(RenderedSegment::new("$0.00"))
        );
    }

    #[test]
    fn hidden_when_cost_absent() {
        assert_eq!(CostSegment.render(&ctx(None)), None);
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(CostSegment.defaults().priority, PRIORITY);
    }
}
