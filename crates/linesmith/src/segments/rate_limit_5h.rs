//! 5-hour rate-limit segment: renders `5h {pct}% · {countdown}` when the
//! session tier exposes a 5-hour window. Hidden for API-tier users and
//! Pro/Max sessions that only surface the 7-day window.

use super::{format_window, rate_limit, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;
use crate::theme::Role;

pub struct RateLimit5hSegment;

impl Segment for RateLimit5hSegment {
    fn render(&self, ctx: &DataContext) -> RenderResult {
        let Some(window) = ctx
            .status
            .rate_limits
            .as_ref()
            .and_then(|rl| rl.five_hour())
        else {
            return Ok(None);
        };
        Ok(Some(
            RenderedSegment::new(format_window("5h", window, chrono::Utc::now()))
                .with_role(Role::Info),
        ))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(rate_limit::PRIORITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{
        ModelInfo, Percent, RateLimitWindow, RateLimits, StatusContext, Tool, WorkspaceInfo,
    };
    use chrono::{Duration, Utc};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn ctx(rate_limits: Option<RateLimits>) -> DataContext {
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
            cost: None,
            rate_limits,
            effort: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    fn window(used: f32, minutes_from_now: i64) -> RateLimitWindow {
        RateLimitWindow {
            used: Percent::new(used).expect("in range"),
            resets_at: Utc::now() + Duration::minutes(minutes_from_now),
        }
    }

    fn render(rl: Option<RateLimits>) -> Option<RenderedSegment> {
        RateLimit5hSegment.render(&ctx(rl)).expect("render ok")
    }

    #[test]
    fn hidden_when_rate_limits_absent() {
        assert_eq!(render(None), None);
    }

    #[test]
    fn hidden_when_only_seven_day_window_present() {
        let rl = RateLimits::SevenDayOnly(window(5.0, 60));
        assert_eq!(render(Some(rl)), None);
    }

    #[test]
    fn renders_five_hour_only_variant() {
        let rl = RateLimits::FiveHourOnly(window(42.0, 73)); // ~1h 13m from now
        let rendered = render(Some(rl)).expect("rendered");
        assert!(
            rendered.text.starts_with("5h 42%"),
            "got {:?}",
            rendered.text
        );
        assert!(rendered.text.contains("1h"));
    }

    #[test]
    fn renders_both_variant_picking_five_hour_window() {
        let rl = RateLimits::Both {
            five_hour: window(67.0, 30),
            seven_day: window(5.0, 60 * 24),
        };
        let rendered = render(Some(rl)).expect("rendered");
        assert!(
            rendered.text.starts_with("5h 67%"),
            "got {:?}",
            rendered.text
        );
    }

    #[test]
    fn defaults_match_combined_rate_limit_priority() {
        assert_eq!(RateLimit5hSegment.defaults().priority, rate_limit::PRIORITY);
    }
}
