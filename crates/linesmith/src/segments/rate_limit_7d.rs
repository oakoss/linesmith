//! 7-day rate-limit segment: renders `7d {pct}% · {countdown}` when the
//! session tier exposes a 7-day window. Hidden for API-tier users.

use super::{format_window, rate_limit, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::input::StatusContext;
use crate::theme::Role;

pub struct RateLimit7dSegment;

impl Segment for RateLimit7dSegment {
    fn render(&self, ctx: &StatusContext) -> RenderResult {
        let Some(window) = ctx.rate_limits.as_ref().and_then(|rl| rl.seven_day()) else {
            return Ok(None);
        };
        Ok(Some(
            RenderedSegment::new(format_window("7d", window, chrono::Utc::now()))
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

    fn render(rl: Option<RateLimits>) -> Option<RenderedSegment> {
        RateLimit7dSegment.render(&ctx(rl)).expect("render ok")
    }

    #[test]
    fn hidden_when_rate_limits_absent() {
        assert_eq!(render(None), None);
    }

    #[test]
    fn hidden_when_only_five_hour_window_present() {
        let rl = RateLimits::FiveHourOnly(window(5.0, 60));
        assert_eq!(render(Some(rl)), None);
    }

    #[test]
    fn renders_seven_day_only_variant() {
        let rl = RateLimits::SevenDayOnly(window(12.0, 60 * 24 * 3)); // 3 days
        let rendered = render(Some(rl)).expect("rendered");
        assert!(
            rendered.text.starts_with("7d 12%"),
            "got {:?}",
            rendered.text
        );
    }

    #[test]
    fn defaults_match_combined_rate_limit_priority() {
        assert_eq!(RateLimit7dSegment.defaults().priority, rate_limit::PRIORITY);
    }
}
