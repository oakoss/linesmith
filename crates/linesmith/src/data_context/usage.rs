//! OAuth `/api/oauth/usage` response + internal usage-data types.
//!
//! Two shapes live here:
//!
//! - [`UsageApiResponse`] mirrors the endpoint's wire JSON per
//!   [ADR-0011](../../../../docs/adrs/0011-rate-limit-data-source.md)
//!   §Endpoint contract. Recognized buckets sit in named `Option`
//!   fields; codenamed/unreleased buckets land in `unknown_buckets`
//!   via `#[serde(flatten)]` so forward-compat is lossless.
//! - [`UsageData`] is what segments consume after the fallback cascade
//!   (endpoint OR JSONL) lands. It adds a [`UsageSource`] tag so
//!   segments can prefix a `stale_marker` on JSONL-sourced values per
//!   `docs/specs/rate-limit-segments.md` §JSONL-fallback display.
//!
//! The endpoint client converts wire → internal via
//! [`UsageApiResponse::into_usage_data`]; the JSONL aggregator builds
//! [`UsageData`] directly with `source = UsageSource::Jsonl`.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::input::Percent;

// --- Wire shape ---------------------------------------------------------

/// Shape of the OAuth `/api/oauth/usage` endpoint response. Every
/// recognized bucket is `Option` because the endpoint omits (or emits
/// `null` for) buckets that don't apply to the account's tier, and
/// `unknown_buckets` captures codenamed / unreleased buckets Anthropic
/// may add without notice (`omelette_*`, `iguana_*`, `cowork`, etc.
/// observed live 2026-04-18). See `docs/research/claude-data-files.md`
/// §Raw data for the reference capture.
#[derive(Debug, Clone, Deserialize, PartialEq)]
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
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
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
#[derive(Debug, Clone, Deserialize, PartialEq)]
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

/// Which path of the fallback cascade produced the [`UsageData`] a
/// segment sees. The endpoint path yields authoritative data; the
/// JSONL fallback is aggregated locally from transcripts and is less
/// rich (no per-model weekly buckets, no `extra_usage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSource {
    /// OAuth `/api/oauth/usage` endpoint.
    Endpoint,
    /// Local JSONL transcript aggregation (ccusage-style 5h blocks).
    Jsonl,
}

/// What [`DataContext::usage`](super::DataContext::usage) surfaces
/// after the cascade in `docs/specs/data-fetching.md` §OAuth fallback
/// cascade finishes. The `source` tag drives the `stale_marker` prefix
/// in rate-limit segment rendering.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct UsageData {
    pub source: UsageSource,
    pub five_hour: Option<UsageBucket>,
    pub seven_day: Option<UsageBucket>,
    pub seven_day_opus: Option<UsageBucket>,
    pub seven_day_sonnet: Option<UsageBucket>,
    pub seven_day_oauth_apps: Option<UsageBucket>,
    pub extra_usage: Option<ExtraUsage>,
}

impl UsageApiResponse {
    /// Drop the forward-compat `unknown_buckets` map and tag the
    /// result with a [`UsageSource`]. The OAuth client calls this with
    /// [`UsageSource::Endpoint`]; the JSONL aggregator builds
    /// [`UsageData`] directly without going through the wire type.
    #[must_use]
    pub fn into_usage_data(self, source: UsageSource) -> UsageData {
        UsageData {
            source,
            five_hour: self.five_hour,
            seven_day: self.seven_day,
            seven_day_opus: self.seven_day_opus,
            seven_day_sonnet: self.seven_day_sonnet,
            seven_day_oauth_apps: self.seven_day_oauth_apps,
            extra_usage: self.extra_usage,
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
    fn into_usage_data_carries_source_and_drops_unknown_buckets() {
        let resp: UsageApiResponse = serde_json::from_str(LIVE_CAPTURE).expect("parse");
        assert_eq!(resp.unknown_buckets.len(), 4);

        let data = resp.into_usage_data(UsageSource::Endpoint);
        assert_eq!(data.source, UsageSource::Endpoint);
        assert!(data.five_hour.is_some());
        assert!(data.seven_day.is_some());
        assert!(data.extra_usage.is_some());
    }

    #[test]
    fn usage_data_built_directly_tags_source_jsonl() {
        let data = UsageData {
            source: UsageSource::Jsonl,
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
        };
        assert_eq!(data.source, UsageSource::Jsonl);
    }
}
