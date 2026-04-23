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

use chrono::{DateTime, Utc};
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

    /// Forward-compat catch-all. Any top-level key not matched above
    /// lands here as raw JSON so we preserve what the endpoint sent
    /// even when we don't yet know what to do with it.
    #[serde(flatten)]
    pub unknown_buckets: HashMap<String, serde_json::Value>,
}

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
    pub resets_at: Option<DateTime<Utc>>,
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
    pub(crate) start: DateTime<Utc>,
}

impl FiveHourWindow {
    #[must_use]
    pub(crate) fn new(tokens: TokenCounts, start: DateTime<Utc>) -> Self {
        Self { tokens, start }
    }

    /// Nominal close of the block: `start + 5h`. When the window was
    /// built from a `FiveHourBlock` via the cascade, this equals
    /// [`FiveHourBlock::end`]; otherwise it's just the direct
    /// derivation from whatever `start` the caller passed.
    #[must_use]
    pub(crate) fn ends_at(&self) -> DateTime<Utc> {
        self.start + chrono::Duration::hours(5)
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
            unknown_buckets: self.unknown_buckets,
        }
    }
}

// --- Deserializer helpers ----------------------------------------------

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
