//! `rate_limit_7d_model`: the weekly bucket the endpoint scopes to a
//! single model — the third row in Orca-style usage displays
//! (`Fable: 82%`).
//!
//! Unlike its siblings this reads `data.limits` rather than a named
//! bucket, because that is where per-model usage arrives per
//! [ADR-0030](../../../../../docs/adrs/0030-model-scoped-usage-arrives-in-a-limits-array.md).
//! It has no JSONL fallback: transcripts carry no per-model split, so
//! there is nothing to degrade to and `stale_marker` never appears.

use std::collections::BTreeMap;

use super::config::{
    apply_common_extras, parse_percent_format, CommonRateLimitConfig, PercentFormat, PRIORITY,
};
use super::format::{render_error, render_percent};
use crate::data_context::{DataContext, DataDep, LimitKind, UsageBucket, UsageData, UsageLimit};
use crate::segments::extras::parse_bool;
use crate::segments::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::theme::Role;

/// Stands in for the bucket's model name when the endpoint failed and
/// there is no bucket to name one.
const DEFAULT_ERROR_LABEL: &str = "7dm";

/// When the segment renders relative to the model in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Visibility {
    /// Render only when a scoped bucket matches the session's model.
    #[default]
    Smart,
    /// Render whenever a scoped bucket is present.
    Always,
}

#[non_exhaustive]
pub struct RateLimit7dModelSegment {
    pub visibility: Visibility,
    pub format: PercentFormat,
    pub invert: bool,
    pub config: CommonRateLimitConfig,
}

impl Default for RateLimit7dModelSegment {
    fn default() -> Self {
        Self {
            visibility: Visibility::Smart,
            format: PercentFormat::Percent,
            invert: false,
            // Empty label means "render the bucket's own model name" for
            // this segment, not "hide the label" as it does for its
            // siblings: an unlabelled percentage is indistinguishable
            // from `rate_limit_7d`.
            config: CommonRateLimitConfig::new(""),
        }
    }
}

impl RateLimit7dModelSegment {
    #[must_use]
    pub fn from_extras(
        extras: &BTreeMap<String, toml::Value>,
        warn: &mut impl FnMut(&str),
    ) -> Self {
        let mut seg = Self::default();
        apply_common_extras(&mut seg.config, extras, "rate_limit_7d_model", warn);
        if let Some(f) = parse_percent_format(extras, "rate_limit_7d_model", warn) {
            seg.format = f;
        }
        if let Some(b) = parse_bool(extras, "invert", "rate_limit_7d_model", warn) {
            seg.invert = b;
        }
        if let Some(v) = parse_visibility(extras, warn) {
            seg.visibility = v;
        }
        if seg.config.invalid_progress_width {
            seg.format = PercentFormat::Percent;
        }
        seg
    }

    /// Picks the bucket to render, or `None` to hide.
    ///
    /// Under `Smart` the family match disambiguates and array order is
    /// the tiebreak among survivors. Under `Always` the highest
    /// `percent` wins — arbitrary array order would show whichever the
    /// server happened to list first with no signal a second exists,
    /// whereas the highest is the one that most needs seeing.
    fn select<'a>(&self, limits: &'a [UsageLimit], family: Option<&str>) -> Option<&'a UsageLimit> {
        let scoped = limits.iter().filter(|l| l.kind == LimitKind::WeeklyScoped);
        match self.visibility {
            Visibility::Smart => {
                let family = family?;
                scoped.into_iter().find(|l| {
                    l.scoped_model_name()
                        .is_some_and(|n| n.eq_ignore_ascii_case(family))
                })
            }
            // `total_cmp` needs no NaN fallback: `Percent` rejects NaN
            // at construction, so the newtype's invariant pays off here.
            Visibility::Always => scoped
                .into_iter()
                .max_by(|a, b| a.percent.value().total_cmp(&b.percent.value())),
        }
    }
}

impl Segment for RateLimit7dModelSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let usage = ctx.usage();
        let data = match &*usage {
            Ok(UsageData::Endpoint(e)) => e,
            Ok(UsageData::Jsonl(_)) => {
                crate::lsm_debug!("rate_limit_7d_model: JSONL mode has no per-model split; hiding");
                return Ok(None);
            }
            Err(err) => {
                // No bucket exists here, so the value path's model-name
                // fallback isn't available — and an unlabelled `[Timeout]`
                // is indistinguishable from `rate_limit_7d`'s, which is
                // the whole reason this segment refuses an empty label.
                let mut cfg = self.config.clone();
                if cfg.label.is_empty() {
                    cfg.label = DEFAULT_ERROR_LABEL.to_owned();
                }
                let rendered = RenderedSegment::new(render_error(err, &cfg)).with_role(Role::Info);
                return Ok(Some(rendered));
            }
        };

        let Some(limits) = data.limits.as_deref() else {
            crate::lsm_debug!("rate_limit_7d_model: endpoint sent no `limits`; hiding");
            return Ok(None);
        };

        // An unparseable or absent model id is not evidence of a match,
        // so `Smart` hides rather than guessing.
        let family = ctx
            .status
            .model
            .as_ref()
            .and_then(crate::input::ModelInfo::family);
        let Some(limit) = self.select(limits, family) else {
            crate::lsm_debug!("rate_limit_7d_model: no scoped bucket to render; hiding");
            return Ok(None);
        };

        // The label is the only thing distinguishing this from
        // `rate_limit_7d`, so a bucket that names no model and has no
        // configured label has nothing to render as.
        let mut cfg = self.config.clone();
        if cfg.label.is_empty() {
            let Some(name) = limit.scoped_model_name() else {
                crate::lsm_debug!(
                    "rate_limit_7d_model: bucket names no model and no label is set; hiding"
                );
                return Ok(None);
            };
            cfg.label = name.to_owned();
        }

        let bucket = UsageBucket {
            utilization: limit.percent,
            resets_at: limit.resets_at,
        };
        Ok(Some(render_percent(
            &bucket,
            self.format,
            self.invert,
            &cfg,
        )))
    }

    fn data_deps(&self) -> &'static [DataDep] {
        &[DataDep::Usage]
    }

    fn defaults(&self) -> SegmentDefaults {
        // Deliberately not `rate_limit_7d`'s calendar: with both enabled
        // the two would render identical glyphs, and telling them apart
        // is what this segment's label rules exist for. `✧` is the
        // outline counterpart of the `model` segment's `✦`, so it reads
        // as "a model thing" without colliding with either.
        SegmentDefaults::with_priority(PRIORITY).with_icon("\u{2727}")
    }
}

fn parse_visibility(
    extras: &BTreeMap<String, toml::Value>,
    warn: &mut impl FnMut(&str),
) -> Option<Visibility> {
    let raw = extras.get("visibility")?;
    let Some(s) = raw.as_str() else {
        warn("rate_limit_7d_model.visibility: expected a string; using \"smart\"");
        return None;
    };
    match s {
        "smart" => Some(Visibility::Smart),
        "always" => Some(Visibility::Always),
        other => {
            warn(&format!(
                "rate_limit_7d_model.visibility: unknown value {other:?}; expected \"smart\" or \"always\"; using \"smart\""
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::data_context::{
        EndpointUsage, JsonlUsage, LimitModel, LimitScope, LimitSeverity, SevenDayWindow,
        TokenCounts, UsageError,
    };
    use crate::input::{ModelInfo, Percent, StatusContext, Tool, WorkspaceInfo};

    fn rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn ctx(usage: Result<UsageData, UsageError>, model_id: Option<&str>) -> DataContext {
        let dc = DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
            model: Some(ModelInfo {
                display_name: "Fable 5".into(),
                id: model_id.map(str::to_owned),
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

    fn scoped(model: Option<&str>, pct: f32) -> UsageLimit {
        UsageLimit {
            kind: LimitKind::WeeklyScoped,
            percent: Percent::new(pct).unwrap(),
            resets_at: None,
            scope: Some(LimitScope {
                model: Some(LimitModel {
                    display_name: model.map(str::to_owned),
                    id: None,
                }),
            }),
            severity: LimitSeverity::Warning,
        }
    }

    fn endpoint(limits: Option<Vec<UsageLimit>>) -> UsageData {
        UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            limits,
            unknown_buckets: HashMap::new(),
        })
    }

    fn text(seg: &RateLimit7dModelSegment, dc: &DataContext) -> Option<String> {
        seg.render(dc, &rc())
            .expect("render")
            .map(|r| r.text().to_string())
    }

    fn smart() -> RateLimit7dModelSegment {
        RateLimit7dModelSegment::default()
    }

    fn always() -> RateLimit7dModelSegment {
        RateLimit7dModelSegment {
            visibility: Visibility::Always,
            ..Default::default()
        }
    }

    #[test]
    fn smart_renders_when_the_family_matches() {
        let dc = ctx(
            Ok(endpoint(Some(vec![scoped(Some("Fable"), 82.0)]))),
            Some("claude-fable-5"),
        );
        assert_eq!(text(&smart(), &dc).as_deref(), Some("Fable: 82.0%"));
    }

    #[test]
    fn smart_hides_when_the_family_does_not_match() {
        let dc = ctx(
            Ok(endpoint(Some(vec![scoped(Some("Fable"), 82.0)]))),
            Some("claude-opus-5[1m]"),
        );
        assert_eq!(text(&smart(), &dc), None);
    }

    #[test]
    fn always_renders_a_non_matching_bucket() {
        let dc = ctx(
            Ok(endpoint(Some(vec![scoped(Some("Fable"), 82.0)]))),
            Some("claude-opus-5[1m]"),
        );
        assert_eq!(text(&always(), &dc).as_deref(), Some("Fable: 82.0%"));
    }

    /// The `[1m]` variant marker is stripped, so a 1M-context session
    /// still matches its family's bucket.
    #[test]
    fn smart_matches_through_the_1m_marker() {
        let dc = ctx(
            Ok(endpoint(Some(vec![scoped(Some("Fable"), 82.0)]))),
            Some("claude-fable-5[1m]"),
        );
        assert_eq!(text(&smart(), &dc).as_deref(), Some("Fable: 82.0%"));
    }

    /// `claude-3-5-sonnet-20241022` puts the family fourth. Taking the
    /// second token unconditionally would yield `3` and match nothing.
    #[test]
    fn smart_matches_the_claude_3_generation() {
        let dc = ctx(
            Ok(endpoint(Some(vec![scoped(Some("Sonnet"), 40.0)]))),
            Some("claude-3-5-sonnet-20241022"),
        );
        assert_eq!(text(&smart(), &dc).as_deref(), Some("Sonnet: 40.0%"));
    }

    #[test]
    fn smart_hides_when_the_session_has_no_model_id() {
        let dc = ctx(Ok(endpoint(Some(vec![scoped(Some("Fable"), 82.0)]))), None);
        assert_eq!(text(&smart(), &dc), None);
    }

    #[test]
    fn always_picks_the_highest_percent_among_several() {
        let dc = ctx(
            Ok(endpoint(Some(vec![
                scoped(Some("Fable"), 12.0),
                scoped(Some("Opus"), 91.0),
                scoped(Some("Sonnet"), 55.0),
            ]))),
            Some("claude-fable-5"),
        );
        assert_eq!(text(&always(), &dc).as_deref(), Some("Opus: 91.0%"));
    }

    #[test]
    fn smart_takes_array_order_among_matches() {
        let dc = ctx(
            Ok(endpoint(Some(vec![
                scoped(Some("Fable"), 12.0),
                scoped(Some("Fable"), 91.0),
            ]))),
            Some("claude-fable-5"),
        );
        assert_eq!(text(&smart(), &dc).as_deref(), Some("Fable: 12.0%"));
    }

    #[test]
    fn hides_when_no_scoped_bucket_is_present() {
        let unscoped = UsageLimit {
            kind: LimitKind::WeeklyAll,
            percent: Percent::new(60.0).unwrap(),
            resets_at: None,
            scope: None,
            severity: LimitSeverity::Normal,
        };
        let dc = ctx(Ok(endpoint(Some(vec![unscoped]))), Some("claude-fable-5"));
        assert_eq!(text(&smart(), &dc), None);
        assert_eq!(text(&always(), &dc), None);
    }

    #[test]
    fn hides_when_the_endpoint_sent_no_limits() {
        let dc = ctx(Ok(endpoint(None)), Some("claude-fable-5"));
        assert_eq!(text(&smart(), &dc), None);
        assert_eq!(text(&always(), &dc), None);
    }

    /// Transcripts carry no per-model split, so there is nothing to
    /// degrade to — and `stale_marker` never appears for this segment.
    #[test]
    fn hides_in_jsonl_mode() {
        let jsonl = UsageData::Jsonl(JsonlUsage::new(
            None,
            SevenDayWindow::new(TokenCounts::from_parts(1, 0, 0, 0)),
        ));
        let dc = ctx(Ok(jsonl), Some("claude-fable-5"));
        assert_eq!(text(&smart(), &dc), None);
        assert_eq!(text(&always(), &dc), None);
    }

    /// An unlabelled bare percentage is indistinguishable from
    /// `rate_limit_7d`, so there is nothing safe to render.
    #[test]
    fn always_hides_a_bucket_that_names_no_model_when_no_label_is_set() {
        let dc = ctx(
            Ok(endpoint(Some(vec![scoped(None, 82.0)]))),
            Some("claude-fable-5"),
        );
        assert_eq!(text(&always(), &dc), None);
    }

    #[test]
    fn always_renders_a_bucket_that_names_no_model_when_a_label_is_set() {
        let mut seg = always();
        seg.config.label = "7dm".into();
        let dc = ctx(
            Ok(endpoint(Some(vec![scoped(None, 82.0)]))),
            Some("claude-fable-5"),
        );
        assert_eq!(text(&seg, &dc).as_deref(), Some("7dm: 82.0%"));
    }

    #[test]
    fn a_configured_label_replaces_the_model_name() {
        let mut seg = smart();
        seg.config.label = "7dm".into();
        let dc = ctx(
            Ok(endpoint(Some(vec![scoped(Some("Fable"), 82.0)]))),
            Some("claude-fable-5"),
        );
        assert_eq!(text(&seg, &dc).as_deref(), Some("7dm: 82.0%"));
    }

    /// Spec §Edge cases distinguishes this from `model: None` — an id
    /// we can't read, rather than no model at all. Both hide under
    /// `smart`; both render under `always`.
    #[test]
    fn an_unparseable_model_id_hides_under_smart_and_renders_under_always() {
        let limits = vec![scoped(Some("Fable"), 82.0)];
        let dc = ctx(Ok(endpoint(Some(limits.clone()))), Some("gpt-4o"));
        assert_eq!(text(&smart(), &dc), None);
        assert_eq!(text(&always(), &dc).as_deref(), Some("Fable: 82.0%"));
    }

    #[test]
    fn always_renders_when_the_session_has_no_model_id() {
        let dc = ctx(Ok(endpoint(Some(vec![scoped(Some("Fable"), 82.0)]))), None);
        assert_eq!(text(&always(), &dc).as_deref(), Some("Fable: 82.0%"));
    }

    /// An endpoint failure must not render a bare bracket string: with
    /// `rate_limit_7d` also enabled the line would read
    /// `7d: [Timeout] · [Timeout]`.
    #[test]
    fn the_error_render_is_labelled_even_with_no_bucket_to_name_one() {
        let dc = ctx(Err(UsageError::Timeout), Some("claude-fable-5"));
        let out = text(&smart(), &dc).expect("errors render");
        assert!(out.starts_with("7dm: "), "{out}");
    }

    #[test]
    fn a_configured_label_is_kept_on_the_error_render() {
        let mut seg = smart();
        seg.config.label = "mdl".into();
        let dc = ctx(Err(UsageError::Timeout), Some("claude-fable-5"));
        let out = text(&seg, &dc).expect("errors render");
        assert!(out.starts_with("mdl: "), "{out}");
    }

    #[test]
    fn invert_reaches_the_renderer() {
        let mut seg = smart();
        seg.invert = true;
        let dc = ctx(
            Ok(endpoint(Some(vec![scoped(Some("Fable"), 82.0)]))),
            Some("claude-fable-5"),
        );
        assert_eq!(text(&seg, &dc).as_deref(), Some("Fable: 18.0%"));
    }

    fn extras(pairs: &[(&str, toml::Value)]) -> BTreeMap<String, toml::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[test]
    fn from_extras_parses_visibility() {
        let mut warns = Vec::new();
        let seg = RateLimit7dModelSegment::from_extras(
            &extras(&[("visibility", toml::Value::String("always".into()))]),
            &mut |w| warns.push(w.to_owned()),
        );
        assert_eq!(seg.visibility, Visibility::Always);
        assert!(warns.is_empty(), "{warns:?}");
    }

    /// A silently-ignored typo here looks exactly like the documented
    /// "no scoped bucket" case, so it has to warn.
    #[test]
    fn from_extras_warns_and_defaults_on_an_unknown_visibility() {
        let mut warns = Vec::new();
        let seg = RateLimit7dModelSegment::from_extras(
            &extras(&[("visibility", toml::Value::String("sometimes".into()))]),
            &mut |w| warns.push(w.to_owned()),
        );
        assert_eq!(seg.visibility, Visibility::Smart);
        assert!(warns.iter().any(|w| w.contains("sometimes")), "{warns:?}");
    }

    #[test]
    fn from_extras_warns_on_a_non_string_visibility() {
        let mut warns = Vec::new();
        let seg = RateLimit7dModelSegment::from_extras(
            &extras(&[("visibility", toml::Value::Boolean(true))]),
            &mut |w| warns.push(w.to_owned()),
        );
        assert_eq!(seg.visibility, Visibility::Smart);
        assert!(warns.iter().any(|w| w.contains("visibility")), "{warns:?}");
    }

    /// `is_active` is not modelled, so it cannot be routed into
    /// visibility even by accident — the type enforces the rule.
    #[test]
    fn matching_drives_visibility_not_the_servers_active_flag() {
        // Both buckets deserialize from wire entries whose `is_active`
        // disagrees with the session; only the family match decides.
        let wire = serde_json::json!([
            {"group":"weekly","is_active":false,"kind":"weekly_scoped","percent":82,
             "resets_at":null,"scope":{"model":{"display_name":"Fable","id":null},"surface":null},
             "severity":"warning"},
            {"group":"weekly","is_active":true,"kind":"weekly_scoped","percent":30,
             "resets_at":null,"scope":{"model":{"display_name":"Opus","id":null},"surface":null},
             "severity":"normal"}
        ]);
        let limits: Vec<UsageLimit> = serde_json::from_value(wire).expect("parse");
        let dc = ctx(Ok(endpoint(Some(limits))), Some("claude-fable-5"));
        // is_active=false on the matching bucket: still renders.
        assert_eq!(text(&smart(), &dc).as_deref(), Some("Fable: 82.0%"));
    }
}
