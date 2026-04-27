//! `extra_usage` segment: monthly overage credits. Auto-hides when
//! the account has not enabled overage (`is_enabled = false`); surfaces
//! error strings for fetch failures per spec §Render semantics.

use super::rate_limit_5h::PRIORITY;
use std::collections::BTreeMap;

use super::rate_limit_format::{
    apply_common_extras, format_extra_usage, parse_extra_usage_format, render_error,
    CommonRateLimitConfig, ExtraUsageFormat,
};
use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::{DataContext, DataDep, UsageData};
use crate::theme::Role;

#[non_exhaustive]
pub struct ExtraUsageSegment {
    pub format: ExtraUsageFormat,
    pub config: CommonRateLimitConfig,
}

impl Default for ExtraUsageSegment {
    fn default() -> Self {
        Self {
            format: ExtraUsageFormat::Currency,
            config: CommonRateLimitConfig::new("extra"),
        }
    }
}

impl ExtraUsageSegment {
    #[must_use]
    pub fn from_extras(
        extras: &BTreeMap<String, toml::Value>,
        warn: &mut impl FnMut(&str),
    ) -> Self {
        let mut seg = Self::default();
        apply_common_extras(&mut seg.config, extras, "extra_usage", warn);
        if let Some(f) = parse_extra_usage_format(extras, "extra_usage", warn) {
            seg.format = f;
        }
        seg
    }
}

impl Segment for ExtraUsageSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let usage = ctx.usage();
        match &*usage {
            Ok(UsageData::Endpoint(e)) => {
                let Some(extra) = e.extra_usage.as_ref() else {
                    crate::lsm_debug!("extra_usage: endpoint extra_usage absent; hiding");
                    return Ok(None);
                };
                // `is_enabled = false` (or missing) hides silently:
                // the user hasn't opted into overage, so there's no
                // state worth rendering.
                if !extra.is_enabled.unwrap_or(false) {
                    crate::lsm_debug!("extra_usage: extra_usage.is_enabled = false/absent; hiding");
                    return Ok(None);
                }
                match format_extra_usage(extra, self.format, &self.config) {
                    Some(text) => Ok(Some(RenderedSegment::new(text).with_role(Role::Info))),
                    None => {
                        crate::lsm_debug!(
                            "extra_usage: format_extra_usage returned None (missing cost or format suppressed); hiding"
                        );
                        Ok(None)
                    }
                }
            }
            // Spec §JSONL-fallback display: transcripts carry no
            // overage data, so the segment hides silently under JSONL.
            Ok(UsageData::Jsonl(_)) => {
                crate::lsm_debug!("extra_usage: jsonl fallback has no overage data; hiding");
                Ok(None)
            }
            Err(err) => {
                // User opted in by enabling this segment; a fetch
                // failure must surface the error string (not silently
                // hide) so regressions aren't indistinguishable from
                // "overage disabled" (spec §Render semantics).
                let text = render_error(err, &self.config);
                Ok(Some(RenderedSegment::new(text).with_role(Role::Info)))
            }
        }
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
        EndpointUsage, ExtraUsage, JsonlUsage, SevenDayWindow, TokenCounts, UsageData, UsageError,
    };
    use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
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
            raw: Arc::new(serde_json::Value::Null),
        });
        dc.preseed_usage(usage).expect("seed");
        dc
    }

    fn data_with_extra(extra: Option<ExtraUsage>) -> UsageData {
        UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: extra,
            unknown_buckets: std::collections::HashMap::new(),
        })
    }

    fn enabled_extra(limit: Option<f64>, used: Option<f64>) -> ExtraUsage {
        ExtraUsage {
            is_enabled: Some(true),
            utilization: None,
            monthly_limit: limit,
            used_credits: used,
            currency: Some("USD".into()),
        }
    }

    #[test]
    fn hidden_when_extra_usage_missing() {
        let dc = ctx_with_usage(Ok(data_with_extra(None)));
        assert_eq!(
            ExtraUsageSegment::default().render(&dc, &rc()).unwrap(),
            None
        );
    }

    #[test]
    fn hidden_when_is_enabled_false() {
        let extra = ExtraUsage {
            is_enabled: Some(false),
            utilization: None,
            monthly_limit: Some(100.0),
            used_credits: Some(40.0),
            currency: Some("USD".into()),
        };
        let dc = ctx_with_usage(Ok(data_with_extra(Some(extra))));
        assert_eq!(
            ExtraUsageSegment::default().render(&dc, &rc()).unwrap(),
            None
        );
    }

    #[test]
    fn renders_remaining_credits_in_currency_format() {
        let dc = ctx_with_usage(Ok(data_with_extra(Some(enabled_extra(
            Some(100.0),
            Some(40.0),
        )))));
        let rendered = ExtraUsageSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "extra: $60.00");
    }

    #[test]
    fn non_usd_currency_renders_iso_code_prefix() {
        let extra = ExtraUsage {
            is_enabled: Some(true),
            utilization: None,
            monthly_limit: Some(100.0),
            used_credits: Some(40.0),
            currency: Some("EUR".into()),
        };
        let dc = ctx_with_usage(Ok(data_with_extra(Some(extra))));
        let rendered = ExtraUsageSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "extra: EUR 60.00");
    }

    #[test]
    fn renders_error_instead_of_hiding_when_fetch_fails() {
        let dc = ctx_with_usage(Err(UsageError::Timeout));
        let rendered = ExtraUsageSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "extra: [Timeout]");
    }

    #[test]
    fn declares_usage_as_its_only_data_dep() {
        assert_eq!(ExtraUsageSegment::default().data_deps(), &[DataDep::Usage],);
    }

    #[test]
    fn hidden_under_jsonl_fallback() {
        // Spec §JSONL-fallback display: transcripts carry no overage
        // data so the segment hides, not errors. ADR-0013 makes this a
        // type-level guarantee — `UsageData::Jsonl` can't carry
        // `extra_usage`.
        let data = UsageData::Jsonl(JsonlUsage::new(
            None,
            SevenDayWindow::new(TokenCounts::default()),
        ));
        let dc = ctx_with_usage(Ok(data));
        assert_eq!(
            ExtraUsageSegment::default().render(&dc, &rc()).unwrap(),
            None
        );
    }

    #[test]
    fn renders_percent_format_when_configured() {
        use crate::input::Percent;
        let extra = ExtraUsage {
            is_enabled: Some(true),
            utilization: Some(Percent::new(42.5).unwrap()),
            monthly_limit: None,
            used_credits: None,
            currency: None,
        };
        let dc = ctx_with_usage(Ok(data_with_extra(Some(extra))));
        let seg = ExtraUsageSegment {
            format: ExtraUsageFormat::Percent,
            ..Default::default()
        };
        let rendered = seg.render(&dc, &rc()).unwrap().expect("visible");
        assert_eq!(rendered.text(), "extra: 42.5%");
    }

    #[test]
    fn from_extras_applies_extra_usage_format_knobs() {
        let mut extras = std::collections::BTreeMap::new();
        extras.insert("format".into(), toml::Value::String("percent".into()));
        extras.insert("label".into(), toml::Value::String("overage".into()));
        let mut warnings = Vec::new();
        let seg = ExtraUsageSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(seg.format, ExtraUsageFormat::Percent);
        assert_eq!(seg.config.label, "overage");
    }

    #[test]
    fn from_extras_warns_on_duration_format_string() {
        // `format = "duration"` is valid for reset segments but NOT
        // for extra_usage; parse_extra_usage_format rejects it.
        let mut extras = std::collections::BTreeMap::new();
        extras.insert("format".into(), toml::Value::String("duration".into()));
        let mut warnings = Vec::new();
        let _ = ExtraUsageSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("format"), "{:?}", warnings[0]);
    }

    #[test]
    fn currency_falls_back_to_percent_when_monthly_limit_missing() {
        use crate::input::Percent;
        let extra = ExtraUsage {
            is_enabled: Some(true),
            utilization: Some(Percent::new(42.5).unwrap()),
            monthly_limit: None,
            used_credits: Some(40.0),
            currency: Some("USD".into()),
        };
        let dc = ctx_with_usage(Ok(data_with_extra(Some(extra))));
        let rendered = ExtraUsageSegment::default()
            .render(&dc, &rc())
            .unwrap()
            .expect("visible");
        assert_eq!(rendered.text(), "extra: 42.5%");
    }
}
