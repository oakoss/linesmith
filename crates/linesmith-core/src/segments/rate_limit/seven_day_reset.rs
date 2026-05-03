//! 7-day window reset countdown. Mirrors the `five_hour_reset`
//! segment shape but reads `data.seven_day`.

use super::five_hour::PRIORITY;
use std::collections::BTreeMap;

use super::format::{
    apply_common_extras, format_duration, parse_bool, parse_duration_format, render_error,
    CommonRateLimitConfig, DurationFormat, ResetWindow,
};
use crate::data_context::{DataContext, DataDep, UsageData};
use crate::segments::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::theme::Role;

#[non_exhaustive]
pub struct RateLimit7dResetSegment {
    pub format: DurationFormat,
    pub compact: bool,
    pub use_days: bool,
    pub config: CommonRateLimitConfig,
}

impl Default for RateLimit7dResetSegment {
    fn default() -> Self {
        Self {
            format: DurationFormat::Duration,
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
        if let Some(f) = parse_duration_format(extras, "rate_limit_7d_reset", warn) {
            seg.format = f;
        }
        if let Some(b) = parse_bool(extras, "compact", "rate_limit_7d_reset", warn) {
            seg.compact = b;
        }
        if let Some(b) = parse_bool(extras, "use_days", "rate_limit_7d_reset", warn) {
            seg.use_days = b;
        }
        if seg.config.invalid_progress_width {
            seg.format = DurationFormat::Duration;
        }
        seg
    }
}

impl Segment for RateLimit7dResetSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let usage = ctx.usage();
        let text = match &*usage {
            Ok(UsageData::Endpoint(e)) => {
                let Some(bucket) = e.seven_day.as_ref() else {
                    crate::lsm_debug!(
                        "rate_limit_7d_reset: endpoint usage.seven_day absent; hiding"
                    );
                    return Ok(None);
                };
                let Some(resets_at) = bucket.resets_at else {
                    crate::lsm_debug!("rate_limit_7d_reset: seven_day.resets_at absent; hiding");
                    return Ok(None);
                };
                let remaining = resets_at.signed_duration_since(chrono::Utc::now());
                if remaining <= chrono::Duration::zero() {
                    crate::lsm_debug!(
                        "rate_limit_7d_reset: seven_day.resets_at in the past ({resets_at}); hiding"
                    );
                    return Ok(None);
                }
                format_duration(
                    remaining,
                    self.format,
                    self.compact,
                    self.use_days,
                    ResetWindow::SevenDay,
                    false,
                    &self.config,
                )
            }
            // Spec §JSONL-fallback display: rolling 7d window has no
            // hard reset, so this segment hides entirely under JSONL.
            // ADR-0013 explicitly rejects synthesizing one.
            Ok(UsageData::Jsonl(_)) => {
                crate::lsm_debug!("rate_limit_7d_reset: jsonl fallback has no hard reset; hiding");
                return Ok(None);
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
    use chrono::Duration;
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

    fn data_with_reset_in(duration: Duration) -> UsageData {
        // 30s of slack so clock drift between setup and render doesn't
        // round the minute boundary down and flip expected output.
        let slack = if duration > Duration::zero() {
            Duration::seconds(30)
        } else {
            Duration::zero()
        };
        UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: Some(UsageBucket {
                utilization: Percent::new(33.0).unwrap(),
                resets_at: Some(chrono::Utc::now() + duration + slack),
            }),
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: std::collections::HashMap::new(),
        })
    }

    #[test]
    fn renders_countdown_with_days_by_default() {
        let dc = ctx_with_usage(Ok(data_with_reset_in(
            Duration::days(4) + Duration::hours(8),
        )));
        let rendered = RateLimit7dResetSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "7d reset: 4d 8hr");
    }

    #[test]
    fn use_days_false_emits_hours_only() {
        let seg = RateLimit7dResetSegment {
            use_days: false,
            ..Default::default()
        };
        let dc = ctx_with_usage(Ok(data_with_reset_in(
            Duration::days(1) + Duration::hours(3),
        )));
        let rendered = seg.render(&dc, &rc()).unwrap().expect("visible");
        assert_eq!(rendered.text(), "7d reset: 27hr");
    }

    #[test]
    fn hidden_when_resets_at_in_past() {
        let dc = ctx_with_usage(Ok(data_with_reset_in(Duration::minutes(-10))));
        assert_eq!(
            RateLimit7dResetSegment::default()
                .render(&dc, &rc())
                .unwrap(),
            None,
        );
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
            RateLimit7dResetSegment::default()
                .render(&ctx_with_usage(Ok(data)), &rc())
                .unwrap(),
            None,
        );
    }

    #[test]
    fn renders_error_when_usage_fails() {
        let dc = ctx_with_usage(Err(UsageError::RateLimited { retry_after: None }));
        let rendered = RateLimit7dResetSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "7d reset: [Rate limited]");
    }

    #[test]
    fn hidden_under_jsonl_fallback() {
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
    fn progress_format_divides_by_seven_day_window_not_five_hour() {
        // Regression guard for the deleted magnitude heuristic: a 7d
        // reset at 4h remaining must render ~97% elapsed, not ~20%.
        // If anyone re-introduces per-magnitude window derivation, this
        // test flips.
        let dc = ctx_with_usage(Ok(data_with_reset_in(Duration::hours(4))));
        let seg = RateLimit7dResetSegment {
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
            (96.0..=98.5).contains(&pct),
            "expected ~97% elapsed, got {pct}% from {:?}",
            rendered.text(),
        );
    }

    #[test]
    fn hidden_when_resets_at_missing() {
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
    fn declares_usage_as_its_only_data_dep() {
        assert_eq!(
            RateLimit7dResetSegment::default().data_deps(),
            &[DataDep::Usage],
        );
    }

    #[test]
    fn from_extras_applies_duration_format_knobs() {
        let mut extras = std::collections::BTreeMap::new();
        extras.insert("format".into(), toml::Value::String("progress".into()));
        extras.insert("compact".into(), toml::Value::Boolean(true));
        let mut warnings = Vec::new();
        let seg =
            RateLimit7dResetSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(seg.format, DurationFormat::Progress);
        assert!(seg.compact);
    }
}
