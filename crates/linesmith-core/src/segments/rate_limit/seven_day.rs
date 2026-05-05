//! 7-day rate-limit segments: utilization (`RateLimit7dSegment`) and
//! reset countdown (`RateLimit7dResetSegment`). Both mirror the
//! `five_hour` segments but read `data.seven_day`.
//!
//! - Utilization hides only when the endpoint bucket is absent — the
//!   7d JSONL window is always populated (zero-valued on empty
//!   transcripts per `rate-limit-segments.md` §JSONL-fallback display).
//! - Reset countdown hides under JSONL entirely (rolling 7d window
//!   has no hard reset; ADR-0013 §Decision drivers explicitly rejects
//!   synthesizing one).

use std::collections::BTreeMap;

use super::config::{
    apply_common_extras, parse_percent_format, parse_reset_format, CommonRateLimitConfig,
    PercentFormat, ResetFormat, PRIORITY,
};
use super::format::{format_jsonl_tokens, format_percent, format_reset, render_error, ResetWindow};
use super::window::{resolve_seven_day_reset, UsageWindow, WindowResolution};
use crate::data_context::{DataContext, DataDep};
use crate::segments::extras::parse_bool;
use crate::segments::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::theme::Role;

#[non_exhaustive]
pub struct RateLimit7dSegment {
    pub format: PercentFormat,
    pub invert: bool,
    pub config: CommonRateLimitConfig,
}

impl Default for RateLimit7dSegment {
    fn default() -> Self {
        Self {
            format: PercentFormat::Percent,
            invert: false,
            config: CommonRateLimitConfig::new("7d"),
        }
    }
}

impl RateLimit7dSegment {
    #[must_use]
    pub fn from_extras(
        extras: &BTreeMap<String, toml::Value>,
        warn: &mut impl FnMut(&str),
    ) -> Self {
        let mut seg = Self::default();
        apply_common_extras(&mut seg.config, extras, "rate_limit_7d", warn);
        if let Some(f) = parse_percent_format(extras, "rate_limit_7d", warn) {
            seg.format = f;
        }
        if let Some(b) = parse_bool(extras, "invert", "rate_limit_7d", warn) {
            seg.invert = b;
        }
        if seg.config.invalid_progress_width {
            seg.format = PercentFormat::Percent;
        }
        seg
    }
}

impl Segment for RateLimit7dSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let usage = ctx.usage();
        let text = match &*usage {
            Ok(data) => match UsageWindow::SevenDay.resolve_percent(data) {
                Ok(WindowResolution::Endpoint(bucket)) => {
                    format_percent(bucket, self.format, self.invert, &self.config)
                }
                Ok(WindowResolution::JsonlTokens(total)) => {
                    format_jsonl_tokens(total, &self.config)
                }
                Err(reason) => {
                    crate::lsm_debug!("rate_limit_7d: {reason}; hiding");
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
pub struct RateLimit7dResetSegment {
    pub format: ResetFormat,
    pub compact: bool,
    pub use_days: bool,
    pub config: CommonRateLimitConfig,
}

impl Default for RateLimit7dResetSegment {
    fn default() -> Self {
        Self {
            format: ResetFormat::Duration,
            compact: false,
            use_days: true,
            config: CommonRateLimitConfig::new("7d reset"),
        }
    }
}

impl RateLimit7dResetSegment {
    #[must_use]
    pub fn from_extras(
        extras: &BTreeMap<String, toml::Value>,
        warn: &mut impl FnMut(&str),
    ) -> Self {
        let mut seg = Self::default();
        apply_common_extras(&mut seg.config, extras, "rate_limit_7d_reset", warn);
        if let Some(f) = parse_reset_format(extras, "rate_limit_7d_reset", warn) {
            seg.format = f;
        }
        if let Some(b) = parse_bool(extras, "compact", "rate_limit_7d_reset", warn) {
            seg.compact = b;
        }
        if let Some(b) = parse_bool(extras, "use_days", "rate_limit_7d_reset", warn) {
            seg.use_days = b;
        }
        // `progress_width` only matters for `Progress`. An invalid
        // value forced the spec's "fall back to duration" rule onto
        // every variant pre-Absolute; now scope it so a stale
        // `progress_width` doesn't clobber `format = "absolute"`.
        if seg.config.invalid_progress_width && matches!(seg.format, ResetFormat::Progress) {
            seg.format = ResetFormat::Duration;
        }
        seg
    }
}

impl Segment for RateLimit7dResetSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let usage = ctx.usage();
        let text = match &*usage {
            Ok(data) => {
                let resets_at = match resolve_seven_day_reset(data) {
                    Ok(at) => at,
                    Err(reason) => {
                        crate::lsm_debug!("rate_limit_7d_reset: {reason}; hiding");
                        return Ok(None);
                    }
                };
                let remaining = resets_at.duration_since(jiff::Timestamp::now());
                if remaining <= jiff::SignedDuration::ZERO {
                    crate::lsm_debug!(
                        "rate_limit_7d_reset: seven_day.resets_at in the past ({resets_at}); hiding"
                    );
                    return Ok(None);
                }
                format_reset(
                    resets_at,
                    remaining,
                    &self.format,
                    self.compact,
                    self.use_days,
                    ResetWindow::SevenDay,
                    false,
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
        EndpointUsage, JsonlUsage, SevenDayWindow, TokenCounts, UsageBucket, UsageData, UsageError,
    };
    use crate::input::{ModelInfo, Percent, StatusContext, Tool, WorkspaceInfo};
    use jiff::SignedDuration;
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

    fn endpoint_data_with_seven_day(pct: f32) -> UsageData {
        UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: Some(UsageBucket {
                utilization: Percent::new(pct).unwrap(),
                resets_at: None,
            }),
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: std::collections::HashMap::new(),
        })
    }

    fn jsonl_data_with_seven_day_tokens(total: u64) -> UsageData {
        let tokens = TokenCounts::from_parts(total, 0, 0, 0);
        UsageData::Jsonl(JsonlUsage::new(None, SevenDayWindow::new(tokens)))
    }

    fn data_with_reset_in(duration: SignedDuration) -> UsageData {
        // 30s of slack so clock drift between setup and render doesn't
        // round the minute boundary down and flip expected output.
        let slack = if duration > SignedDuration::ZERO {
            SignedDuration::from_secs(30)
        } else {
            SignedDuration::ZERO
        };
        UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: Some(UsageBucket {
                utilization: Percent::new(33.0).unwrap(),
                resets_at: Some(jiff::Timestamp::now() + duration + slack),
            }),
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: std::collections::HashMap::new(),
        })
    }

    #[test]
    fn hidden_when_seven_day_bucket_absent() {
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
            RateLimit7dSegment::default()
                .render(&ctx_with_usage(Ok(data)), &rc())
                .unwrap(),
            None,
        );
    }

    #[test]
    fn renders_percent_happy_path() {
        let dc = ctx_with_usage(Ok(endpoint_data_with_seven_day(33.0)));
        let rendered = RateLimit7dSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "7d: 33.0%");
    }

    #[test]
    fn renders_inverted_percent_when_configured() {
        let dc = ctx_with_usage(Ok(endpoint_data_with_seven_day(33.0)));
        let seg = RateLimit7dSegment {
            invert: true,
            ..Default::default()
        };
        let rendered = seg.render(&dc, &rc()).unwrap().expect("visible");
        assert_eq!(rendered.text(), "7d: 67.0%");
    }

    #[test]
    fn jsonl_mode_renders_compact_tokens_with_stale_marker() {
        let dc = ctx_with_usage(Ok(jsonl_data_with_seven_day_tokens(1_200_000)));
        let rendered = RateLimit7dSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "~7d: 1.2M");
    }

    #[test]
    fn jsonl_mode_still_renders_on_zero_tokens() {
        // The 7d window is always populated under JSONL (zero-valued
        // on empty transcripts per `docs/specs/jsonl-aggregation.md`),
        // so an empty-transcript user still sees `~7d: 0`, not a hide.
        let dc = ctx_with_usage(Ok(jsonl_data_with_seven_day_tokens(0)));
        let rendered = RateLimit7dSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "~7d: 0");
    }

    #[test]
    fn renders_error_when_usage_fails() {
        let dc = ctx_with_usage(Err(UsageError::Unauthorized));
        let rendered = RateLimit7dSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "7d: [Unauthorized]");
    }

    #[test]
    fn declares_usage_as_its_only_data_dep() {
        assert_eq!(RateLimit7dSegment::default().data_deps(), &[DataDep::Usage],);
    }

    #[test]
    fn from_extras_applies_percent_format_knobs() {
        let mut extras = std::collections::BTreeMap::new();
        extras.insert("format".into(), toml::Value::String("progress".into()));
        extras.insert("invert".into(), toml::Value::Boolean(true));
        extras.insert("label".into(), toml::Value::String("week".into()));
        let mut warnings = Vec::new();
        let seg = RateLimit7dSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(seg.format, PercentFormat::Progress);
        assert!(seg.invert);
        assert_eq!(seg.config.label, "week");
    }

    // ---- RateLimit7dResetSegment tests ----

    #[test]
    fn reset_renders_countdown_with_days_by_default() {
        let dc = ctx_with_usage(Ok(data_with_reset_in(
            SignedDuration::from_hours(4 * 24) + SignedDuration::from_hours(8),
        )));
        let rendered = RateLimit7dResetSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "7d reset: 4d 8hr");
    }

    #[test]
    fn reset_use_days_false_emits_hours_only() {
        let seg = RateLimit7dResetSegment {
            use_days: false,
            ..Default::default()
        };
        let dc = ctx_with_usage(Ok(data_with_reset_in(
            SignedDuration::from_hours(24) + SignedDuration::from_hours(3),
        )));
        let rendered = seg.render(&dc, &rc()).unwrap().expect("visible");
        assert_eq!(rendered.text(), "7d reset: 27hr");
    }

    #[test]
    fn reset_hidden_when_resets_at_in_past() {
        let dc = ctx_with_usage(Ok(data_with_reset_in(SignedDuration::from_mins(-10))));
        assert_eq!(
            RateLimit7dResetSegment::default()
                .render(&dc, &rc())
                .unwrap(),
            None,
        );
    }

    #[test]
    fn reset_hidden_when_seven_day_bucket_absent() {
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
            RateLimit7dResetSegment::default()
                .render(&ctx_with_usage(Ok(data)), &rc())
                .unwrap(),
            None,
        );
    }

    #[test]
    fn reset_renders_error_when_usage_fails() {
        let dc = ctx_with_usage(Err(UsageError::RateLimited { retry_after: None }));
        let rendered = RateLimit7dResetSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "7d reset: [Rate limited]");
    }

    #[test]
    fn reset_hidden_under_jsonl_fallback() {
        // Spec §JSONL-fallback display: 7d reset hides entirely
        // under JSONL because the 7d window is rolling (no hard reset).
        // ADR-0013 §Decision drivers: faking one is the exact failure
        // mode the ADR rejects.
        let data = UsageData::Jsonl(JsonlUsage::new(
            None,
            SevenDayWindow::new(TokenCounts::default()),
        ));
        let dc = ctx_with_usage(Ok(data));
        assert_eq!(
            RateLimit7dResetSegment::default()
                .render(&dc, &rc())
                .unwrap(),
            None,
        );
    }

    #[test]
    fn reset_progress_format_divides_by_seven_day_window_not_five_hour() {
        // Regression guard for the deleted magnitude heuristic: a 7d
        // reset at 4h remaining must render ~97% elapsed, not ~20%.
        // If anyone re-introduces per-magnitude window derivation, this
        // test flips.
        let dc = ctx_with_usage(Ok(data_with_reset_in(SignedDuration::from_hours(4))));
        let seg = RateLimit7dResetSegment {
            format: ResetFormat::Progress,
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
            (96.0..=98.5).contains(&pct),
            "expected ~97% elapsed, got {pct}% from {:?}",
            rendered.text(),
        );
    }

    #[test]
    fn reset_hidden_when_resets_at_missing() {
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: Some(UsageBucket {
                utilization: Percent::new(33.0).unwrap(),
                resets_at: None,
            }),
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: std::collections::HashMap::new(),
        });
        assert_eq!(
            RateLimit7dResetSegment::default()
                .render(&ctx_with_usage(Ok(data)), &rc())
                .unwrap(),
            None,
        );
    }

    #[test]
    fn reset_declares_usage_as_its_only_data_dep() {
        assert_eq!(
            RateLimit7dResetSegment::default().data_deps(),
            &[DataDep::Usage],
        );
    }

    #[test]
    fn reset_from_extras_applies_duration_format_knobs() {
        let mut extras = std::collections::BTreeMap::new();
        extras.insert("format".into(), toml::Value::String("progress".into()));
        extras.insert("compact".into(), toml::Value::Boolean(true));
        let mut warnings = Vec::new();
        let seg =
            RateLimit7dResetSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(seg.format, ResetFormat::Progress);
        assert!(seg.compact);
    }

    #[test]
    fn reset_from_extras_warns_on_percent_format_string() {
        // Parity with five_hour: `format = "percent"` is valid for
        // utilization segments but NOT for reset segments;
        // parse_reset_format rejects it.
        let mut extras = std::collections::BTreeMap::new();
        extras.insert("format".into(), toml::Value::String("percent".into()));
        let mut warnings = Vec::new();
        let _ =
            RateLimit7dResetSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("format"), "{:?}", warnings[0]);
    }

    #[test]
    fn reset_invalid_progress_width_does_not_clobber_absolute_format() {
        // Parity with five_hour: a stale `progress_width` must not
        // silently downgrade `format = "absolute"` to `Duration`.
        let mut extras = std::collections::BTreeMap::new();
        extras.insert("format".into(), toml::Value::String("absolute".into()));
        extras.insert("progress_width".into(), toml::Value::Integer(0));
        let mut warnings = Vec::new();
        let seg =
            RateLimit7dResetSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert!(
            matches!(seg.format, ResetFormat::Absolute(_)),
            "absolute survived: {:?}",
            seg.format
        );
        assert!(
            warnings.iter().any(|w| w.contains("progress_width")),
            "expected progress_width warning: {warnings:?}"
        );
    }
}
