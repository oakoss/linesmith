//! 5-hour rate-limit segment: renders `5h {pct}% · {countdown}` when the
//! session tier exposes a 5-hour window. Hidden for API-tier users and
//! Pro/Max sessions that only surface the 7-day window.

use super::{format_window, RenderedSegment, Segment};
use crate::input::StatusContext;

pub struct RateLimit5hSegment;

impl Segment for RateLimit5hSegment {
    fn render(&self, ctx: &StatusContext) -> Option<RenderedSegment> {
        let window = ctx.rate_limits.as_ref()?.five_hour()?;
        Some(RenderedSegment::new(format_window(
            "5h",
            window,
            chrono::Utc::now(),
        )))
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

    fn ctx(rate_limits: Option<RateLimits>) -> StatusContext {
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
            rate_limits,
            effort: None,
            raw: Arc::new(serde_json::Value::Null),
        }
    }

    fn window(used: f32, minutes_from_now: i64) -> RateLimitWindow {
        RateLimitWindow {
            used: Percent::new(used).expect("in range"),
            resets_at: Utc::now() + Duration::minutes(minutes_from_now),
        }
    }

    #[test]
    fn hidden_when_rate_limits_absent() {
        assert_eq!(RateLimit5hSegment.render(&ctx(None)), None);
    }

    #[test]
    fn hidden_when_only_seven_day_window_present() {
        let rl = RateLimits::SevenDayOnly(window(5.0, 60));
        assert_eq!(RateLimit5hSegment.render(&ctx(Some(rl))), None);
    }

    #[test]
    fn renders_five_hour_only_variant() {
        let rl = RateLimits::FiveHourOnly(window(42.0, 73)); // ~1h 13m from now
        let rendered = RateLimit5hSegment.render(&ctx(Some(rl))).expect("rendered");
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
        let rendered = RateLimit5hSegment.render(&ctx(Some(rl))).expect("rendered");
        assert!(
            rendered.text.starts_with("5h 67%"),
            "got {:?}",
            rendered.text
        );
    }
}
