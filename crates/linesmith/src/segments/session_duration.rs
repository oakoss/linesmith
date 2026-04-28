//! Session duration segment: renders elapsed session time in compact
//! `Nh Nm` / `Nm Ns` / `Ns` form. The `total_duration_ms` field lives
//! inside the input schema's `cost` block, so the segment hides
//! whenever `cost` is absent.

use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;
use crate::theme::Role;

pub struct SessionDurationSegment;

/// Same bucket as `cost` (192) and the `tokens_*` family: passive
/// metadata that yields first under width pressure. Position order
/// breaks the tie among segments at this priority.
const PRIORITY: u8 = 192;

impl Segment for SessionDurationSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(cost) = ctx.status.cost.as_ref() else {
            crate::lsm_debug!("session_duration: status.cost absent; hiding");
            return Ok(None);
        };
        Ok(Some(
            RenderedSegment::new(format_duration(cost.total_duration_ms)).with_role(Role::Muted),
        ))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

fn format_duration(ms: u64) -> String {
    let total_secs = ms / 1000;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        if m > 0 {
            format!("{h}h {m}m")
        } else {
            format!("{h}h")
        }
    } else if m > 0 {
        if s > 0 {
            format!("{m}m {s}s")
        } else {
            format!("{m}m")
        }
    } else {
        format!("{s}s")
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

    fn cost_of(duration_ms: u64) -> CostMetrics {
        CostMetrics {
            total_cost_usd: 0.0,
            total_duration_ms: duration_ms,
            total_api_duration_ms: 0,
            total_lines_added: 0,
            total_lines_removed: 0,
        }
    }

    fn render_for(duration_ms: u64) -> Option<RenderedSegment> {
        SessionDurationSegment
            .render(&ctx(Some(cost_of(duration_ms))), &rc())
            .unwrap()
    }

    #[test]
    fn renders_zero_seconds() {
        assert_eq!(
            render_for(0),
            Some(RenderedSegment::new("0s").with_role(Role::Muted))
        );
    }

    #[test]
    fn renders_sub_minute_as_seconds_only() {
        assert_eq!(render_for(5_000).unwrap().text(), "5s");
        assert_eq!(render_for(59_999).unwrap().text(), "59s");
    }

    #[test]
    fn renders_exact_minute_without_trailing_zero_seconds() {
        assert_eq!(render_for(60_000).unwrap().text(), "1m");
    }

    #[test]
    fn renders_minutes_and_seconds() {
        assert_eq!(render_for(65_000).unwrap().text(), "1m 5s");
        assert_eq!(render_for(125_500).unwrap().text(), "2m 5s");
    }

    #[test]
    fn renders_just_below_an_hour() {
        // 3_599_999ms is one ms shy of an hour; integer division gives
        // 3599s = 59m 59s, the upper edge of the minute tier.
        assert_eq!(render_for(3_599_999).unwrap().text(), "59m 59s");
    }

    #[test]
    fn renders_exact_hour_without_trailing_zero_minutes() {
        assert_eq!(render_for(3_600_000).unwrap().text(), "1h");
    }

    #[test]
    fn renders_hours_and_minutes_drops_seconds() {
        // 1h 2m 5s — seconds intentionally dropped at the hour tier so
        // the segment stays compact under width pressure.
        assert_eq!(render_for(3_725_000).unwrap().text(), "1h 2m");
    }

    #[test]
    fn renders_long_sessions_at_hour_resolution() {
        assert_eq!(render_for(86_400_000).unwrap().text(), "24h");
        assert_eq!(render_for(91_800_000).unwrap().text(), "25h 30m");
    }

    #[test]
    fn renders_hour_with_seconds_only_drops_them() {
        // 3_630_000ms = 1h 0m 30s. Seconds must drop at the hour tier
        // even when minutes are zero; pulling them back into the
        // m == 0 branch (e.g. `format!("{h}h {s}s")`) would render
        // `"1h 30s"` instead.
        assert_eq!(render_for(3_630_000).unwrap().text(), "1h");
    }

    #[test]
    fn renders_with_muted_role() {
        assert_eq!(render_for(5_000).unwrap().style().role, Some(Role::Muted));
    }

    #[test]
    fn hidden_when_cost_absent() {
        assert_eq!(
            SessionDurationSegment.render(&ctx(None), &rc()).unwrap(),
            None
        );
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(SessionDurationSegment.defaults().priority, PRIORITY);
    }

    #[test]
    fn sub_second_durations_floor_to_zero_seconds() {
        // Rounding-down across the 1-second boundary is the natural
        // behavior of integer division; pin it so a future refactor
        // doesn't switch to nearest-second and produce "1s" for 999ms.
        assert_eq!(render_for(999).unwrap().text(), "0s");
    }
}
