//! 5-hour rate-limit segments: utilization (`RateLimit5hSegment`) and
//! reset countdown (`RateLimit5hResetSegment`). Both read `ctx.usage()`
//! (`Arc<Result<UsageData, UsageError>>`) and route through the
//! `UsageWindow::FiveHour` seam in `super::window`.
//!
//! - Utilization renders a percentage or progress bar from the endpoint
//!   bucket, or the JSONL block's token total under the JSONL fallback.
//!   Hidden when the bucket/block is absent. Spec:
//!   `docs/specs/rate-limit-segments.md` §Render semantics.
//! - Reset countdown renders the time remaining until `resets_at`
//!   (endpoint) or `block.start + 5h` (JSONL fallback). Hidden when
//!   `resets_at` is missing or in the past — spec §Edge cases.

use std::collections::BTreeMap;

use super::config::{
    apply_common_extras, parse_duration_format, parse_percent_format, CommonRateLimitConfig,
    DurationFormat, PercentFormat, PRIORITY,
};
use super::format::{
    format_duration, format_jsonl_tokens, format_percent, render_error, ResetWindow,
};
use super::window::{resolve_five_hour_reset, ResetSource, UsageWindow, WindowResolution};
use crate::data_context::{DataContext, DataDep};
use crate::segments::extras::parse_bool;
use crate::segments::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::theme::Role;

#[non_exhaustive]
pub struct RateLimit5hSegment {
    pub format: PercentFormat,
    pub invert: bool,
    pub config: CommonRateLimitConfig,
}

impl Default for RateLimit5hSegment {
    fn default() -> Self {
        Self {
            format: PercentFormat::Percent,
            invert: false,
            config: CommonRateLimitConfig::new("5h"),
        }
    }
}

impl RateLimit5hSegment {
    /// Build from a `[segments.rate_limit_5h]` TOML extras bag. Keys
    /// absent from `extras` inherit the spec defaults.
    #[must_use]
    pub fn from_extras(
        extras: &BTreeMap<String, toml::Value>,
        warn: &mut impl FnMut(&str),
    ) -> Self {
        let mut seg = Self::default();
        apply_common_extras(&mut seg.config, extras, "rate_limit_5h", warn);
        if let Some(f) = parse_percent_format(extras, "rate_limit_5h", warn) {
            seg.format = f;
        }
        if let Some(b) = parse_bool(extras, "invert", "rate_limit_5h", warn) {
            seg.invert = b;
        }
        // Spec §Edge cases: invalid progress_width falls back to
        // percent format, not just a default-width bar.
        if seg.config.invalid_progress_width {
            seg.format = PercentFormat::Percent;
        }
        seg
    }
}

impl Segment for RateLimit5hSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let usage = ctx.usage();
        let text = match &*usage {
            Ok(data) => match UsageWindow::FiveHour.resolve_percent(data) {
                Ok(WindowResolution::Endpoint(bucket)) => {
                    format_percent(bucket, self.format, self.invert, &self.config)
                }
                Ok(WindowResolution::JsonlTokens(total)) => {
                    format_jsonl_tokens(total, &self.config)
                }
                Err(reason) => {
                    crate::lsm_debug!("rate_limit_5h: {reason}; hiding");
                    return Ok(None);
                }
            },
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
                let (resets_at, jsonl) = match resolve_five_hour_reset(data) {
                    Ok(ResetSource::Endpoint(at)) => (at, false),
                    Ok(ResetSource::JsonlBlockEnd(at)) => (at, true),
                    Err(reason) => {
                        crate::lsm_debug!("rate_limit_5h_reset: {reason}; hiding");
                        return Ok(None);
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
        EndpointUsage, ExtraUsage, FiveHourWindow, JsonlUsage, SevenDayWindow, TokenCounts,
        UsageBucket, UsageData, UsageError,
    };
    use crate::input::{ModelInfo, Percent, StatusContext, Tool, WorkspaceInfo};
    use chrono::{Duration, Utc};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn ctx_with_usage(usage: Result<UsageData, UsageError>) -> DataContext {
        let dc = DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
            model: Some(ModelInfo {
                display_name: "X".into(),
            }),
            workspace: Some(WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            }),
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

    fn endpoint_data_with_five_hour(pct: f32) -> UsageData {
        UsageData::Endpoint(EndpointUsage {
            five_hour: Some(UsageBucket {
                utilization: Percent::new(pct).unwrap(),
                resets_at: None,
            }),
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: std::collections::HashMap::new(),
        })
    }

    fn endpoint_empty() -> UsageData {
        UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: std::collections::HashMap::new(),
        })
    }

    fn jsonl_with_five_hour_tokens(total: u64) -> UsageData {
        let tokens = TokenCounts::from_parts(total, 0, 0, 0);
        // Block start ~1h ago → ends_at() lands ~4h in the future.
        let start = Utc::now() - Duration::hours(1);
        UsageData::Jsonl(JsonlUsage::new(
            Some(FiveHourWindow::new(tokens, start)),
            SevenDayWindow::new(TokenCounts::default()),
        ))
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
                resets_at: Some(Utc::now() + Duration::minutes(minutes) + slack),
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
        let start = Utc::now() + Duration::minutes(minutes) + slack - Duration::hours(5);
        UsageData::Jsonl(JsonlUsage::new(
            Some(FiveHourWindow::new(TokenCounts::default(), start)),
            SevenDayWindow::new(TokenCounts::default()),
        ))
    }

    #[test]
    fn hidden_when_five_hour_bucket_absent() {
        let rendered = RateLimit5hSegment::default()
            .render(&ctx_with_usage(Ok(endpoint_empty())), &rc())
            .expect("render ok");
        assert_eq!(rendered, None);
    }

    #[test]
    fn renders_percent_happy_path() {
        let dc = ctx_with_usage(Ok(endpoint_data_with_five_hour(22.0)));
        let rendered = RateLimit5hSegment::default()
            .render(&dc, &rc())
            .expect("render ok")
            .expect("visible");
        assert_eq!(rendered.text(), "5h: 22.0%");
    }

    #[test]
    fn renders_inverted_percent_when_configured() {
        let dc = ctx_with_usage(Ok(endpoint_data_with_five_hour(22.0)));
        let seg = RateLimit5hSegment {
            invert: true,
            ..Default::default()
        };
        let rendered = seg.render(&dc, &rc()).unwrap().expect("visible");
        assert_eq!(rendered.text(), "5h: 78.0%");
    }

    #[test]
    fn jsonl_mode_renders_compact_tokens_with_stale_marker() {
        let dc = ctx_with_usage(Ok(jsonl_with_five_hour_tokens(420_000)));
        let rendered = RateLimit5hSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "~5h: 420k");
    }

    #[test]
    fn jsonl_mode_hides_when_no_active_block() {
        // Inactive block → aggregator returns None for the 5h window,
        // which matches endpoint's "bucket absent" semantic.
        let dc = ctx_with_usage(Ok(UsageData::Jsonl(JsonlUsage::new(
            None,
            SevenDayWindow::new(TokenCounts::default()),
        ))));
        assert_eq!(
            RateLimit5hSegment::default().render(&dc, &rc()).unwrap(),
            None
        );
    }

    #[test]
    fn jsonl_mode_ignores_invert_and_progress_knobs() {
        // Invert/progress only make sense against a 0-100 axis; under
        // JSONL there's no ceiling so the segment still renders raw
        // tokens with just the stale marker.
        let dc = ctx_with_usage(Ok(jsonl_with_five_hour_tokens(1_200_000)));
        let seg = RateLimit5hSegment {
            format: PercentFormat::Progress,
            invert: true,
            ..Default::default()
        };
        let rendered = seg.render(&dc, &rc()).unwrap().expect("visible");
        assert_eq!(rendered.text(), "~5h: 1.2M");
    }

    #[test]
    fn renders_progress_bar_when_format_is_progress() {
        let dc = ctx_with_usage(Ok(endpoint_data_with_five_hour(50.0)));
        let seg = RateLimit5hSegment {
            format: PercentFormat::Progress,
            ..Default::default()
        };
        let rendered = seg.render(&dc, &rc()).unwrap().expect("visible");
        assert!(rendered.text().starts_with("5h: "), "{}", rendered.text());
        assert!(rendered.text().contains("█"));
        assert!(rendered.text().ends_with("50.0%"), "{}", rendered.text());
    }

    #[test]
    fn progress_bar_at_zero_is_entirely_empty_cells() {
        let dc = ctx_with_usage(Ok(endpoint_data_with_five_hour(0.0)));
        let seg = RateLimit5hSegment {
            format: PercentFormat::Progress,
            ..Default::default()
        };
        let rendered = seg.render(&dc, &rc()).unwrap().expect("visible");
        assert!(rendered.text().contains("░"));
        assert!(!rendered.text().contains("█"));
        assert!(rendered.text().ends_with("0.0%"), "{}", rendered.text());
    }

    #[test]
    fn progress_bar_at_one_hundred_is_entirely_filled_cells() {
        let dc = ctx_with_usage(Ok(endpoint_data_with_five_hour(100.0)));
        let seg = RateLimit5hSegment {
            format: PercentFormat::Progress,
            ..Default::default()
        };
        let rendered = seg.render(&dc, &rc()).unwrap().expect("visible");
        assert!(rendered.text().contains("█"));
        assert!(!rendered.text().contains("░"));
        assert!(rendered.text().ends_with("100.0%"), "{}", rendered.text());
    }

    #[test]
    fn renders_error_table_strings() {
        let dc = ctx_with_usage(Err(UsageError::Timeout));
        let rendered = RateLimit5hSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "5h: [Timeout]");
    }

    #[test]
    fn declares_usage_as_its_only_data_dep() {
        let deps = RateLimit5hSegment::default().data_deps();
        assert_eq!(deps, &[DataDep::Usage]);
    }

    #[test]
    fn from_extras_applies_format_invert_and_common_knobs() {
        let mut extras = std::collections::BTreeMap::new();
        extras.insert("format".into(), toml::Value::String("progress".into()));
        extras.insert("invert".into(), toml::Value::Boolean(true));
        extras.insert("label".into(), toml::Value::String("five".into()));
        extras.insert("icon".into(), toml::Value::String("⏱".into()));
        extras.insert("stale_marker".into(), toml::Value::String("*".into()));
        extras.insert("progress_width".into(), toml::Value::Integer(10));
        let mut warnings = Vec::new();
        let seg = RateLimit5hSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(seg.format, PercentFormat::Progress);
        assert!(seg.invert);
        assert_eq!(seg.config.label, "five");
        assert_eq!(seg.config.icon, "⏱");
        assert_eq!(seg.config.stale_marker, "*");
        assert_eq!(seg.config.progress_width, 10);
    }

    #[test]
    fn from_extras_flips_progress_to_percent_on_invalid_width() {
        // Spec §Edge cases: invalid `progress_width` warns AND forces
        // fallback to percent format; a silent default-width bar
        // would contradict the user's stated intent.
        let mut extras = std::collections::BTreeMap::new();
        extras.insert("format".into(), toml::Value::String("progress".into()));
        extras.insert("progress_width".into(), toml::Value::Integer(0));
        let mut warnings = Vec::new();
        let seg = RateLimit5hSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("progress_width"), "{:?}", warnings[0]);
        assert_eq!(seg.format, PercentFormat::Percent);
    }

    #[test]
    fn from_extras_warns_on_bad_format_string() {
        let mut extras = std::collections::BTreeMap::new();
        extras.insert("format".into(), toml::Value::String("bogus".into()));
        let mut warnings = Vec::new();
        let seg = RateLimit5hSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("format"), "{:?}", warnings[0]);
        assert_eq!(seg.format, PercentFormat::Percent);
    }

    #[test]
    fn does_not_read_extra_usage_field() {
        // Regression guard: the 5h segment must not accidentally
        // render extra_usage state when its own bucket is absent.
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: Some(ExtraUsage {
                is_enabled: Some(true),
                utilization: Some(Percent::new(50.0).unwrap()),
                monthly_limit: Some(100.0),
                used_credits: Some(50.0),
                currency: Some("USD".into()),
            }),
            unknown_buckets: std::collections::HashMap::new(),
        });
        let rendered = RateLimit5hSegment::default()
            .render(&ctx_with_usage(Ok(data)), &rc())
            .unwrap();
        assert_eq!(rendered, None);
    }

    // ---- RateLimit5hResetSegment tests ----

    #[test]
    fn reset_renders_countdown_in_default_format() {
        let dc = ctx_with_usage(Ok(data_with_reset_in(4 * 60 + 37)));
        let rendered = RateLimit5hResetSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "5h reset: 4hr 37m");
    }

    #[test]
    fn reset_hidden_when_resets_at_in_past() {
        let dc = ctx_with_usage(Ok(data_with_reset_in(-10)));
        assert_eq!(
            RateLimit5hResetSegment::default()
                .render(&dc, &rc())
                .unwrap(),
            None
        );
    }

    #[test]
    fn reset_hidden_when_resets_at_missing() {
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
    fn reset_hidden_when_five_hour_bucket_absent() {
        assert_eq!(
            RateLimit5hResetSegment::default()
                .render(&ctx_with_usage(Ok(endpoint_empty())), &rc())
                .unwrap(),
            None,
        );
    }

    #[test]
    fn reset_compact_format_drops_suffix_spaces() {
        let seg = RateLimit5hResetSegment {
            compact: true,
            ..Default::default()
        };
        let dc = ctx_with_usage(Ok(data_with_reset_in(4 * 60 + 37)));
        let rendered = seg.render(&dc, &rc()).unwrap().expect("visible");
        assert_eq!(rendered.text(), "5h reset: 4h37m");
    }

    #[test]
    fn reset_renders_error_when_usage_fails() {
        let dc = ctx_with_usage(Err(UsageError::Timeout));
        let rendered = RateLimit5hResetSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "5h reset: [Timeout]");
    }

    #[test]
    fn reset_progress_format_divides_by_five_hour_window_not_seven_day() {
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
    fn reset_jsonl_mode_derives_reset_from_five_hour_window_ends_at() {
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
    fn reset_jsonl_mode_hides_when_block_inactive() {
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
    fn reset_jsonl_mode_hides_when_ends_at_in_past() {
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
    fn reset_declares_usage_as_its_only_data_dep() {
        assert_eq!(
            RateLimit5hResetSegment::default().data_deps(),
            &[DataDep::Usage],
        );
    }

    #[test]
    fn reset_from_extras_applies_duration_format_knobs() {
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
    fn reset_from_extras_warns_on_percent_format_string() {
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
