# Rate-Limit Segments

- Status: draft
- Version: 0.3
- Last updated: 2026-08-05
- Driving ADRs: [ADR-0011](../adrs/0011-rate-limit-data-source.md), [ADR-0013](../adrs/0013-jsonl-fallback-carries-token-counts.md), [ADR-0030](../adrs/0030-model-scoped-usage-arrives-in-a-limits-array.md)
- Related specs: [input-schema.md](input-schema.md) (`ModelInfo.id`, added for `rate_limit_7d_model` matching)

## Overview

Rate-limit visibility is the single most-requested feature across the Claude Code statusline community (44+ reactions on anthropics/claude-code#8412). This spec defines the six user-facing segments that surface rate-limit data sourced by [ADR-0011](../adrs/0011-rate-limit-data-source.md): the 5-hour rolling window, the 7-day rolling window, their reset timers, `extra_usage` credit tracking, and the model-scoped weekly bucket.

The spec translates the ADR's data contract (the `UsageData` enum, fallback cascade, error taxonomy) into concrete segment IDs, config schemas, render formats, and error-display rules. The model-scoped weekly bucket is covered here as `rate_limit_7d_model`; tier-aware behavior is deferred to a follow-up spec.

This spec does NOT cover: how `UsageData` is fetched ([data-fetching.md](data-fetching.md)), where OAuth credentials come from ([credentials.md](credentials.md)), or the general segment plugin contract ([segment-system.md](segment-system.md)).

## Requirements

### Functional

Six segment IDs, each opt-in via config:

| Segment ID            | Surfaces                              | Format                          |
| --------------------- | ------------------------------------- | ------------------------------- |
| `rate_limit_5h`       | 5-hour window utilization %           | percent or progress             |
| `rate_limit_7d`       | 7-day window utilization %            | percent or progress             |
| `rate_limit_5h_reset` | Time until the 5-hour window resets   | duration, absolute, or progress |
| `rate_limit_7d_reset` | Time until the 7-day window resets    | duration, absolute, or progress |
| `extra_usage`         | Credits remaining in monthly overage  | currency or percent             |
| `rate_limit_7d_model` | 7-day utilization scoped to one model | percent or progress             |

Behavior requirements:

- All six segments declare `DataDep::Usage` via the `Segment` trait ([data-fetching.md](data-fetching.md) §Segment dependency declaration). They do NOT declare `DataDep::Credentials` directly: the `ctx.usage()` implementation pulls in credentials internally only when it needs to hit the endpoint (cache misses), so fresh-cache hits avoid the Keychain subprocess entirely.
- Each segment reads `ctx.usage()` once per render; never repeats fetch logic
- Render format per segment is config-driven (percent vs progress bar; duration vs progress bar)
- In JSONL mode (`Ok(UsageData::Jsonl(_))`), display shape changes (tokens instead of percent) AND carries a `~` prefix; full signal taxonomy in §JSONL-fallback display
- Error states (`NoCredentials`, `Timeout`, `RateLimited`, `ApiError`, `ParseError`) render explicit text so users can diagnose without checking logs
- `extra_usage` is auto-hidden when `is_enabled = false` — no empty-state placeholder
- `rate_limit_7d_model` reads the model-scoped weekly bucket and hides when none is present, or — under `visibility = "smart"` — when the scoped model is not the one in use. See §`rate_limit_7d_model`.
- The reset segments hide entirely if `resets_at` is missing or in the past (stale data already handled by the cache TTL in ADR-0011)

### Non-functional

- Each segment's render completes in <1ms (data is already in `DataContext`; no I/O in the render path)
- Forward-compat: segments consume only modelled fields. Genuinely unrecognized keys surface as `EndpointUsage::unknown_buckets` and are ignored. A key a segment comes to depend on is promoted out of the catch-all into the typed response model — see `rate_limit_7d_model`, which required this for `limits`
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

[segments.rate_limit_7d_model]
enabled = false
visibility = "smart"        # "smart" | "always"; see §`rate_limit_7d_model`
format  = "percent"         # "percent" | "progress"
invert  = false
progress_width = 20
icon = ""
label = ""                  # "" renders the model's own name; a set value replaces it

[segments.rate_limit_5h_reset]
enabled = false
format  = "duration"        # "duration" | "absolute" | "progress"
compact = false             # false = "4hr 37m"; true = "4h37m"; ignored under "absolute"
use_days = true             # true = "1d 3hr"; false = "27hr"; ignored under "absolute"
progress_width = 20
icon = ""
label = "5h reset"
stale_marker = "~"

# Wall-clock knobs — only consulted when `format = "absolute"`.
# Renders e.g. "5h reset: 7:00 PM PDT" (12h) or "19:00 PDT" (24h).
timezone    = "America/Los_Angeles"   # IANA name; absent = system local via jiff auto-detect
hour_format = "24h"                   # "12h" | "24h"; default 24h
locale      = "en-US"                 # v0.1 ships English-only; unsupported values warn-and-fallback

[segments.rate_limit_7d_reset]
enabled = false
format  = "duration"        # same enum as the 5h variant
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

The render snippets below use `Option<String>` shorthand to focus on the rate-limit-specific formatting logic. The real return type is `RenderResult` with a `RenderedSegment`. The percent/progress segments escalate their color by usage (green→yellow→red, thresholds default 50/80) via the shared `progress_bar` renderer ([segment-system.md](segment-system.md) §RenderedSegment); set `threshold_color = false` for the pre-s0vw flat `Info`. The percent format is a single styled run in that role. The progress bar fans into spans: label + threshold-colored fill + dim trough + threshold-colored percentage. Reset and JSONL/error renders stay a single `Info` run.

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

# Absolute (wall-clock) reset format
5h reset: 7:00 PM PDT       # format = "absolute", hour_format = "12h"
5h reset: 19:00 PDT         # format = "absolute", hour_format = "24h"

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

`rate_limit_7d_model` (label defaults to the bucket's model name):

```text
Fable: 82.0%                      # percent, smart or always
7dm: 82.0%                        # label = "7dm"
Fable: ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░ 82.0% # format = "progress"
Fable: 18%                        # invert = true
                                  # (hidden) smart, model in use is not Fable
                                  # (hidden) no weekly_scoped bucket present
                                  # (hidden) JSONL fallback — no per-model data
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

`render_percent` derives the threshold color from raw utilization, never the `invert`ed display value (a 5%-used "95% remaining" reads green, not red). It applies `invert` and clamps to `[0, 100]` for the displayed value, then renders `percent` text or the shared `progress_bar` (one decimal, `progress_width` cells, `fill`/`brackets`/`dim_empty`/`characters` knobs shared with context_bar). `format_jsonl_tokens` routes the raw token count through `format_tokens` (`420k` / `1.2M` compact form), prepends `stale_marker`, and wraps with `label` / `icon`. The `invert`, `format = "progress"`, and `progress_width` config keys are ignored in JSONL mode: no "used vs remaining" axis exists on a raw count without a ceiling, and a progress bar requires a 0-100 value. JSONL token renders stay flat `Info` (no percentage axis to threshold-color).

#### `rate_limit_7d_model`

Surfaces the weekly bucket the endpoint scopes to a single model — the third
row in Orca-style usage displays (`82% used Fable`).

**This segment requires two changes outside the render layer.** Both are
prerequisites, not implementation detail:

1. **`limits` becomes a modelled field.** The data currently lands in
   `UsageApiResponse`'s `#[serde(flatten)] unknown_buckets` catch-all. Reading it
   from there would contradict [ADR-0030](../adrs/0030-model-scoped-usage-arrives-in-a-limits-array.md)'s
   promote-on-dependency rule and `usage.rs`'s "core segments don't read it", and
   would keep `endpoint.shape_current` warning about a key a shipped segment
   depends on. Add a typed `limits: Option<Vec<UsageLimit>>` to the response
   model and `"limits"` to `KNOWN_BUCKETS`. Typing it also inherits the clamping and degrade-to-`None`
   discipline the other buckets get from serde, which a raw
   `serde_json::Value` read would bypass. `UsageLimit` is defined in
   [data-fetching.md](data-fetching.md) §OAuth usage cache stack.
2. **`ModelInfo` gains `id`.** `smart` mode matches on the model id, and
   `ModelInfo` currently carries only `display_name` — the raw stdin payload
   has `model.id`, but `parse_model` discards it. Add `id: Option<String>`
   with the same degrade-to-`None` handling, amend
   [input-schema.md](input-schema.md), and extend `build_model` in
   `plugins/ctx_mirror.rs` — it emits `display_name` alone today, and
   `ctx.status` is specified as a mirror of `StatusContext`.

Shape of a scoped entry:

```json
{
  "group": "weekly",
  "kind": "weekly_scoped",
  "percent": 82,
  "is_active": true,
  "resets_at": "2026-08-08T14:00:00Z",
  "scope": {
    "model": { "display_name": "Fable", "id": null },
    "surface": null
  },
  "severity": "warning"
}
```

Selection is `kind == "weekly_scoped"`. Siblings in the same array (`session`,
`weekly_all`) duplicate what `rate_limit_5h` / `rate_limit_7d` already read from
`five_hour` / `seven_day`; this segment ignores them.

**`is_active` is not a visibility signal.** The usage request carries no model
or session context — a bearer-token GET against an account-level endpoint,
which cannot know what the local session selected. The field is a server-side
judgement about the account, and nothing constrains it to agree with the
session. `UsageLimit` therefore does not model it — see
[data-fetching.md](data-fetching.md) §OAuth usage cache stack — so the rule
holds structurally rather than by convention.

**Matching, for `visibility = "smart"`.** Compare the family token of
`ctx.status.model.as_ref()?.id` — the first dash-delimited token after
`claude-` that is not purely numeric — against `UsageLimit::scoped_model_name()`,
lower-cased, by exact equality. Observed pairs:

| stdin `model.id`     | stdin `display_name`  | family  | API `display_name` | matches |
| -------------------- | --------------------- | ------- | ------------------ | ------- |
| `claude-fable-5`     | `Fable 5`             | `fable` | `Fable`            | yes     |
| `claude-fable-5[1m]` | `Fable 5`             | `fable` | `Fable`            | yes     |
| `claude-opus-5[1m]`  | `Opus 5 (1M context)` | `opus`  | `Fable`            | no      |

The "skip purely-numeric tokens" clause is what makes the rule survive the
`claude-3-*` generation, where the family sits third or fourth
(`claude-3-5-sonnet-20241022` → `sonnet`, `claude-3-opus-20240229` → `opus`).
Taking the second token unconditionally would yield `3`, which matches no API
`display_name`, and the segment would silently hide for everyone on those ids.

Display names cannot be matched directly: `Fable 5` is not `Fable`, and the
`(1M context)` suffix is applied inconsistently — `claude-opus-4-7[1m]` renders
it while `claude-fable-5[1m]` does not, so it tracks neither the family nor the
`[1m]` marker.

Two properties of this rule are deliberate. It is **exact-equality, not
containment**, so an endpoint that started returning `Claude Opus` or
`Opus 4.7` would stop matching rather than match loosely — the segment hides,
which is the safe direction. And it is **version-blind**: `claude-opus-4-7`
matches a bucket scoped to any Opus. The endpoint has only ever been observed
scoping by family, and a version-aware rule would need a version in
`scope.model` that is not there. If `scope.model.id` is ever populated, prefer
an id-to-id comparison and retire this heuristic.

**Visibility:**

- `smart` (default) — render only when a `weekly_scoped` bucket matches the
  model in use. Hides otherwise, including when `ctx.status.model` is absent or
  its id yields no family token: an unparseable id is not evidence of a match.
- `always` — render whenever a `weekly_scoped` bucket is present, regardless of
  the model in use. The one exception is a bucket that names no model and no
  configured `label`: there is nothing to render it as, so it hides (see
  §Edge cases).

**Several scoped buckets.** Under `smart`, matching disambiguates; if more than
one still matches, render the first in array order (`limits` is a JSON array, so
order is well-defined and stable for a given response). Under `always`, render
the one with the highest `percent` — arbitrary array order would show a user
whichever the server happened to list first, with no signal a second exists,
whereas the highest is the one that most needs seeing. Rendering several would
require a multi-value contract this segment family does not have.

**Render template.** Follows the file's `label: value` convention — `Fable: 82.0%`,
not `82% Fable`. The one-decimal form is the shared percent renderer's, matching
`5h: 22.0%` and `7d: 33.0%`. `label` defaults to `UsageLimit::scoped_model_name()`;
setting it replaces that (`label = "7dm"` → `7dm: 82.0%`). Unlike its siblings,
`label = ""` cannot mean "hide the label", because an unlabelled percentage is
indistinguishable from `rate_limit_7d`; `""` falls back to the model name.

`percent` is already 0-100 and feeds the shared percent/progress rendering
unchanged, threshold-coloured from the raw value like its siblings. `severity`
(`normal` | `warning`) is deliberately not consulted: threshold colouring
already derives from utilization, and honouring both would let two mechanisms
disagree about one number.

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
    let remaining = resets_at.duration_since(jiff::Timestamp::now());
    if remaining <= jiff::SignedDuration::ZERO {
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

| Segment               | Endpoint mode       | JSONL mode                                                   |
| --------------------- | ------------------- | ------------------------------------------------------------ |
| `rate_limit_5h`       | `5h: 22%`           | `~5h: 420k` (via `FiveHourWindow.tokens.total()`)            |
| `rate_limit_7d`       | `7d: 33%`           | `~7d: 1.2M` (via `SevenDayWindow.tokens.total()`)            |
| `rate_limit_5h_reset` | `5h reset: 4hr 37m` | `~5h reset: 4hr 37m` (derived from `FiveHourWindow.ends_at`) |
| `rate_limit_7d_reset` | `7d reset: 2d 14hr` | **hidden** — rolling 7d window has no hard reset             |
| `extra_usage`         | `extra: $12.50`     | **hidden** — no overage data in transcripts                  |
| `rate_limit_7d_model` | varies              | **hidden** — no per-model split in transcripts               |

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
- **No `weekly_scoped` bucket in the response**: `rate_limit_7d_model` hides under both visibility modes. No user-visible error — most accounts have no scoped bucket most of the time.
- **`ctx.status.model` absent from the payload**: hides under `smart` (nothing to match against), renders under `always`. Distinct from an unparseable id — no model at all rather than one we can't read.
- **Malformed `limits` entry** (`percent` missing, non-numeric, or out of range; `scope` present but neither null nor an object — `null` is valid and is what `session` / `weekly_all` carry; `limits` present but not an array): out-of-range `percent` clamps to `[0, 100]` like the other buckets, but the rest needs a **per-item-tolerant deserializer** — a plain `Option<Vec<UsageLimit>>` fails the whole response parse on one bad element, which would drop the endpoint to the JSONL fallback instead of hiding this one segment. Warn-and-drop the offending element and keep the rest, mirroring `deserialize_line_entries` (`crates/linesmith-core/src/config.rs`), which exists for exactly this trap on `LineConfig.segments`. A `limits` value that isn't an array degrades the field to `None`, warning unless it is `null` (null is the endpoint's own idiom for an absent bucket, so warning there would fire on every render). That requires deserializing to `serde_json::Value` first; `deserialize_line_entries` is per-item tolerant but still fails the parse on a non-array.
- **`weekly_scoped` present but the model id has no parseable family token**: hides under `smart` (an unrecognised id is not evidence of a match), renders under `always`.
- **The bucket names no model** — `scope` null, `scope.model` null, or `scope.model.display_name` null: one case, not three. `UsageLimit::scoped_model_name` collapses them, so the segment hides under `smart` (nothing to match against) and under `always` renders with `label` if set, otherwise hides — an unlabelled bare percentage is indistinguishable from `rate_limit_7d`.
- **Model-scoped bucket in JSONL fallback**: hides. Transcripts carry no per-model split, so there is nothing to fall back to; `stale_marker` is therefore never emitted by this segment.
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
  - `rate_limit_7d_model` selection and visibility: a `weekly_scoped` bucket matching the model in use renders under both modes; a non-matching one hides under `smart` and renders under `always`; no scoped bucket hides under both; a model id with no parseable family token hides under `smart`. That `is_active` is never consulted cannot be asserted against a stubbed `UsageLimit` — the type has no such field — so it is tested one layer down instead: deserialize raw endpoint JSON carrying `is_active: false` on a matching bucket and `is_active: true` on a non-matching one, and assert the rendered output tracks the match rather than the flag
  - JSONL-fallback rendering with `UsageData::Jsonl(_)`: assert token-shaped output, `~` prefix, reset derives from `FiveHourWindow.ends_at`, and the segments spec'd to hide (`rate_limit_7d_reset`, `extra_usage`, `rate_limit_7d_model`) actually return `None`

- **Manual test plan:**
  - Enable all six segments in a config; verify rendering on a live Max account
  - Disconnect network; verify JSONL-fallback rendering with `~` prefix
  - Corrupt the `usage.json` cache; verify cache-miss recovery

## Open questions

- **Per-model weekly buckets — resolved 2026-08-05.** Answered by `rate_limit_7d_model` above. The premise was wrong: `seven_day_sonnet` / `seven_day_opus` / `seven_day_oauth_apps` are null in the live response, and the data moved to the `limits` array. Display format follows the 7d segment, the label defaults to the bucket's own model name, and hiding is governed by `visibility`. What remains open is narrower: whether the endpoint can return several `weekly_scoped` buckets at once. Only one has ever been observed, so the spec picks a deterministic single-bucket rule per mode — under `smart` the match disambiguates and array order is the tiebreak; under `always` the highest `percent` wins — rather than inventing a multi-value contract. Revisit if a second is seen.
- **Cost-segment coordination.** `extra_usage` tracks monetary overage; the existing `cost` segment tracks session-level USD spend. Users may want a single combined display. Out of scope for this spec; worth discussing when the cost segment gets a refresh.
- **Icon set.** `icon = ""` defaults to empty. Nerd Font users might want `⏱` or similar; Catppuccin users might want emoji. Leaving `icon` as an arbitrary user string for v0.1; a curated set could come in a theme-bound follow-up.
- **Stale-marker customization per-tier of stale.** If a value is cached up to 180s, users might accept it. If it's 30-minute-old fallback, they might want a louder indicator. v0.1 treats all JSONL-fallback data identically; tiered indication is a v0.2+ refinement.
- **Accessibility / screen-reader hints.** Status lines are read by screen-reader users. Our output is pure text; no ARIA analog in terminal. Worth checking with accessibility-focused users once linesmith has any users.

## Change log

- 2026-08-05 (v0.3): add `rate_limit_7d_model`, the model-scoped weekly bucket
  (`lsm-zgju`). Resolves this spec's deferred per-model open question, whose
  premise had gone stale: it expected `seven_day_sonnet` / `seven_day_opus` /
  `seven_day_oauth_apps`, all three of which are null in the live response, and
  the data moved to a `limits` array. Two prerequisites fall out of that and are
  part of this contract, not implementation detail. `limits` is promoted from
  the `#[serde(flatten)]` catch-all into the typed response model and added to
  `KNOWN_BUCKETS` — reading it as an unknown bucket would have contradicted this
  spec's own forward-compat requirement, left `endpoint.shape_current` warning
  about a key a shipped segment depends on, and bypassed the clamping the other
  buckets inherit from serde. And `ModelInfo` gains `id`: matching needs the
  model id, and only `display_name` was parsed. Two findings shaped the
  contract. The bucket's `is_active` flag cannot drive visibility — the request
  is an account-level bearer-token GET with no model or session context, so
  nothing constrains the flag to agree with the session — and
  `smart` matches locally on the id's family token instead of display names,
  because stdin reports `Fable 5` where the endpoint reports `Fable` and the
  `(1M context)` suffix is applied inconsistently across models; both confirmed
  against ~120k captured stdin records. `severity` is left unconsulted so
  threshold colouring keeps a single source.

- 2026-04-23 (v0.2.1): implementation landed — cascade returns
  `Ok(UsageData::Jsonl(...))` whenever a fetch is attempted and the
  endpoint fails (401, timeout, network error, rate-limited, no
  credentials) and the aggregator produces data. `NoCredentials` and
  `Unauthorized` now fall through to JSONL instead of surfacing the
  error unconditionally (previously blocked on `lsm-xhu`). The
  fresh-cache short-circuit at the top of `resolve_usage` returns
  cached endpoint data without attempting a fetch, so it does NOT
  route through the JSONL fallback — that path serves whatever was
  last cached until the entry goes stale. lsm-jes0 invalidates the
  cache from the 401 arm so peer invocations after a 401 don't keep
  serving stale-token data; the single-process first-invocation
  latency window (token revoked while the cache is still fresh) is
  fundamental at this layer and not addressed by that change.
  `block.start` is clamped to `floor_to_grain(now, 3600)` before
  surfacing so future-dated transcript entries (mild clock skew)
  can't inflate `ends_at` beyond the current window's nominal close.
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
