//! 7-day rate-limit segment: renders `7d {pct}% · {countdown}` when the
//! session tier exposes a 7-day window. Hidden for API-tier users.

use super::{format_window, RenderedSegment, Segment};
use crate::input::StatusContext;

pub struct RateLimit7dSegment;

impl Segment for RateLimit7dSegment {
    fn render(&self, ctx: &StatusContext) -> Option<RenderedSegment> {
        let window = ctx.rate_limits.as_ref()?.seven_day()?;
        Some(RenderedSegment::new(format_window(
            "7d",
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
        assert_eq!(RateLimit7dSegment.render(&ctx(None)), None);
    }

    #[test]
    fn hidden_when_only_five_hour_window_present() {
        let rl = RateLimits::FiveHourOnly(window(5.0, 60));
        assert_eq!(RateLimit7dSegment.render(&ctx(Some(rl))), None);
    }

    #[test]
    fn renders_seven_day_only_variant() {
        let rl = RateLimits::SevenDayOnly(window(12.0, 60 * 24 * 3)); // 3 days
        let rendered = RateLimit7dSegment.render(&ctx(Some(rl))).expect("rendered");
        assert!(
            rendered.text.starts_with("7d 12%"),
            "got {:?}",
            rendered.text
        );
    }
}
