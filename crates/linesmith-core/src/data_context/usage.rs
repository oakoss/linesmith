//! OAuth `/api/oauth/usage` response + internal usage-data types.
//!
//! Two shapes live here:
//!
//! - [`UsageApiResponse`] mirrors the endpoint's wire JSON per
//!   [ADR-0011](../../../../docs/adrs/0011-rate-limit-data-source.md)
//!   §Endpoint contract. Recognized buckets sit in named `Option`
//!   fields; codenamed/unreleased buckets land in `unknown_buckets`
//!   via `#[serde(flatten)]` so forward-compat is lossless.
//! - [`UsageData`] is the enum segments consume after the fallback
//!   cascade lands. Per [ADR-0013](../../../../docs/adrs/0013-jsonl-fallback-carries-token-counts.md),
//!   the variant IS the provenance tag: `Endpoint(EndpointUsage)`
//!   carries authoritative endpoint data; `Jsonl(JsonlUsage)` carries
//!   raw token counts aggregated from transcripts so segments can
//!   render `~5h: 420k` instead of synthesizing a percentage against a
//!   tier ceiling we don't know.
//!
//! The endpoint client converts wire → internal via
//! [`UsageApiResponse::into_endpoint_usage`]; the JSONL-mode cascade
//! constructs a [`JsonlUsage`] directly from the aggregator output in
//! `cascade.rs`.

use std::collections::HashMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::jsonl::TokenCounts;
use crate::input::Percent;

// --- Wire shape ---------------------------------------------------------

/// Shape of the OAuth `/api/oauth/usage` endpoint response. Every
/// recognized bucket is `Option` because the endpoint omits (or emits
/// `null` for) buckets that don't apply to the account's tier, and
/// `unknown_buckets` captures codenamed / unreleased buckets Anthropic
/// may add without notice (`omelette_*`, `iguana_*`, `cowork`, etc.
/// observed live 2026-04-18). See `docs/research/claude-data-files.md`
/// §Raw data for the reference capture.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[non_exhaustive]
pub struct UsageApiResponse {
    #[serde(default)]
    pub five_hour: Option<UsageBucket>,

    #[serde(default)]
    pub seven_day: Option<UsageBucket>,

    #[serde(default)]
    pub seven_day_opus: Option<UsageBucket>,

    #[serde(default)]
    pub seven_day_sonnet: Option<UsageBucket>,

    #[serde(default)]
    pub seven_day_oauth_apps: Option<UsageBucket>,

    #[serde(default)]
    pub extra_usage: Option<ExtraUsage>,

    /// Model-scoped and aggregate limits. Per
    /// [ADR-0030](../../../../docs/adrs/0030-model-scoped-usage-arrives-in-a-limits-array.md)
    /// this is where per-model weekly usage now arrives; the
    /// `seven_day_*` fields above are null in practice.
    ///
    /// `default` is load-bearing alongside `deserialize_with`: the
    /// latter does not imply the former, so without it an omitted
    /// `limits` key fails the whole response with "missing field"
    /// before any per-item tolerance runs.
    #[serde(default, deserialize_with = "deserialize_limits")]
    pub limits: Option<Vec<UsageLimit>>,

    /// Forward-compat catch-all. Any top-level key not matched above
    /// lands here as raw JSON so we preserve what the endpoint sent
    /// even when we don't yet know what to do with it.
    #[serde(flatten)]
    pub unknown_buckets: HashMap<String, serde_json::Value>,
}

/// Names of every recognized top-level field on
/// [`UsageApiResponse`]. Exported so `linesmith doctor` can check
/// "did the endpoint return any forward-compat keys?" without
/// duplicating the field list — the
/// `known_buckets_matches_usage_api_response_fields` test below pins
/// this against `UsageApiResponse` so the two can't drift.
pub const KNOWN_BUCKETS: &[&str] = &[
    "five_hour",
    "seven_day",
    "seven_day_opus",
    "seven_day_sonnet",
    "seven_day_oauth_apps",
    "extra_usage",
    "limits",
];

/// Codenamed forward-compat buckets observed in the live endpoint
/// during research captures (see `docs/research/claude-data-files.md`
/// §Raw data, 2026-04-18 and 2026-08-05 captures). These are unrecognized by
/// `UsageApiResponse`'s strict struct fields but Anthropic ships
/// them on every response — gating the doctor's
/// "endpoint.shape_current" WARN on this list keeps the report quiet
/// on healthy accounts while preserving the WARN for *new* unknown
/// keys (the actual signal a maintainer wants).
///
/// Refresh whenever the research doc captures a new live response.
pub const RESEARCH_DOCUMENTED_BUCKETS: &[&str] = &[
    // 2026-04-18 capture
    "iguana_necktie",
    "omelette_promotional",
    "seven_day_cowork",
    "seven_day_omelette",
    "tangelo",
    // 2026-08-05 capture
    "amber_ladder",
    "cinder_cove",
    "member_dashboard_available",
    "nimbus_quill",
    "spend",
];

/// Utilization plus reset-time for a single rolling window.
///
/// `resets_at` is `Option` because the live endpoint has been observed
/// to emit `null` for codenamed buckets (e.g. `seven_day_omelette`
/// in the 2026-04-18 capture) and we can't rule out the same for
/// recognized buckets under some account states.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
pub struct UsageBucket {
    /// Percent used within the window. Clamped to `[0, 100]` during
    /// deserialization per `rate-limit-segments.md` §Edge cases
    /// ("clamp silently ... defends against unexpected API changes").
    #[serde(deserialize_with = "deserialize_clamped_percent")]
    pub utilization: Percent,

    #[serde(default)]
    pub resets_at: Option<Timestamp>,
}

/// One element of the endpoint's `limits` array.
///
/// Only the fields something consumes are modelled. `group` is
/// derivable from `kind`; `scope.surface` has only ever been `null`, so
/// its populated type is unknown and modelling it as `Option<String>`
/// would drop the whole element under the tolerant deserializer if it
/// turned out to be an object; `is_active` is omitted so
/// `rate-limit-segments.md`'s "not a visibility signal" rule holds
/// structurally rather than by convention — it is a server-side
/// judgement about the account, and nothing constrains it to agree with
/// the local session's model.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[non_exhaustive]
pub struct UsageLimit {
    /// Selection key. `weekly_scoped` is the model-scoped bucket
    /// `rate_limit_7d_model` renders; `session` / `weekly_all`
    /// duplicate `five_hour` / `seven_day` and are ignored.
    pub kind: LimitKind,

    /// Arrives as a JSON integer (`82`) where [`UsageBucket`]'s
    /// `utilization` arrives as a float; the shared helper reads either
    /// and clamps to `[0, 100]`.
    #[serde(deserialize_with = "deserialize_clamped_percent")]
    pub percent: Percent,

    #[serde(default)]
    pub resets_at: Option<Timestamp>,

    /// Populated only for `kind == WeeklyScoped` in every capture to
    /// date. The correlation is not enforced by the type.
    #[serde(default)]
    pub scope: Option<LimitScope>,

    /// Not consulted when rendering — threshold colouring already
    /// derives from `percent`, and honouring both would let two
    /// mechanisms disagree about one number. Modelled so plugins can
    /// reach it.
    #[serde(default)]
    pub severity: LimitSeverity,
}

impl UsageLimit {
    /// The model family this limit is scoped to, or `None` for any limit
    /// that does not name one — wrong `kind`, absent `scope`, absent
    /// `model`, absent or empty `display_name`. Those five are
    /// indistinguishable to every consumer, so the judgement lives here
    /// rather than being re-derived by the segment and again by each
    /// plugin. Empty counts as absent for the same reason it does at the
    /// stdin boundary: a caller that treats `Some("")` as a name renders
    /// a label that isn't there.
    #[must_use]
    pub fn scoped_model_name(&self) -> Option<&str> {
        if self.kind != LimitKind::WeeklyScoped {
            return None;
        }
        self.scope
            .as_ref()?
            .model
            .as_ref()?
            .display_name
            .as_deref()
            .filter(|n| !n.is_empty())
    }
}

/// What a [`UsageLimit`] is scoped to. `#[non_exhaustive]` leaves room
/// for `surface`, which the endpoint sends as `null` today.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[non_exhaustive]
pub struct LimitScope {
    #[serde(default)]
    pub model: Option<LimitModel>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[non_exhaustive]
pub struct LimitModel {
    /// The family name (`"Fable"`), not the stdin `display_name`
    /// (`"Fable 5"`).
    #[serde(default)]
    pub display_name: Option<String>,

    /// `null` in every capture to date. If it ever populates, it retires
    /// the family-token heuristic in `rate-limit-segments.md`
    /// §`rate_limit_7d_model` in favour of an id-to-id comparison.
    #[serde(default)]
    pub id: Option<String>,
}

/// `#[serde(other)]` absorbs a new server-side kind as `Unknown` rather
/// than failing the element; `#[non_exhaustive]` keeps adding a variant
/// from breaking downstream matches, which `serde(other)` does nothing
/// for.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LimitKind {
    Session,
    WeeklyAll,
    WeeklyScoped,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LimitSeverity {
    Normal,
    Warning,
    #[default]
    #[serde(other)]
    Unknown,
}

/// Overage-credit tracking for accounts with extra-usage enabled.
/// `is_enabled` is the load-bearing flag: when `false`, every other
/// field is typically `null` in the live endpoint.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[non_exhaustive]
pub struct ExtraUsage {
    #[serde(default)]
    pub is_enabled: Option<bool>,

    #[serde(default, deserialize_with = "deserialize_optional_clamped_percent")]
    pub utilization: Option<Percent>,

    #[serde(default)]
    pub monthly_limit: Option<f64>,

    #[serde(default)]
    pub used_credits: Option<f64>,

    /// ISO-4217 currency code. Segments render `$` for `"USD"` or
    /// null/missing, and the code as a prefix (e.g. `"EUR 12.50"`)
    /// otherwise, per `rate-limit-segments.md` §Precision and
    /// clamping.
    #[serde(default)]
    pub currency: Option<String>,
}

// --- Internal shape -----------------------------------------------------

/// What [`DataContext::usage`](super::DataContext::usage) surfaces
/// after the cascade in `docs/specs/data-fetching.md` §OAuth fallback
/// cascade finishes. The variant IS the provenance tag per
/// [ADR-0013](../../../../docs/adrs/0013-jsonl-fallback-carries-token-counts.md):
/// segments dispatch on it to pick between percent rendering
/// (endpoint) and raw-token rendering (JSONL). `#[non_exhaustive]`
/// leaves room for a future third source without a SemVer break.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
// `Endpoint` outgrew `Jsonl` by more than clippy's 200-byte threshold
// when ADR-0030 added `limits`. Boxing it is the usual remedy and is
// wrong here: exactly one `UsageData` exists per invocation and it is
// always handed out as `Arc<Result<UsageData, _>>`, so it is already
// behind one indirection and the disparity costs nothing. Boxing would
// add a second hop and break every downstream `Endpoint(e)` pattern for
// no gain.
#[allow(clippy::large_enum_variant)]
pub enum UsageData {
    Endpoint(EndpointUsage),
    Jsonl(JsonlUsage),
}

/// Data from a successful OAuth `/api/oauth/usage` response (possibly
/// served from cache). `unknown_buckets` carries codenamed buckets
/// forward so plugins can inspect them; core segments don't read it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EndpointUsage {
    pub five_hour: Option<UsageBucket>,
    pub seven_day: Option<UsageBucket>,
    pub seven_day_opus: Option<UsageBucket>,
    pub seven_day_sonnet: Option<UsageBucket>,
    pub seven_day_oauth_apps: Option<UsageBucket>,
    pub extra_usage: Option<ExtraUsage>,

    /// Where per-model weekly usage arrives per ADR-0030. The
    /// `seven_day_*` fields above are null in practice.
    pub limits: Option<Vec<UsageLimit>>,

    pub unknown_buckets: HashMap<String, serde_json::Value>,
}

/// Data derived from the JSONL transcript aggregator. `seven_day` is
/// always populated (zero-valued on an empty transcript); `five_hour`
/// is `None` when the current 5h block has no recent activity, per
/// `docs/specs/jsonl-aggregation.md`. Fields are `pub(crate)` so the
/// aggregator+cascade own the construction invariants; segments in
/// this crate read them directly.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonlUsage {
    pub(crate) five_hour: Option<FiveHourWindow>,
    pub(crate) seven_day: SevenDayWindow,
}

impl JsonlUsage {
    #[must_use]
    pub(crate) fn new(five_hour: Option<FiveHourWindow>, seven_day: SevenDayWindow) -> Self {
        Self {
            five_hour,
            seven_day,
        }
    }
}

/// Active-block window surfaced to segments under JSONL fallback.
///
/// # Invariants
///
/// - `ends_at()` is derived as `start + 5h`, so the "block lasts 5
///   hours" invariant is structural rather than prose — the window
///   cannot drift from its anchor after construction.
/// - `start` is expected to be UTC-floor-to-hour in production,
///   matching [`FiveHourBlock::start`] from the aggregator. The
///   cascade honors this precondition; `FiveHourWindow::new` itself
///   does not enforce it because legitimate test fixtures pass
///   mid-hour starts to exercise minute-level countdown rendering
///   that wouldn't occur with a real (hour-aligned) aggregator output.
#[derive(Debug, Clone, PartialEq)]
pub struct FiveHourWindow {
    pub(crate) tokens: TokenCounts,
    pub(crate) start: Timestamp,
}

impl FiveHourWindow {
    #[must_use]
    pub(crate) fn new(tokens: TokenCounts, start: Timestamp) -> Self {
        Self { tokens, start }
    }

    /// Nominal close of the block: `start + 5h`. When the window was
    /// built from a `FiveHourBlock` via the cascade, this equals
    /// [`FiveHourBlock::end`]; otherwise it's just the direct
    /// derivation from whatever `start` the caller passed.
    #[must_use]
    pub(crate) fn ends_at(&self) -> Timestamp {
        self.start + jiff::SignedDuration::from_hours(5)
    }
}

/// Rolling 7-day window under JSONL fallback. No `resets_at`: this is
/// a rolling window, not a hard-reset bucket, so the `rate_limit_7d_reset`
/// segment hides entirely under JSONL per
/// `docs/specs/rate-limit-segments.md` §JSONL-fallback display.
#[derive(Debug, Clone, PartialEq)]
pub struct SevenDayWindow {
    pub(crate) tokens: TokenCounts,
}

impl SevenDayWindow {
    #[must_use]
    pub(crate) fn new(tokens: TokenCounts) -> Self {
        Self { tokens }
    }
}

impl UsageApiResponse {
    /// Convert the wire shape into the internal [`EndpointUsage`].
    /// Unknown buckets are preserved so plugin-facing mirrors can
    /// surface them; the wire `UsageApiResponse` is not retained.
    #[must_use]
    pub fn into_endpoint_usage(self) -> EndpointUsage {
        EndpointUsage {
            five_hour: self.five_hour,
            seven_day: self.seven_day,
            seven_day_opus: self.seven_day_opus,
            seven_day_sonnet: self.seven_day_sonnet,
            seven_day_oauth_apps: self.seven_day_oauth_apps,
            extra_usage: self.extra_usage,
            limits: self.limits,
            unknown_buckets: self.unknown_buckets,
        }
    }
}

// --- Deserializer helpers ----------------------------------------------

/// Per-item-tolerant `limits` reader. One malformed element must not
/// fail the whole response and drop to the JSONL fallback.
///
/// `deserialize_line_entries` in `config.rs` is the closest precedent
/// but only solves the per-item half — it still fails the parse if the
/// value isn't an array. This degrades a non-array to `None` instead,
/// warning unless the value is `null`, which is this endpoint's own
/// idiom for an absent bucket.
fn deserialize_limits<'de, D>(de: D) -> Result<Option<Vec<UsageLimit>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = serde_json::Value::deserialize(de)?;
    let items = match raw {
        serde_json::Value::Null => return Ok(None),
        serde_json::Value::Array(items) => items,
        other => {
            crate::lsm_warn!(
                "usage endpoint sent `limits` as {}, expected an array; ignoring",
                json_kind(&other)
            );
            return Ok(None);
        }
    };

    let mut out = Vec::with_capacity(items.len());
    for (idx, item) in items.into_iter().enumerate() {
        match serde_json::from_value::<UsageLimit>(item) {
            Ok(limit) => out.push(limit),
            Err(e) => crate::lsm_warn!("usage endpoint `limits[{idx}]` unusable, dropping: {e}"),
        }
    }
    Ok(Some(out))
}

fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

fn deserialize_clamped_percent<'de, D>(de: D) -> Result<Percent, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = f64::deserialize(de)?;
    if raw.is_nan() {
        return Err(serde::de::Error::custom("utilization is NaN"));
    }
    let clamped = raw.clamp(0.0, 100.0);
    Percent::from_f64(clamped).ok_or_else(|| {
        serde::de::Error::custom(format!("utilization {raw} failed to clamp into [0, 100]"))
    })
}

fn deserialize_optional_clamped_percent<'de, D>(de: D) -> Result<Option<Percent>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<f64> = Option::deserialize(de)?;
    match raw {
        None => Ok(None),
        Some(v) if v.is_nan() => Err(serde::de::Error::custom("utilization is NaN")),
        Some(v) => {
            let clamped = v.clamp(0.0, 100.0);
            Percent::from_f64(clamped).map(Some).ok_or_else(|| {
                serde::de::Error::custom(format!("utilization {v} failed to clamp into [0, 100]"))
            })
        }
    }
}

// --- Tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire sends `percent` as an integer while `utilization`
    /// arrives as a float; the shared clamp helper has to read both.
    #[test]
    fn limits_percent_accepts_an_integer() {
        let r: UsageApiResponse = serde_json::from_value(serde_json::json!({
            "limits": [{"kind": "weekly_scoped", "percent": 82}]
        }))
        .expect("parse");
        let limits = r.limits.expect("limits");
        assert_eq!(limits[0].percent.value(), 82.0);
    }

    #[test]
    fn limits_percent_clamps_out_of_range() {
        let r: UsageApiResponse = serde_json::from_value(serde_json::json!({
            "limits": [{"kind": "session", "percent": 140}]
        }))
        .expect("parse");
        assert_eq!(r.limits.expect("limits")[0].percent.value(), 100.0);
    }

    /// One bad element must not take the response down to the JSONL
    /// fallback — that would hide every rate-limit segment, not just
    /// this one.
    #[test]
    fn limits_drops_only_the_malformed_element() {
        let r: UsageApiResponse = serde_json::from_value(serde_json::json!({
            "limits": [
                {"kind": "weekly_scoped", "percent": 82},
                {"kind": "weekly_scoped"},
                {"kind": "session", "percent": 5}
            ]
        }))
        .expect("parse");
        let limits = r.limits.expect("limits");
        assert_eq!(limits.len(), 2);
        assert_eq!(limits[0].percent.value(), 82.0);
        assert_eq!(limits[1].kind, LimitKind::Session);
    }

    #[test]
    fn limits_degrades_a_non_array_to_none() {
        let r: UsageApiResponse = serde_json::from_value(serde_json::json!({
            "limits": {"kind": "weekly_scoped"}
        }))
        .expect("parse");
        assert!(r.limits.is_none());
    }

    /// `null` is this endpoint's own idiom for an absent bucket, so it
    /// degrades silently where other non-array shapes warn.
    #[test]
    fn limits_null_and_missing_both_yield_none() {
        let explicit: UsageApiResponse =
            serde_json::from_value(serde_json::json!({"limits": null})).expect("parse");
        assert!(explicit.limits.is_none());

        let absent: UsageApiResponse =
            serde_json::from_value(serde_json::json!({})).expect("parse");
        assert!(absent.limits.is_none());
    }

    #[test]
    fn unrecognized_kind_and_severity_degrade_rather_than_fail() {
        let r: UsageApiResponse = serde_json::from_value(serde_json::json!({
            "limits": [{"kind": "monthly_thing", "percent": 10, "severity": "spicy"}]
        }))
        .expect("parse");
        let limits = r.limits.expect("limits");
        // Distinguishes "degraded to Unknown" from "element dropped":
        // if `#[serde(other)]` failed to match, the element would fail to
        // deserialize and the tolerant reader would drop it, leaving an
        // empty vec rather than an `Unknown` variant.
        assert_eq!(limits.len(), 1, "element was dropped, not degraded");
        assert_eq!(limits[0].kind, LimitKind::Unknown);
        assert_eq!(limits[0].severity, LimitSeverity::Unknown);
    }

    /// The five ways a limit can fail to name a model are one case to
    /// every consumer, so the accessor collapses them.
    #[test]
    fn scoped_model_name_collapses_every_absence() {
        let wire = serde_json::json!([
            {"kind": "session", "percent": 5,
             "scope": {"model": {"display_name": "Fable"}}},
            {"kind": "weekly_scoped", "percent": 1},
            {"kind": "weekly_scoped", "percent": 1, "scope": {}},
            {"kind": "weekly_scoped", "percent": 1, "scope": {"model": {}}},
            {"kind": "weekly_scoped", "percent": 1,
             "scope": {"model": {"display_name": ""}}},
            {"kind": "weekly_scoped", "percent": 1,
             "scope": {"model": {"display_name": "Fable"}}}
        ]);
        let limits: Vec<UsageLimit> = serde_json::from_value(wire).expect("parse");
        let names: Vec<Option<&str>> = limits.iter().map(UsageLimit::scoped_model_name).collect();
        assert_eq!(names, vec![None, None, None, None, None, Some("Fable")]);
    }

    /// Round-tripping through the cache is lossy by design, but must be
    /// stable: an already-`Unknown` kind must not decay further.
    #[test]
    fn limits_survive_a_cache_round_trip() {
        let original: UsageApiResponse = serde_json::from_value(serde_json::json!({
            "limits": [{"kind": "monthly_thing", "percent": 10}]
        }))
        .expect("parse");
        let json = serde_json::to_value(&original).expect("serialize");
        let back: UsageApiResponse = serde_json::from_value(json).expect("reparse");
        assert_eq!(original.limits, back.limits);
    }

    /// `limits` sits alongside `#[serde(flatten)] unknown_buckets`, so
    /// it deserializes through serde's content-buffering path rather
    /// than the direct one. Parse a realistic full body to prove the
    /// combination works and that `limits` does not also leak into the
    /// catch-all — a leak would put it back in doctor's unknown-key WARN.
    #[test]
    fn limits_parses_alongside_the_flatten_catch_all() {
        let body = serde_json::json!({
            "five_hour": {"utilization": 22.0, "resets_at": "2026-08-05T19:50:00Z"},
            "seven_day": {"utilization": 60.0, "resets_at": "2026-08-08T13:59:59Z"},
            "seven_day_opus": null,
            "seven_day_sonnet": null,
            "seven_day_oauth_apps": null,
            "extra_usage": {"is_enabled": false},
            "limits": [{
                "group": "weekly", "is_active": true, "kind": "weekly_scoped",
                "percent": 82, "resets_at": "2026-08-08T14:00:00Z",
                "scope": {"model": {"display_name": "Fable", "id": null}, "surface": null},
                "severity": "warning"
            }],
            "amber_ladder": null,
            "cinder_cove": null,
            "nimbus_quill": null,
            "spend": {"anything": 1},
            "member_dashboard_available": true
        });
        let r: UsageApiResponse = serde_json::from_value(body).expect("full body parses");

        assert_eq!(
            r.limits
                .as_deref()
                .and_then(|l| l.first())
                .and_then(UsageLimit::scoped_model_name),
            Some("Fable")
        );
        assert!(
            !r.unknown_buckets.contains_key("limits"),
            "limits leaked into the catch-all: {:?}",
            r.unknown_buckets.keys().collect::<Vec<_>>()
        );
        assert_eq!(r.five_hour.expect("five_hour").utilization.value(), 22.0);
        assert_eq!(r.unknown_buckets.len(), 5, "the five 2026-08-05 keys");
    }

    /// Tripwire: `KNOWN_BUCKETS` must list every recognized field
    /// on `UsageApiResponse`. If a new bucket lands here without
    /// the const being updated, `linesmith doctor` would WARN
    /// forever on every healthy endpoint response. Build a struct
    /// with all known fields populated and verify a JSON round-trip
    /// produces exactly the expected key set.
    #[test]
    fn known_buckets_matches_usage_api_response_fields() {
        let response = UsageApiResponse {
            five_hour: Some(UsageBucket {
                utilization: Percent::new(0.0).expect("0 percent"),
                resets_at: None,
            }),
            seven_day: Some(UsageBucket {
                utilization: Percent::new(0.0).expect("0 percent"),
                resets_at: None,
            }),
            seven_day_opus: Some(UsageBucket {
                utilization: Percent::new(0.0).expect("0 percent"),
                resets_at: None,
            }),
            seven_day_sonnet: Some(UsageBucket {
                utilization: Percent::new(0.0).expect("0 percent"),
                resets_at: None,
            }),
            seven_day_oauth_apps: Some(UsageBucket {
                utilization: Percent::new(0.0).expect("0 percent"),
                resets_at: None,
            }),
            extra_usage: Some(ExtraUsage {
                is_enabled: Some(false),
                utilization: None,
                monthly_limit: None,
                used_credits: None,
                currency: None,
            }),
            limits: None,
            unknown_buckets: HashMap::new(),
        };
        let value = serde_json::to_value(&response).expect("serialize");
        let mut keys: Vec<String> = value
            .as_object()
            .expect("response is an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        let mut expected: Vec<String> = KNOWN_BUCKETS.iter().map(|s| (*s).to_string()).collect();
        expected.sort();
        assert_eq!(
            keys, expected,
            "KNOWN_BUCKETS drifted from UsageApiResponse; update both lists",
        );
    }

    /// Live `/api/oauth/usage` capture from 2026-04-18 (Max-tier user),
    /// payload-equivalent to `docs/research/claude-data-files.md`
    /// §Raw data (whitespace differs, fields preserved). Keep in sync
    /// if the research doc is refreshed.
    const LIVE_CAPTURE: &str = r#"{
        "five_hour": {
            "utilization": 22.0,
            "resets_at": "2026-04-19T05:00:00.112536+00:00"
        },
        "seven_day": {
            "utilization": 33.0,
            "resets_at": "2026-04-23T19:00:01.112554+00:00"
        },
        "seven_day_oauth_apps": null,
        "seven_day_opus": null,
        "seven_day_sonnet": {
            "utilization": 0.0,
            "resets_at": "2026-04-24T16:00:00.112562+00:00"
        },
        "seven_day_cowork": null,
        "seven_day_omelette": { "utilization": 0.0, "resets_at": null },
        "iguana_necktie": null,
        "omelette_promotional": null,
        "extra_usage": {
            "is_enabled": false,
            "monthly_limit": null,
            "used_credits": null,
            "utilization": null,
            "currency": null
        }
    }"#;

    #[test]
    fn parses_live_capture_losslessly() {
        let resp: UsageApiResponse = serde_json::from_str(LIVE_CAPTURE).expect("parse");

        assert_eq!(resp.five_hour.unwrap().utilization.value(), 22.0);
        assert_eq!(resp.seven_day.unwrap().utilization.value(), 33.0);
        assert_eq!(resp.seven_day_sonnet.unwrap().utilization.value(), 0.0);
        assert!(resp.seven_day_opus.is_none());
        assert!(resp.seven_day_oauth_apps.is_none());

        let extra = resp.extra_usage.unwrap();
        assert_eq!(extra.is_enabled, Some(false));
        assert!(extra.monthly_limit.is_none());
        assert!(extra.currency.is_none());

        // Codenamed buckets land in the catch-all.
        assert_eq!(resp.unknown_buckets.len(), 4);
        for key in [
            "seven_day_cowork",
            "seven_day_omelette",
            "iguana_necktie",
            "omelette_promotional",
        ] {
            assert!(
                resp.unknown_buckets.contains_key(key),
                "expected {key} in unknown_buckets",
            );
        }
    }

    #[test]
    fn parses_empty_response() {
        let resp: UsageApiResponse = serde_json::from_str("{}").expect("parse");
        assert!(resp.five_hour.is_none());
        assert!(resp.seven_day.is_none());
        assert!(resp.extra_usage.is_none());
        assert!(resp.unknown_buckets.is_empty());
    }

    #[test]
    fn injected_codename_lands_in_unknown_buckets() {
        let json = r#"{
            "five_hour": { "utilization": 10.0, "resets_at": "2026-04-19T05:00:00Z" },
            "quokka_experimental": { "utilization": 99.0, "resets_at": null }
        }"#;
        let resp: UsageApiResponse = serde_json::from_str(json).expect("parse");
        assert!(resp.five_hour.is_some());
        assert!(resp.unknown_buckets.contains_key("quokka_experimental"));
    }

    #[test]
    fn bucket_resets_at_accepts_null() {
        let json = r#"{ "utilization": 0.0, "resets_at": null }"#;
        let bucket: UsageBucket = serde_json::from_str(json).expect("parse");
        assert_eq!(bucket.utilization.value(), 0.0);
        assert!(bucket.resets_at.is_none());
    }

    #[test]
    fn utilization_clamps_above_one_hundred() {
        let json = r#"{ "utilization": 150.5, "resets_at": "2026-04-19T05:00:00Z" }"#;
        let bucket: UsageBucket = serde_json::from_str(json).expect("parse");
        assert_eq!(bucket.utilization.value(), 100.0);
    }

    #[test]
    fn utilization_clamps_below_zero() {
        let json = r#"{ "utilization": -5.0, "resets_at": "2026-04-19T05:00:00Z" }"#;
        let bucket: UsageBucket = serde_json::from_str(json).expect("parse");
        assert_eq!(bucket.utilization.value(), 0.0);
    }

    #[test]
    fn utilization_rejects_non_number() {
        let json = r#"{ "utilization": "hello", "resets_at": null }"#;
        assert!(serde_json::from_str::<UsageBucket>(json).is_err());
    }

    #[test]
    fn extra_usage_null_utilization_parses_as_none() {
        let json = r#"{
            "is_enabled": true,
            "utilization": null,
            "monthly_limit": 100.0,
            "used_credits": null,
            "currency": null
        }"#;
        let extra: ExtraUsage = serde_json::from_str(json).expect("parse");
        assert_eq!(extra.is_enabled, Some(true));
        assert!(extra.utilization.is_none());
        assert_eq!(extra.monthly_limit, Some(100.0));
    }

    #[test]
    fn extra_usage_utilization_clamps() {
        let json = r#"{ "utilization": 250.0 }"#;
        let extra: ExtraUsage = serde_json::from_str(json).expect("parse");
        assert_eq!(extra.utilization.unwrap().value(), 100.0);
    }

    #[test]
    fn into_endpoint_usage_preserves_unknown_buckets() {
        // Forward-compat: codenamed buckets survive the wire→internal
        // hop so plugin ctx mirrors can surface them. The pre-ADR-0013
        // shape dropped `unknown_buckets` at this boundary.
        let resp: UsageApiResponse = serde_json::from_str(LIVE_CAPTURE).expect("parse");
        assert_eq!(resp.unknown_buckets.len(), 4);

        let endpoint = resp.into_endpoint_usage();
        assert!(endpoint.five_hour.is_some());
        assert!(endpoint.seven_day.is_some());
        assert!(endpoint.extra_usage.is_some());
        assert_eq!(endpoint.unknown_buckets.len(), 4);
    }

    #[test]
    fn jsonl_usage_smart_ctor_stores_windows() {
        let seven = SevenDayWindow::new(TokenCounts::default());
        let jsonl = JsonlUsage::new(None, seven.clone());
        assert!(jsonl.five_hour.is_none());
        assert_eq!(jsonl.seven_day, seven);
    }
}
