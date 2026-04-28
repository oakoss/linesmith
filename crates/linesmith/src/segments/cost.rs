//! Cost segment: renders session cost in USD. Hidden when the payload
//! doesn't carry cost metrics (currently always present in Claude Code).

use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;
use crate::theme::Role;

pub struct CostSegment;

/// Highest droppable priority in the built-in set: cost is useful but
/// least time-sensitive, so it yields first under width pressure.
const PRIORITY: u8 = 192;

impl Segment for CostSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(cost) = ctx.status.cost.as_ref() else {
            crate::lsm_debug!("cost: status.cost absent; hiding");
            return Ok(None);
        };
        Ok(Some(
            RenderedSegment::new(format!("${:.2}", cost.total_cost_usd)).with_role(Role::Muted),
        ))
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

    fn rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn ctx(cost: Option<CostMetrics>) -> DataContext {
        DataContext::new(StatusContext {
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
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            raw: Arc::new(serde_json::Value::Null),
        })
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
    fn renders_two_decimal_places_with_muted_role() {
        assert_eq!(
            CostSegment
                .render(&ctx(Some(cost_of(1.234))), &rc())
                .unwrap(),
            Some(RenderedSegment::new("$1.23").with_role(Role::Muted))
        );
    }

    #[test]
    fn renders_zero_cost() {
        assert_eq!(
            CostSegment.render(&ctx(Some(cost_of(0.0))), &rc()).unwrap(),
            Some(RenderedSegment::new("$0.00").with_role(Role::Muted))
        );
    }

    #[test]
    fn hidden_when_cost_absent() {
        assert_eq!(CostSegment.render(&ctx(None), &rc()).unwrap(), None);
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(CostSegment.defaults().priority, PRIORITY);
    }
}
