//! Combined rate-limit segment: renders one window or both, joined with
//! `|` when both are present. Degrades gracefully on each `RateLimits`
//! variant so users can pick this segment without branching on tier.
//!
//! Renders a self-contained combined string today; sub-composition via
//! `Segment::children()` is the eventual shape (see
//! `docs/specs/segment-system.md`).

use super::{format_window, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::input::{RateLimits, StatusContext};

pub struct RateLimitSegment;

/// Between model (64) and effort (160). Rate-limit visibility is highly
/// demanded, but the data is cached/delayed so it yields before the
/// live-health metrics.
pub(crate) const PRIORITY: u8 = 96;

impl Segment for RateLimitSegment {
    fn render(&self, ctx: &StatusContext) -> RenderResult {
        let Some(rl) = ctx.rate_limits.as_ref() else {
            return Ok(None);
        };
        let now = chrono::Utc::now();
        let text = match rl {
            RateLimits::FiveHourOnly(w) => format_window("5h", w, now),
            RateLimits::SevenDayOnly(w) => format_window("7d", w, now),
            RateLimits::Both {
                five_hour,
                seven_day,
            } => format!(
                "{} | {}",
                format_window("5h", five_hour, now),
                format_window("7d", seven_day, now)
            ),
        };
        Ok(Some(RenderedSegment::new(text)))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
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
        RateLimitSegment.render(&ctx(rl)).expect("render ok")
    }

    #[test]
    fn hidden_when_rate_limits_absent() {
        assert_eq!(render(None), None);
    }

    #[test]
    fn renders_single_window_for_five_hour_only() {
        let rl = RateLimits::FiveHourOnly(window(42.0, 60));
        let rendered = render(Some(rl)).expect("rendered");
        assert!(rendered.text.starts_with("5h 42%"));
        assert!(!rendered.text.contains('|'));
    }

    #[test]
    fn renders_single_window_for_seven_day_only() {
        let rl = RateLimits::SevenDayOnly(window(7.0, 60 * 24));
        let rendered = render(Some(rl)).expect("rendered");
        assert!(rendered.text.starts_with("7d 7%"));
        assert!(!rendered.text.contains('|'));
    }

    #[test]
    fn renders_both_windows_joined_with_pipe() {
        let rl = RateLimits::Both {
            five_hour: window(45.0, 30),
            seven_day: window(10.0, 60 * 24 * 4),
        };
        let rendered = render(Some(rl)).expect("rendered");
        assert!(rendered.text.contains("5h 45%"));
        assert!(rendered.text.contains("7d 10%"));
        assert!(rendered.text.contains(" | "));
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(RateLimitSegment.defaults().priority, PRIORITY);
    }
}
