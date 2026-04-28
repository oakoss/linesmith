//! 5-hour window reset countdown. Hidden when `resets_at` is missing
//! or in the past (spec §Edge cases — stale data that cache TTL will
//! refresh on the next render).

use super::rate_limit_5h::PRIORITY;
use std::collections::BTreeMap;

use super::rate_limit_format::{
    apply_common_extras, format_duration, parse_bool, parse_duration_format, render_error,
    CommonRateLimitConfig, DurationFormat, ResetWindow,
};
use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::{DataContext, DataDep, UsageData};
use crate::theme::Role;

#[non_exhaustive]
pub struct RateLimit5hResetSegment {
    pub format: DurationFormat,
    pub compact: bool,
    pub use_days: bool,
    pub config: CommonRateLimitConfig,
}

impl Default for RateLimit5hResetSegment {
    fn default() -> Self {
        Self {
            format: DurationFormat::Duration,
            compact: false,
            use_days: true,
            config: CommonRateLimitConfig::new("5h reset"),
        }
    }
}

impl RateLimit5hResetSegment {
    #[must_use]
    pub fn from_extras(
        extras: &BTreeMap<String, toml::Value>,
        warn: &mut impl FnMut(&str),
    ) -> Self {
        let mut seg = Self::default();
        apply_common_extras(&mut seg.config, extras, "rate_limit_5h_reset", warn);
        if let Some(f) = parse_duration_format(extras, "rate_limit_5h_reset", warn) {
            seg.format = f;
        }
        if let Some(b) = parse_bool(extras, "compact", "rate_limit_5h_reset", warn) {
            seg.compact = b;
        }
        if let Some(b) = parse_bool(extras, "use_days", "rate_limit_5h_reset", warn) {
            seg.use_days = b;
        }
        if seg.config.invalid_progress_width {
            seg.format = DurationFormat::Duration;
        }
        seg
    }
}

impl Segment for RateLimit5hResetSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let usage = ctx.usage();
        let text = match &*usage {
            Ok(data) => {
                let (resets_at, jsonl) = match data {
                    UsageData::Endpoint(e) => {
                        let Some(bucket) = e.five_hour.as_ref() else {
                            crate::lsm_debug!(
                                "rate_limit_5h_reset: endpoint usage.five_hour absent; hiding"
                            );
                            return Ok(None);
                        };
                        let Some(resets_at) = bucket.resets_at else {
                            crate::lsm_debug!(
                                "rate_limit_5h_reset: five_hour.resets_at absent; hiding"
                            );
                            return Ok(None);
                        };
                        (resets_at, false)
                    }
                    UsageData::Jsonl(j) => {
                        // Spec §JSONL-fallback display: derive the
                        // reset timestamp from the active block's
                        // `ends_at` (= block.start + 5h). Hide when no
                        // active block — same hide rule as endpoint.
                        let Some(window) = j.five_hour.as_ref() else {
                            crate::lsm_debug!(
                                "rate_limit_5h_reset: jsonl five_hour block inactive; hiding"
                            );
                            return Ok(None);
                        };
                        (window.ends_at(), true)
                    }
                };
                let remaining = resets_at.signed_duration_since(chrono::Utc::now());
                if remaining <= chrono::Duration::zero() {
                    crate::lsm_debug!(
                        "rate_limit_5h_reset: resets_at in the past ({resets_at}); hiding"
                    );
                    return Ok(None);
                }
                format_duration(
                    remaining,
                    self.format,
                    self.compact,
                    self.use_days,
                    ResetWindow::FiveHour,
                    jsonl,
                    &self.config,
                )
            }
            Err(err) => render_error(err, &self.config),
        };
        Ok(Some(RenderedSegment::new(text).with_role(Role::Info)))
    }

    fn data_deps(&self) -> &'static [DataDep] {
        &[DataDep::Usage]
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_context::{
        EndpointUsage, FiveHourWindow, JsonlUsage, SevenDayWindow, TokenCounts, UsageBucket,
        UsageData, UsageError,
    };
    use crate::input::{ModelInfo, Percent, StatusContext, Tool, WorkspaceInfo};
    use chrono::Duration;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn ctx_with_usage(usage: Result<UsageData, UsageError>) -> DataContext {
        let dc = DataContext::new(StatusContext {
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
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            version: None,
            raw: Arc::new(serde_json::Value::Null),
        });
        dc.preseed_usage(usage).expect("seed");
        dc
    }

    fn data_with_reset_in(minutes: i64) -> UsageData {
        // 30s of slack so clock drift between setup and render doesn't
        // round `num_minutes()` down a boundary (e.g. 277m → 276m), which
        // would flip `"4hr 37m"` to `"4hr 36m"`.
        let slack = if minutes > 0 {
            Duration::seconds(30)
        } else {
            Duration::zero()
        };
        UsageData::Endpoint(EndpointUsage {
            five_hour: Some(UsageBucket {
                utilization: Percent::new(42.0).unwrap(),
                resets_at: Some(chrono::Utc::now() + Duration::minutes(minutes) + slack),
            }),
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: std::collections::HashMap::new(),
        })
    }

    fn jsonl_data_with_reset_in(minutes: i64) -> UsageData {
        let slack = if minutes > 0 {
            Duration::seconds(30)
        } else {
            Duration::zero()
        };
        // Block must start 5h before the desired reset because
        // `FiveHourWindow::ends_at()` is derived as `start + 5h`.
        let start = chrono::Utc::now() + Duration::minutes(minutes) + slack - Duration::hours(5);
        UsageData::Jsonl(JsonlUsage::new(
            Some(FiveHourWindow::new(TokenCounts::default(), start)),
            SevenDayWindow::new(TokenCounts::default()),
        ))
    }

    #[test]
    fn renders_countdown_in_default_format() {
        let dc = ctx_with_usage(Ok(data_with_reset_in(4 * 60 + 37)));
        let rendered = RateLimit5hResetSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "5h reset: 4hr 37m");
    }

    #[test]
    fn hidden_when_resets_at_in_past() {
        let dc = ctx_with_usage(Ok(data_with_reset_in(-10)));
        assert_eq!(
            RateLimit5hResetSegment::default()
                .render(&dc, &rc())
                .unwrap(),
            None
        );
    }

    #[test]
    fn hidden_when_resets_at_missing() {
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: Some(UsageBucket {
                utilization: Percent::new(42.0).unwrap(),
                resets_at: None,
            }),
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: std::collections::HashMap::new(),
        });
        assert_eq!(
            RateLimit5hResetSegment::default()
                .render(&ctx_with_usage(Ok(data)), &rc())
                .unwrap(),
            None,
        );
    }

    #[test]
    fn hidden_when_five_hour_bucket_absent() {
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: std::collections::HashMap::new(),
        });
        assert_eq!(
            RateLimit5hResetSegment::default()
                .render(&ctx_with_usage(Ok(data)), &rc())
                .unwrap(),
            None,
        );
    }

    #[test]
    fn compact_format_drops_suffix_spaces() {
        let seg = RateLimit5hResetSegment {
            compact: true,
            ..Default::default()
        };
        let dc = ctx_with_usage(Ok(data_with_reset_in(4 * 60 + 37)));
        let rendered = seg.render(&dc, &rc()).unwrap().expect("visible");
        assert_eq!(rendered.text(), "5h reset: 4h37m");
    }

    #[test]
    fn renders_error_when_usage_fails() {
        let dc = ctx_with_usage(Err(UsageError::Timeout));
        let rendered = RateLimit5hResetSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "5h reset: [Timeout]");
    }

    #[test]
    fn progress_format_divides_by_five_hour_window_not_seven_day() {
        // Regression guard matching the 7d_reset version: a 5h reset
        // at 30m remaining must render ~90% elapsed against the 5h
        // window, not ~0.3% against a 7d window.
        let dc = ctx_with_usage(Ok(data_with_reset_in(30)));
        let seg = RateLimit5hResetSegment {
            format: DurationFormat::Progress,
            ..Default::default()
        };
        let rendered = seg.render(&dc, &rc()).unwrap().expect("visible");
        let pct_str = rendered
            .text()
            .rsplit(' ')
            .next()
            .expect("percent suffix")
            .trim_end_matches('%');
        let pct: f64 = pct_str.parse().expect("numeric percent");
        assert!(
            (88.0..=92.0).contains(&pct),
            "expected ~90% elapsed, got {pct}% from {:?}",
            rendered.text(),
        );
    }

    #[test]
    fn jsonl_mode_derives_reset_from_five_hour_window_ends_at() {
        // Spec §JSONL-fallback display: reset timestamp derives from
        // `FiveHourWindow.ends_at` (= block.start + 5h) rather than
        // the endpoint's `resets_at`.
        let dc = ctx_with_usage(Ok(jsonl_data_with_reset_in(4 * 60 + 37)));
        let rendered = RateLimit5hResetSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "~5h reset: 4hr 37m");
    }

    #[test]
    fn jsonl_mode_hides_when_block_inactive() {
        let dc = ctx_with_usage(Ok(UsageData::Jsonl(JsonlUsage::new(
            None,
            SevenDayWindow::new(TokenCounts::default()),
        ))));
        assert_eq!(
            RateLimit5hResetSegment::default()
                .render(&dc, &rc())
                .unwrap(),
            None,
        );
    }

    #[test]
    fn jsonl_mode_hides_when_ends_at_in_past() {
        // The aggregator's active-block invariant should prevent this
        // (compute_active_block hides blocks whose last activity is
        // >5h old), but the segment's `remaining <= 0` gate must also
        // apply to the JSONL branch as defense in depth. Regression
        // this catches: a future aggregator loosening that lets stale
        // blocks through must not render "0m" or a negative duration.
        let dc = ctx_with_usage(Ok(jsonl_data_with_reset_in(-10)));
        assert_eq!(
            RateLimit5hResetSegment::default()
                .render(&dc, &rc())
                .unwrap(),
            None,
        );
    }

    #[test]
    fn declares_usage_as_its_only_data_dep() {
        assert_eq!(
            RateLimit5hResetSegment::default().data_deps(),
            &[DataDep::Usage],
        );
    }

    #[test]
    fn from_extras_applies_duration_format_knobs() {
        let mut extras = std::collections::BTreeMap::new();
        extras.insert("format".into(), toml::Value::String("duration".into()));
        extras.insert("compact".into(), toml::Value::Boolean(true));
        extras.insert("use_days".into(), toml::Value::Boolean(false));
        let mut warnings = Vec::new();
        let seg =
            RateLimit5hResetSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(seg.format, DurationFormat::Duration);
        assert!(seg.compact);
        assert!(!seg.use_days);
    }

    #[test]
    fn from_extras_warns_on_percent_format_string() {
        // `format = "percent"` is valid for utilization segments but
        // NOT for reset segments; parse_duration_format rejects it.
        let mut extras = std::collections::BTreeMap::new();
        extras.insert("format".into(), toml::Value::String("percent".into()));
        let mut warnings = Vec::new();
        let _ =
            RateLimit5hResetSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("format"), "{:?}", warnings[0]);
    }
}
