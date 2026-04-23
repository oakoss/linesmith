# Rate-Limit Segments

- Status: draft
- Version: 0.2.1
- Last updated: 2026-04-23
- Driving ADRs: [ADR-0011](../adrs/0011-rate-limit-data-source.md), [ADR-0013](../adrs/0013-jsonl-fallback-carries-token-counts.md)

## Overview

Rate-limit visibility is the single most-requested feature across the Claude Code statusline community (44+ reactions on anthropics/claude-code#8412). This spec defines the five user-facing segments that surface rate-limit data sourced by [ADR-0011](../adrs/0011-rate-limit-data-source.md): the 5-hour rolling window, the 7-day rolling window, their reset timers, and `extra_usage` credit tracking.

The spec translates the ADR's data contract (`UsageData` struct, fallback cascade, error taxonomy) into concrete segment IDs, config schemas, render formats, and error-display rules. Per-model weekly buckets (`rate_limit_7d_sonnet`, `rate_limit_7d_opus`) and tier-aware behavior are deferred to follow-up specs.

This spec does NOT cover: how `UsageData` is fetched ([data-fetching.md](data-fetching.md)), where OAuth credentials come from ([credentials.md](credentials.md)), or the general segment plugin contract ([segment-system.md](segment-system.md)).

## Requirements

### Functional

Five segment IDs, each opt-in via config:

| Segment ID            | Surfaces                             | Format               |
| --------------------- | ------------------------------------ | -------------------- |
| `rate_limit_5h`       | 5-hour window utilization %          | percent or progress  |
| `rate_limit_7d`       | 7-day window utilization %           | percent or progress  |
| `rate_limit_5h_reset` | Time until the 5-hour window resets  | duration or progress |
| `rate_limit_7d_reset` | Time until the 7-day window resets   | duration or progress |
| `extra_usage`         | Credits remaining in monthly overage | currency or percent  |

Behavior requirements:

- All five segments declare `DataDep::Usage` via the `Segment` trait ([data-fetching.md](data-fetching.md) §Segment dependency declaration). They do NOT declare `DataDep::Credentials` directly: the `ctx.usage()` implementation pulls in credentials internally only when it needs to hit the endpoint (cache misses), so fresh-cache hits avoid the Keychain subprocess entirely.
- Each segment reads `ctx.usage()` once per render; never repeats fetch logic
- Render format per segment is config-driven (percent vs progress bar; duration vs progress bar)
- In JSONL mode (`Ok(UsageData::Jsonl(_))`), display shape changes (tokens instead of percent) AND carries a `~` prefix; full signal taxonomy in §JSONL-fallback display
- Error states (`NoCredentials`, `Timeout`, `RateLimited`, `ApiError`, `ParseError`) render explicit text so users can diagnose without checking logs
- `extra_usage` is auto-hidden when `is_enabled = false` — no empty-state placeholder
- The reset segments hide entirely if `resets_at` is missing or in the past (stale data already handled by the cache TTL in ADR-0011)

### Non-functional

- Each segment's render completes in <1ms (data is already in `DataContext`; no I/O in the render path)
- Forward-compat: segments consume only documented fields; new Anthropic codenamed buckets surface as `EndpointUsage::unknown_buckets` and are ignored
- Stable config keys: renaming a segment ID is a breaking change and warrants a v0.2 migration note
- No allocation-per-render beyond the output `String`

## Interface / Contract

### Config schema

Segments are enabled in the main `[line.segments]` array ([config.md](config.md)) and optionally configured in per-segment tables. Defaults shown below.

```toml
[segments.rate_limit_5h]
enabled = true
format  = "percent"         # "percent" | "progress"; ignored in JSONL mode (tokens only)
invert  = false             # false = show elapsed/used; true = show remaining; ignored in JSONL mode
progress_width = 20         # cells, when format = "progress"; ignored in JSONL mode
icon = ""                   # optional prefix (empty string = no icon)
label = "5h"                # label text; "" hides the label entirely
stale_marker = "~"          # prefix for JSONL-mode values

[segments.rate_limit_7d]
enabled = true
format  = "percent"         # ignored in JSONL mode
invert  = false             # ignored in JSONL mode
progress_width = 20         # ignored in JSONL mode
icon = ""
label = "7d"
stale_marker = "~"

[segments.rate_limit_5h_reset]
enabled = false
format  = "duration"        # "duration" | "progress"
compact = false             # false = "4hr 37m"; true = "4h37m"
use_days = true             # true = "1d 3hr"; false = "27hr"
progress_width = 20
icon = ""
label = "5h reset"
stale_marker = "~"

[segments.rate_limit_7d_reset]
enabled = false
format  = "duration"
compact = false
use_days = true
progress_width = 20
icon = ""
label = "7d reset"
stale_marker = "~"

[segments.extra_usage]
enabled = false
format  = "currency"        # "currency" | "percent"
# When format = "currency", values are rendered as "$X.XX"
# When format = "percent", rendered as "{utilization}%"
icon = ""
label = "extra"
stale_marker = "~"
```

`invert` swaps the rendered value between elapsed (default) and remaining for the percent/progress segments. A user who thinks of "5h: 78% left" instead of "5h: 22% used" sets `invert = true`.

### Render output contract

Rate-limit segments implement the canonical `Segment` trait from [segment-system.md](segment-system.md) under its v0.3 signature (tracked in lsm-thm): `render(&self, ctx: &DataContext) -> RenderResult`. `Ok(None)` hides, `Ok(Some(RenderedSegment))` renders, `Err(SegmentError)` surfaces a failure (logged to stderr, segment hidden). `StatusContext` is accessible via `ctx.status`; `UsageData` via `ctx.usage()`. Rate-limit segments additionally declare `data_deps()` per [data-fetching.md](data-fetching.md):

```rust
fn data_deps(&self) -> &'static [DataDep] {
    &[DataDep::Usage]
}
```

Credentials are a dependency of `ctx.usage()`'s internal endpoint-fetch path, not of the segment. Declaring only `Usage` lets the runtime's lazy loader skip credential resolution on cache hits.

The render snippets below use `Option<String>` shorthand to focus on the rate-limit-specific formatting logic. The real return type is `RenderResult` with `RenderedSegment { runs, width, right_separator }`; the string content becomes one `StyledRun` with the segment's configured `role` (see [theming.md](theming.md)).

### Render examples

```text
# Happy-path with default formatting
5h: 22%
7d: 33%
5h reset: 4hr 37m
7d reset: 4d 8hr

# Progress bar format
5h: ████░░░░░░░░░░░░░░░░ 22%
7d: ██████░░░░░░░░░░░░░░ 33%
5h reset: ██░░░░░░░░░░░░░░░░░░  7%
7d reset: ████████░░░░░░░░░░░░ 40%

# Inverted (remaining instead of used)
5h: 78%
7d: 67%

# JSONL mode (endpoint unreachable) — raw token counts, no synthesized percentage
~5h: 420k
~7d: 1.2M
~5h reset: 2hr 43m                # derived from FiveHourWindow.ends_at

# Error states
5h: [No credentials]
5h: [Timeout]
5h: [Rate limited]
5h: [API Error]
5h: [Parse Error]

# extra_usage when enabled with currency format
extra: $12.50

# extra_usage when not enabled → segment returns None (hidden)
```

### Render semantics per segment

#### `rate_limit_5h` and `rate_limit_7d`

```rust
fn render(&self, ctx: &DataContext) -> Option<String> {
    let usage = ctx.usage();            // Arc<Result<UsageData, UsageError>>
    match &*usage {
        Ok(UsageData::Endpoint(e)) => {
            let bucket = if self.id() == "rate_limit_5h" {
                e.five_hour.as_ref()
            } else {
                e.seven_day.as_ref()
            };
            bucket.map(|b| self.format_percent(b))
        }
        Ok(UsageData::Jsonl(j)) => {
            // 5h: None when no activity in the current block → hide.
            // 7d: JsonlUsage::seven_day is always populated (zero-valued
            // on an empty transcript), so no early-hide here.
            let tokens = match self.id() {
                "rate_limit_5h" => j.five_hour.as_ref()?.tokens.total(),
                "rate_limit_7d" => j.seven_day.tokens.total(),
                _ => unreachable!(),
            };
            Some(self.format_jsonl_tokens(tokens))
        }
        Err(e) => Some(self.render_error(e)),
    }
}
```

`format_percent` applies `invert`, clamps to `[0, 100]`, picks `percent` or `progress` format, and wraps with `label` / `icon`. `format_jsonl_tokens` routes the raw token count through `format_tokens` (`420k` / `1.2M` compact form), prepends `stale_marker`, and wraps with `label` / `icon`. The `invert`, `format = "progress"`, and `progress_width` config keys are ignored in JSONL mode: no "used vs remaining" axis exists on a raw count without a ceiling, and a progress bar requires a 0-100 value.

#### `rate_limit_5h_reset` and `rate_limit_7d_reset`

```rust
fn render(&self, ctx: &DataContext) -> Option<String> {
    let usage = ctx.usage();
    let (resets_at, is_jsonl) = match &*usage {
        Ok(UsageData::Endpoint(e)) => {
            let bucket = match self.id() {
                "rate_limit_5h_reset" => e.five_hour.as_ref()?,
                "rate_limit_7d_reset" => e.seven_day.as_ref()?,
                _ => unreachable!(),
            };
            (bucket.resets_at?, false)
        }
        Ok(UsageData::Jsonl(j)) => match self.id() {
            // 5h reset derives from FiveHourWindow.ends_at (= block.start + 5h).
            "rate_limit_5h_reset" => (j.five_hour.as_ref()?.ends_at, true),
            // 7d is a rolling window in JSONL mode — no hard reset.
            "rate_limit_7d_reset" => return None,
            _ => unreachable!(),
        },
        Err(e) => return Some(self.render_error(e)),
    };
    let remaining = resets_at.signed_duration_since(chrono::Utc::now());
    if remaining <= chrono::Duration::zero() {
        return None;  // already reset; stale data, hide
    }
    Some(self.format_duration(remaining, is_jsonl))
}
```

#### `extra_usage`

```rust
fn render(&self, ctx: &DataContext) -> Option<String> {
    let usage = ctx.usage();
    match &*usage {
        Ok(UsageData::Endpoint(e)) => {
            let extra = e.extra_usage.as_ref()?;
            if !extra.is_enabled.unwrap_or(false) {
                return None; // account-level disabled → hide (no error)
            }
            Some(self.format_extra_usage(extra))
        }
        // JSONL transcripts carry no overage data; hide silently.
        Ok(UsageData::Jsonl(_)) => None,
        Err(e) => Some(self.render_error(e)),
    }
}
```

The hide-on-error behavior is deliberately scoped: `extra_usage` hides when the account has not enabled overage (`is_enabled = false`) or when the fallback path is JSONL-only (no overage data in transcripts), not when the fetch fails. A user who enables the segment in their config has opted in to see its state, so endpoint/credential failures render the same `[No credentials]` / `[Timeout]` / `[Keychain error]` strings as the other rate-limit segments. Silent hide on fetch failure would make regressions indistinguishable from the "overage not enabled" case.

### Error message table

Maps `UsageError` variants to rendered strings:

| `UsageError`                           | Rendered                   | When                                                                                                               |
| -------------------------------------- | -------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `NoCredentials`                        | `[No credentials]`         | No OAuth token found in any cascade path AND JSONL also empty                                                      |
| `SubprocessFailed`                     | `[Keychain error]`         | macOS `security` subprocess failed AND no file fallback succeeded                                                  |
| `IoError`                              | `[Credentials unreadable]` | Credentials file present but unreadable (permission, IO failure)                                                   |
| `Timeout`                              | `[Timeout]`                | Endpoint took >2s AND no stale cache AND JSONL empty                                                               |
| `RateLimited`                          | `[Rate limited]`           | Endpoint returned 429 AND no stale cache AND JSONL empty                                                           |
| `NetworkError`                         | `[Network error]`          | Connection failed AND no stale cache AND JSONL empty                                                               |
| `ParseError`                           | `[Parse error]`            | Endpoint returned malformed JSON                                                                                   |
| `Unauthorized`                         | `[Unauthorized]`           | Endpoint returned 401 (token expired or revoked) AND JSONL empty                                                   |
| `Jsonl(NoEntries \| DirectoryMissing)` | `[No data]`                | Reserved for future direct-JSONL segments; today only reachable if the endpoint layer wraps a `Jsonl` error itself |
| `Jsonl(IoError \| ParseError)`         | `[Parse error]`            | Same as above — aggregator systemic failures collapse to `NoEntries` at the cascade boundary with a `warn!` trace  |

Error strings are intentionally concise to fit within typical statusline widths. Users run `linesmith doctor` for full diagnostics.

## Behavior

### Rendering flow

1. Runtime computes the union of `DataDep`s across enabled segments. `DataDep::Usage` is pulled in by any enabled rate-limit segment; credentials are resolved transitively by `ctx.usage()` only on the endpoint-fetch path.
2. Runtime calls `ctx.usage()` once; the fallback cascade in [ADR-0011](../adrs/0011-rate-limit-data-source.md) runs.
3. Runtime invokes each segment's `render()`. Each rate-limit segment reads the same cached `Arc<Result<UsageData, UsageError>>`.
4. Returned `Option<String>` values pass through layout ([segment-system.md](segment-system.md)) for truncation/priority.

### Precision and clamping

- Percent values: clamp to `[0, 100]`, format to one decimal place (e.g., `22.0%`).
- Duration values: floor to minute precision, never show "0m" (use "<1m" instead). Max unit honored by `use_days` config.
- Currency values: `extra_usage.monthly_limit - used_credits` → dollars with 2 decimal places; never show negative (clamp to `$0.00`). If `extra_usage.currency` is present and not `"USD"`, render the ISO code prefix instead of `$` (e.g. `"EUR 12.50"`); if `currency` is null/missing, default to `$`. v0.1 does not do live FX conversion — we report the currency Anthropic returns.

### JSONL-fallback display

When `ctx.usage()` returns `Ok(UsageData::Jsonl(...))`, segments render a different shape: raw token counts via the `format_tokens` helper (`420k`, `1.2M`), still with the `~` prefix. Two signals — a shape change AND a prefix — so the mode switch survives `NO_COLOR`, 16-color terminals, or a user-set `stale_marker = ""`.

Per-segment behavior:

| Segment                                     | Endpoint mode       | JSONL mode                                                   |
| ------------------------------------------- | ------------------- | ------------------------------------------------------------ |
| `rate_limit_5h`                             | `5h: 22%`           | `~5h: 420k` (via `FiveHourWindow.tokens.total()`)            |
| `rate_limit_7d`                             | `7d: 33%`           | `~7d: 1.2M` (via `SevenDayWindow.tokens.total()`)            |
| `rate_limit_5h_reset`                       | `5h reset: 4hr 37m` | `~5h reset: 4hr 37m` (derived from `FiveHourWindow.ends_at`) |
| `rate_limit_7d_reset`                       | `7d reset: 2d 14hr` | **hidden** — rolling 7d window has no hard reset             |
| `extra_usage`                               | `extra: $12.50`     | **hidden** — no overage data in transcripts                  |
| Per-model (`seven_day_sonnet` etc., future) | varies              | **hidden** — no per-model split in transcripts               |

Why raw tokens and not a synthesized percentage: tier detection is out of scope ([ADR-0011](../adrs/0011-rate-limit-data-source.md) §Tier handling (out of scope)), and faking a percentage against a Max-tier ceiling would ship the wrong number to every Pro/Free user who landed on the fallback path. ccstatusline handles this by emitting its error tags literally (`[No credentials]`, `[Rate limited]`) and CCometixLine by hiding the segment. linesmith's divergence is to show useful partial data instead of nothing; tokens are the unit the aggregator produces without tier inference.

The `rate_limit_5h_reset` JSONL-mode timestamp is `FiveHourWindow.ends_at` (= `block.start + 5h`), matching ccstatusline's `getUsageWindowFromBlockMetrics`. The aggregator also exposes `FiveHourBlock.usage_limit_reset` ([jsonl-aggregation.md](jsonl-aggregation.md)), but its provenance is unverified and segments do not consume it; `lsm-ghpj` tracks verification before wiring.

### Staleness bounds

The data-fetching layer already enforces a 180s default TTL. Segments don't independently check staleness — they render whatever `ctx.usage()` returns. If the user configures `usage.cache_duration = 3600` (see [config.md](config.md) §Top-level schema), stale-up-to-an-hour values render without warning. That tradeoff is exposed and owned at the config layer.

## Edge cases

- **All buckets null in endpoint response**: `data.five_hour`, `data.seven_day`, `data.extra_usage` all `None`. The two reset segments hide; the utilization segments hide; `extra_usage` hides. No user-visible error.
- **`resets_at` in the past**: reset segment hides (data is stale; segment will re-render next prompt with fresh data).
- **`utilization` negative or >100**: clamp silently. Anthropic should never emit these; clamp defends against unexpected API changes.
- **`extra_usage.monthly_limit` missing while `is_enabled = true`**: cannot compute currency remaining. Fall back to `percent` format using `utilization`; if `utilization` also missing, hide.
- **Progress width 0 or negative**: treat as invalid config; fall back to `percent` format and emit a warning at startup via `linesmith doctor`.
- **Label set to a multi-byte emoji**: fine; the terminal layout engine handles grapheme widths ([segment-system.md](segment-system.md) §Layout).
- **Duration >= 100 days**: very unlikely (the 7-day window caps at 7d), but clamp format to max 4 digit days.
- **`stale_marker` set to a non-printable character**: accepted as configured; the terminal library will render it or not. No sanitization.
- **Multiple rate-limit segments enabled in one line**: each reads the same `Arc<UsageData>`; no duplicate I/O. Output: `5h: 22% · 7d: 33% · 5h reset: 4hr 37m` etc.

## Testing strategy

- **Snapshot tests** per segment, per format variant, per `invert` setting:
  - `rate_limit_5h` percent happy path
  - `rate_limit_5h` progress bar at 0%, 50%, 100%
  - `rate_limit_5h_reset` at various remaining durations (1m, 1h, 1d, 1d12h)
  - `extra_usage` currency with varying credits used
  - Each error state rendered for each segment
  - JSONL-fallback marker applied

- **Unit tests:**
  - Duration formatting: `compact`, `use_days`, edge cases
  - Currency formatting: negative clamp, 2-decimal precision
  - Clamp logic: negative utilization, >100 utilization

- **Integration tests:**
  - Full render with a stubbed `DataContext` (prepopulated `UsageData`); assert output exactly matches expected
  - Error-state rendering with stubbed `UsageError`
  - JSONL-fallback rendering with `UsageData::Jsonl(_)`: assert token-shaped output, `~` prefix, reset derives from `FiveHourWindow.ends_at`, and the segments spec'd to hide (`rate_limit_7d_reset`, `extra_usage`, per-model) actually return `None`

- **Manual test plan:**
  - Enable all five segments in a config; verify rendering on a live Max account
  - Disconnect network; verify JSONL-fallback rendering with `~` prefix
  - Corrupt the `usage.json` cache; verify cache-miss recovery

## Open questions

- **Per-model weekly buckets.** The endpoint returns `seven_day_sonnet`, `seven_day_opus`, `seven_day_oauth_apps`. Exposing them as `rate_limit_7d_sonnet` etc. is appealing but requires decisions on: display format (same as 7d?), label conventions (full model name or abbreviated?), hiding rules (hide when null, which happens for most models per user). Defer to a follow-up spec once we have user demand signal.
- **Cost-segment coordination.** `extra_usage` tracks monetary overage; the existing `cost` segment tracks session-level USD spend. Users may want a single combined display. Out of scope for this spec; worth discussing when the cost segment gets a refresh.
- **Icon set.** `icon = ""` defaults to empty. Nerd Font users might want `⏱` or similar; Catppuccin users might want emoji. Leaving `icon` as an arbitrary user string for v0.1; a curated set could come in a theme-bound follow-up.
- **Stale-marker customization per-tier of stale.** If a value is cached up to 180s, users might accept it. If it's 30-minute-old fallback, they might want a louder indicator. v0.1 treats all JSONL-fallback data identically; tiered indication is a v0.2+ refinement.
- **Accessibility / screen-reader hints.** Status lines are read by screen-reader users. Our output is pure text; no ARIA analog in terminal. Worth checking with accessibility-focused users once linesmith has any users.

## Change log

- 2026-04-23 (v0.2.1): implementation landed — cascade returns
  `Ok(UsageData::Jsonl(...))` on every endpoint-failure path (401,
  timeout, network error, rate-limited, no credentials) when the
  aggregator produces data. `NoCredentials` and `Unauthorized` now
  fall through to JSONL instead of surfacing the error unconditionally
  (previously blocked on `lsm-xhu`). `block.start` is clamped to
  `floor_to_hour(now)` before surfacing so future-dated transcript
  entries (mild clock skew) can't inflate `ends_at` beyond the
  current window's nominal close.
- 2026-04-22 (v0.2): JSONL mode renders raw `TokenCounts` per
  [ADR-0013](../adrs/0013-jsonl-fallback-carries-token-counts.md).
  `UsageData` is an enum (`Endpoint` / `Jsonl`); `UsageSource`
  deleted. `rate_limit_5h` / `_7d` render `~420k` / `~1.2M` under
  JSONL; `rate_limit_5h_reset` derives its timestamp from
  `FiveHourWindow.ends_at`; `rate_limit_7d_reset`, `extra_usage`,
  and future per-model segments hide. `invert` /
  `format = "progress"` / `progress_width` config keys are ignored
  in JSONL mode.
- 2026-04-19: initial draft (v0.1). Defines five segment IDs, their config schemas, render formats (percent/progress/duration/currency), error-state rendering table, JSONL-fallback marker convention, and render semantics. Driven by ADR-0011; cross-references data-fetching.md and credentials.md.
