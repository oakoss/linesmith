//! 5-hour rate-limit utilization segment.
//!
//! Reads `ctx.usage()` (Arc<Result<UsageData, UsageError>>) and
//! renders either a percentage, a progress bar, or one of the error
//! strings from `docs/specs/rate-limit-segments.md` §Error message
//! table. Hidden entirely when the bucket is missing from a
//! successful response (accounts with no 5-hour window exposed).

use std::collections::BTreeMap;

use super::rate_limit_format::{
    apply_common_extras, format_jsonl_tokens, format_percent, parse_bool, parse_percent_format,
    render_error, CommonRateLimitConfig, PercentFormat,
};
use super::{RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::{DataContext, DataDep, UsageData};
use crate::theme::Role;

/// Between model (64) and effort (160). Rate-limit visibility is
/// high-demand but the data is cached/delayed, so it yields before
/// live-health metrics.
pub(crate) const PRIORITY: u8 = 96;

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
    fn render(&self, ctx: &DataContext) -> RenderResult {
        let usage = ctx.usage();
        let text = match &*usage {
            Ok(UsageData::Endpoint(e)) => match &e.five_hour {
                Some(bucket) => format_percent(bucket, self.format, self.invert, &self.config),
                None => {
                    crate::lsm_debug!("rate_limit_5h: endpoint usage.five_hour absent; hiding");
                    return Ok(None);
                }
            },
            Ok(UsageData::Jsonl(j)) => match &j.five_hour {
                Some(window) => format_jsonl_tokens(window.tokens.total(), &self.config),
                None => {
                    crate::lsm_debug!("rate_limit_5h: jsonl five_hour block inactive; hiding");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_context::{
        EndpointUsage, ExtraUsage, FiveHourWindow, JsonlUsage, SevenDayWindow, TokenCounts,
        UsageBucket, UsageData, UsageError,
    };
    use crate::input::{ModelInfo, Percent, StatusContext, Tool, WorkspaceInfo};
    use chrono::{Duration as ChronoDuration, Utc};
    use std::path::PathBuf;
    use std::sync::Arc;

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
        let start = Utc::now() - ChronoDuration::hours(1);
        UsageData::Jsonl(JsonlUsage::new(
            Some(FiveHourWindow::new(tokens, start)),
            SevenDayWindow::new(TokenCounts::default()),
        ))
    }

    #[test]
    fn hidden_when_five_hour_bucket_absent() {
        let rendered = RateLimit5hSegment::default()
            .render(&ctx_with_usage(Ok(endpoint_empty())))
            .expect("render ok");
        assert_eq!(rendered, None);
    }

    #[test]
    fn renders_percent_happy_path() {
        let dc = ctx_with_usage(Ok(endpoint_data_with_five_hour(22.0)));
        let rendered = RateLimit5hSegment::default()
            .render(&dc)
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
        let rendered = seg.render(&dc).unwrap().expect("visible");
        assert_eq!(rendered.text(), "5h: 78.0%");
    }

    #[test]
    fn jsonl_mode_renders_compact_tokens_with_stale_marker() {
        let dc = ctx_with_usage(Ok(jsonl_with_five_hour_tokens(420_000)));
        let rendered = RateLimit5hSegment::default()
            .render(&dc)
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
        assert_eq!(RateLimit5hSegment::default().render(&dc).unwrap(), None);
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
        let rendered = seg.render(&dc).unwrap().expect("visible");
        assert_eq!(rendered.text(), "~5h: 1.2M");
    }

    #[test]
    fn renders_progress_bar_when_format_is_progress() {
        let dc = ctx_with_usage(Ok(endpoint_data_with_five_hour(50.0)));
        let seg = RateLimit5hSegment {
            format: PercentFormat::Progress,
            ..Default::default()
        };
        let rendered = seg.render(&dc).unwrap().expect("visible");
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
        let rendered = seg.render(&dc).unwrap().expect("visible");
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
        let rendered = seg.render(&dc).unwrap().expect("visible");
        assert!(rendered.text().contains("█"));
        assert!(!rendered.text().contains("░"));
        assert!(rendered.text().ends_with("100.0%"), "{}", rendered.text());
    }

    #[test]
    fn renders_error_table_strings() {
        let dc = ctx_with_usage(Err(UsageError::Timeout));
        let rendered = RateLimit5hSegment::default()
            .render(&dc)
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
            .render(&ctx_with_usage(Ok(data)))
            .unwrap();
        assert_eq!(rendered, None);
    }
}
