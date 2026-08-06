//! [`UsageWindow`] and the per-window `resolve_*_reset` functions
//! collapse the (Endpoint vs JSONL) match dance for the four
//! rate-limit segments into typed calls. Without this seam, each
//! segment open-codes the same nested match against [`UsageData`],
//! differing only in window selector (`five_hour` vs `seven_day`) and
//! hide-reason wording.
//!
//! Reset resolution is split per-window because the return types are
//! asymmetric: 5h has two sources (endpoint `resets_at` or the JSONL
//! block's derived `ends_at()`), while 7d has only the endpoint
//! source. A unified return shape would force an `unreachable!()`
//! arm at the 7d caller; per-window functions express the asymmetry
//! in the type system instead.
//!
//! Returns `Result<_, &'static str>` for every resolution path: the
//! `Err` arm carries the body of the segment's `lsm_debug!` line so
//! the hide-reason wording sits at one site (here), while the
//! segment-name prefix stays at the segment's call site (preserves
//! per-segment ops grep).

use crate::data_context::{UsageBucket, UsageData};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageWindow {
    FiveHour,
    SevenDay,
}

/// Outcome of [`UsageWindow::resolve_percent`] when a percent segment
/// has data to render. Callers route `Endpoint` through
/// `format_percent` and `JsonlTokens` through `format_jsonl_tokens`.
pub(crate) enum WindowResolution<'a> {
    Endpoint(&'a UsageBucket),
    JsonlTokens(u64),
}

/// Outcome of [`resolve_five_hour_reset`]. `Endpoint` carries the
/// bucket-sourced `resets_at`; `JsonlBlockEnd` carries the JSONL
/// block's derived `ends_at()` (= block.start + 5h) and signals
/// stale-marker rendering at the segment.
pub(crate) enum ResetSource {
    Endpoint(jiff::Timestamp),
    JsonlBlockEnd(jiff::Timestamp),
}

impl UsageWindow {
    /// Resolve a percent-segment render path for this window. `Err`
    /// carries the hide-reason body for `lsm_debug!`; callers prepend
    /// the segment name and append `"; hiding"`.
    pub(crate) fn resolve_percent<'a>(
        self,
        data: &'a UsageData,
    ) -> Result<WindowResolution<'a>, &'static str> {
        match (self, data) {
            (Self::FiveHour, UsageData::Endpoint(e)) => e
                .five_hour
                .as_ref()
                .map(WindowResolution::Endpoint)
                .ok_or("endpoint usage.five_hour absent"),
            (Self::SevenDay, UsageData::Endpoint(e)) => e
                .seven_day
                .as_ref()
                .map(WindowResolution::Endpoint)
                .ok_or("endpoint usage.seven_day absent"),
            (Self::FiveHour, UsageData::Jsonl(j)) => j
                .five_hour
                .as_ref()
                .map(|w| WindowResolution::JsonlTokens(w.tokens.total()))
                .ok_or("jsonl five_hour block inactive"),
            // 7d JSONL is always populated (zero-valued on empty
            // transcripts per `docs/specs/jsonl-aggregation.md`), so
            // this arm never hides.
            (Self::SevenDay, UsageData::Jsonl(j)) => {
                Ok(WindowResolution::JsonlTokens(j.seven_day.tokens.total()))
            }
        }
    }
}

/// Resolve the 5h reset path. Returns either the endpoint-sourced
/// `resets_at` or the JSONL block's derived `ends_at()`. Callers gate
/// the resulting timestamp with `remaining > 0`; the `remaining` check
/// would couple this function to wall-clock time, so it lives at the
/// segment.
pub(crate) fn resolve_five_hour_reset(data: &UsageData) -> Result<ResetSource, &'static str> {
    match data {
        UsageData::Endpoint(e) => {
            let bucket = e
                .five_hour
                .as_ref()
                .ok_or("endpoint usage.five_hour absent")?;
            bucket
                .resets_at
                .map(ResetSource::Endpoint)
                .ok_or("five_hour.resets_at absent")
        }
        UsageData::Jsonl(j) => {
            let window = j
                .five_hour
                .as_ref()
                .ok_or("jsonl five_hour block inactive")?;
            Ok(ResetSource::JsonlBlockEnd(window.ends_at()))
        }
    }
}

/// Resolve the 7d reset path. Always returns the endpoint-sourced
/// `resets_at` on success; the return type is `jiff::Timestamp` (not
/// `ResetSource`) because ADR-0013 rejects synthesizing a 7d reset
/// timestamp under JSONL — the rolling 7d window has no hard reset,
/// and the signature encodes that. JSONL data always returns `Err`.
pub(crate) fn resolve_seven_day_reset(data: &UsageData) -> Result<jiff::Timestamp, &'static str> {
    match data {
        UsageData::Endpoint(e) => {
            let bucket = e
                .seven_day
                .as_ref()
                .ok_or("endpoint usage.seven_day absent")?;
            bucket.resets_at.ok_or("seven_day.resets_at absent")
        }
        UsageData::Jsonl(_) => Err("jsonl fallback has no hard reset"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_context::{EndpointUsage, JsonlUsage, SevenDayWindow, TokenCounts, UsageData};

    #[test]
    fn seven_day_reset_returns_err_under_jsonl() {
        // Pins ADR-0013's invariant: the 7d rolling window has no hard
        // reset under JSONL. If a future contributor adds a JSONL arm
        // that synthesizes a timestamp, this test fails before the
        // signature's `Result<jiff::Timestamp, _>` shape can be silently
        // relaxed back into a sum type.
        let data = UsageData::Jsonl(JsonlUsage::new(
            None,
            SevenDayWindow::new(TokenCounts::default()),
        ));
        assert_eq!(
            resolve_seven_day_reset(&data),
            Err("jsonl fallback has no hard reset"),
        );
    }

    #[test]
    fn seven_day_reset_returns_err_when_endpoint_bucket_absent() {
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            limits: None,
            unknown_buckets: std::collections::HashMap::new(),
        });
        assert_eq!(
            resolve_seven_day_reset(&data),
            Err("endpoint usage.seven_day absent"),
        );
    }
}
