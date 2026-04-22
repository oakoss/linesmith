//! 7-day rate-limit utilization segment. Mirrors `rate_limit_5h` but
//! reads `data.seven_day`. Hidden when the bucket is absent (JSONL
//! fallback always omits it per `rate-limit-segments.md`
//! §JSONL-fallback display).

use super::rate_limit_5h::PRIORITY;
use std::collections::BTreeMap;

use super::rate_limit_format::{
    apply_common_extras, format_percent, parse_bool, parse_percent_format, render_error,
    CommonRateLimitConfig, PercentFormat,
};
use super::{RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::{DataContext, DataDep};
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
    fn render(&self, ctx: &DataContext) -> RenderResult {
        let usage = ctx.usage();
        let text = match &*usage {
            Ok(data) => match &data.seven_day {
                Some(bucket) => {
                    format_percent(bucket, self.format, self.invert, data.source, &self.config)
                }
                None => return Ok(None),
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
    use crate::data_context::{UsageBucket, UsageData, UsageError, UsageSource};
    use crate::input::{ModelInfo, Percent, StatusContext, Tool, WorkspaceInfo};
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

    fn data_with_seven_day(pct: f32, source: UsageSource) -> UsageData {
        UsageData {
            source,
            five_hour: None,
            seven_day: Some(UsageBucket {
                utilization: Percent::new(pct).unwrap(),
                resets_at: None,
            }),
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
        }
    }

    #[test]
    fn hidden_when_seven_day_bucket_absent() {
        let data = UsageData {
            source: UsageSource::Endpoint,
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
        };
        assert_eq!(
            RateLimit7dSegment::default()
                .render(&ctx_with_usage(Ok(data)))
                .unwrap(),
            None,
        );
    }

    #[test]
    fn renders_percent_happy_path() {
        let dc = ctx_with_usage(Ok(data_with_seven_day(33.0, UsageSource::Endpoint)));
        let rendered = RateLimit7dSegment::default()
            .render(&dc)
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "7d: 33.0%");
    }

    #[test]
    fn renders_inverted_percent_when_configured() {
        let dc = ctx_with_usage(Ok(data_with_seven_day(33.0, UsageSource::Endpoint)));
        let seg = RateLimit7dSegment {
            invert: true,
            ..Default::default()
        };
        let rendered = seg.render(&dc).unwrap().expect("visible");
        assert_eq!(rendered.text(), "7d: 67.0%");
    }

    #[test]
    fn prefixes_stale_marker_on_jsonl_source() {
        let dc = ctx_with_usage(Ok(data_with_seven_day(33.0, UsageSource::Jsonl)));
        let rendered = RateLimit7dSegment::default()
            .render(&dc)
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "~7d: 33.0%");
    }

    #[test]
    fn renders_error_when_usage_fails() {
        let dc = ctx_with_usage(Err(UsageError::Unauthorized));
        let rendered = RateLimit7dSegment::default()
            .render(&dc)
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "7d: [Unauthorized]");
    }

    #[test]
    fn jsonl_source_still_hides_seven_day_when_bucket_missing() {
        // JSONL fallback doesn't populate `seven_day` (see
        // `docs/specs/jsonl-aggregation.md`); hide rather than render
        // the stale marker on an empty bucket.
        let data = UsageData {
            source: UsageSource::Jsonl,
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
        };
        assert_eq!(
            RateLimit7dSegment::default()
                .render(&ctx_with_usage(Ok(data)))
                .unwrap(),
            None,
        );
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
}
