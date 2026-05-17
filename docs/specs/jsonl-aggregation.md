# JSONL Aggregation

- Status: draft
- Version: 0.3
- Last updated: 2026-05-16
- Driving ADRs: [ADR-0009](../adrs/0009-json-parsing-stack.md), [ADR-0010](../adrs/0010-data-fetching-architecture.md), [ADR-0011](../adrs/0011-rate-limit-data-source.md), [ADR-0013](../adrs/0013-jsonl-fallback-carries-token-counts.md)

## Overview

The JSONL aggregator is the terminal fallback for the rate-limit data pipeline. When the OAuth `/api/oauth/usage` endpoint is unreachable (no credentials, revoked token, network down, active lock without stale cache), the cascade in [data-fetching.md](data-fetching.md) §OAuth fallback cascade drops to reading Claude Code's own transcript files under `~/.claude/projects/**/*.jsonl`. This spec defines the aggregator's types, record shape, block-math semantics, and project-root discovery.

The aggregator is a read-only port of the math from [`ryoppippi/ccusage`](https://github.com/ryoppippi/ccusage)'s `_session-blocks.ts` (MIT). It produces raw token counts and block boundaries — not rate-limit utilization percentages. The orchestrator wraps the aggregator's output in [`UsageData::Jsonl`](data-fetching.md#oauth-usage-cache-stack) per [ADR-0013](../adrs/0013-jsonl-fallback-carries-token-counts.md); segments render raw `TokenCounts` in JSONL mode rather than synthesizing a percentage against a tier ceiling.

This spec does NOT cover: the `ctx.usage()` fallback orchestration (lives in [data-fetching.md](data-fetching.md) §OAuth fallback cascade), segment rendering of JSONL-derived values ([rate-limit-segments.md](rate-limit-segments.md) §JSONL-fallback display), or the `JsonlTailer` byte-offset incremental reader (covered in [data-fetching.md](data-fetching.md) §JSONL incremental tail).

## Requirements

### Functional

- Discover project directories across the `$CLAUDE_CONFIG_DIR/projects/` / `~/.config/claude/projects/` / `~/.claude/projects/` cascade; every root that exists contributes
- Parse each `*.jsonl` file under `projects/` as a sequence of newline-delimited JSON records
- Deduplicate entries by `message.id` — Claude Code writes duplicates on retries / edits per [research/jsonl-data-source.md](../research/jsonl-data-source.md) §Open questions
- Compute the **active 5-hour billing block**: start time is the UTC-floor-to-hour of the first entry in the block; end is `start + 5h`; a new block begins when the gap from the previous entry exceeds 5h
- Compute the **rolling 7-day window**: all entries whose `timestamp` falls in `[now - 7d, now]`
- Report aggregated `token_counts` (input / output / cache_creation / cache_read) per window
- Collect the set of `model` strings observed in each window (for diagnostic use; segments don't render this in v0.1)
- Surface `usageLimitResetTime` when present in the most recent entry of the active 5h block. Empirically the field never appears in real-world transcripts ([research/jsonl-data-source.md §Verification](../research/jsonl-data-source.md#verification-usagelimitresettime-provenance-2026-05-16)); the aggregator still deserializes it so a future Claude Code release that begins emitting it just works
- Log malformed JSON lines at `warn!`, advance past them, continue aggregation; never fail the batch on a single bad line
- Return [`JsonlError::DirectoryMissing`] when no project root exists at all — distinct from `NoEntries` (directory exists but is empty)

### Non-functional

- Per-invocation aggregation cost ≤5ms for typical JSONL volumes (per-user transcripts rarely exceed ~10 MB aggregate)
- Dedup working set bounded by the 7-day window — not total transcript size
- Byte-offset incremental tail ([data-fetching.md](data-fetching.md) §JSONL incremental tail) keeps scan cost at O(new bytes) on repeat invocations
- Partial parsing: only fields listed in [§Per-line record schema](#per-line-record-schema) are consumed; unknown keys are silently dropped per [ADR-0009](../adrs/0009-json-parsing-stack.md)
- No tier-specific math: aggregator produces raw counts. Tier-aware utilization is out of scope per [ADR-0011](../adrs/0011-rate-limit-data-source.md) §Tier handling

## Interface / Contract

### Entry point

```rust
/// Top-level aggregator. Discovers project roots, opens every
/// `*.jsonl` under each, runs the two-pass aggregation, and returns
/// the result. Called exactly once per process through the
/// `DataContext::jsonl` accessor; memoization is the caller's
/// responsibility.
pub fn aggregate_jsonl() -> Result<JsonlAggregate, JsonlError>;
```

### `JsonlAggregate`

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct JsonlAggregate {
    /// The currently-active 5h billing block, if any entry lies
    /// within the last 5h. `None` when no activity recorded.
    pub five_hour: Option<FiveHourBlock>,

    /// Rolling 7-day window. Always present even on an empty
    /// transcript — `token_counts` is all-zeros in that case.
    pub seven_day: SevenDayWindow,

    /// Source files scanned, for `linesmith doctor` diagnostics.
    /// Order matches the project-root cascade; within a root, order
    /// is directory-traversal order (undefined across filesystems).
    pub source_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct FiveHourBlock {
    /// UTC-floor-to-hour of the first entry in this block.
    pub start: Timestamp,
    /// `start + 5h` — the block's nominal close.
    pub end: Timestamp,
    /// Timestamp of the most recent entry observed in this block.
    pub actual_last_activity: Timestamp,
    pub token_counts: TokenCounts,
    /// Set of `message.model` strings seen in this block. Ordered
    /// by first-observation; no deduplication guarantees between
    /// casing variants.
    pub models: Vec<String>,
    /// Claude API reset hint if the most recent entry carried one.
    /// Verified absent across the surveyed Claude Code 2.1.108–2.1.143
    /// corpus (123k records, zero `usageLimitResetTime` keys; see
    /// [research/jsonl-data-source.md §Verification](../research/jsonl-data-source.md#verification-usagelimitresettime-provenance-2026-05-16)).
    /// Aggregator deserializes the field so a future emission just
    /// works, but segments do NOT consume it: the `rate_limit_5h_reset`
    /// JSONL-mode render uses [`FiveHourBlock::end`] instead. ADR-0013
    /// §Per-segment render in JSONL mode remains the authoritative
    /// render contract; no change implied by the verification.
    pub usage_limit_reset: Option<Timestamp>,
}

#[derive(Debug, Clone)]
pub struct SevenDayWindow {
    /// `now - 7d` at aggregation time. Consumers must not cache
    /// `JsonlAggregate` instances past the cache TTL enforced at
    /// the orchestrator layer.
    pub window_start: Timestamp,
    pub token_counts: TokenCounts,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
}

impl TokenCounts {
    /// Sum across all four categories. Used when the orchestrator
    /// needs a single "tokens consumed" number.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_creation + self.cache_read
    }
}
```

### `JsonlError`

```rust
#[derive(Debug)]
#[non_exhaustive]
pub enum JsonlError {
    /// No project-root directory exists in any cascade path.
    /// Distinct from `NoEntries`: this means the user has never run
    /// Claude Code, not that they have but produced zero records.
    DirectoryMissing,
    /// Project root(s) exist but contain no `*.jsonl` files.
    NoEntries,
    /// Filesystem error opening, reading, or traversing a path.
    /// Partial results above this point are discarded; the caller
    /// decides whether to degrade to `NoEntries` semantics.
    IoError { path: PathBuf, cause: io::Error },
    /// Parse failed on a line the caller requested strictly
    /// (e.g., a test fixture). Production aggregation logs
    /// per-line parse failures at `warn!` and continues — this
    /// variant is reserved for fail-fast callers.
    ParseError {
        path: PathBuf,
        line: u64,
        cause: serde_json::Error,
    },
}
```

`JsonlError` must provide a `code()` method returning a short plugin-facing tag per [plugin-api.md](plugin-api.md) §ctx shape, matching the pattern established by [`CredentialError`](credentials.md#types) and [`UsageError`](data-fetching.md#oauth-fallback-cascade). Tags: `"DirectoryMissing"`, `"NoEntries"`, `"IoError"`, `"ParseError"`.

### Per-line record schema

Records use `#[serde(default)]` + partial fields per [ADR-0009](../adrs/0009-json-parsing-stack.md). Only the fields below are consumed; unknown keys are silently dropped.

```rust
#[derive(serde::Deserialize)]
struct UsageEntry {
    timestamp: Timestamp,
    message: MessageFields,
    #[serde(default, rename = "costUSD")]
    cost_usd: Option<f64>,
    /// Claude API usage-limit reset hint. Present on a subset of
    /// entries (exact conditions unconfirmed — see open questions).
    #[serde(default, rename = "usageLimitResetTime")]
    usage_limit_reset_time: Option<Timestamp>,
    #[serde(default)]
    version: Option<String>,
}

#[derive(serde::Deserialize)]
struct MessageFields {
    #[serde(default)]
    usage: Option<UsageCounts>,
    #[serde(default)]
    model: Option<String>,
    /// De-dup key. Missing-id entries are NOT dropped — they count
    /// individually because Claude Code's retry-rewrite path only
    /// preserves the original's `id`.
    #[serde(default)]
    id: Option<String>,
}

#[derive(serde::Deserialize)]
struct UsageCounts {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default, rename = "cache_creation_input_tokens")]
    cache_creation: u64,
    #[serde(default, rename = "cache_read_input_tokens")]
    cache_read: u64,
}
```

Example record:

```json
{
  "timestamp": "2026-04-20T14:23:47.112Z",
  "message": {
    "id": "msg_01abc...",
    "model": "claude-opus-4-7",
    "usage": {
      "input_tokens": 1842,
      "output_tokens": 631,
      "cache_creation_input_tokens": 0,
      "cache_read_input_tokens": 48122
    }
  },
  "costUSD": 0.0421,
  "version": "1.0.85",
  "usageLimitResetTime": "2026-04-20T19:00:00Z"
}
```

### Project-root discovery

```rust
fn project_roots() -> Vec<PathBuf>;
```

Candidate roots, in order:

1. `$CLAUDE_CONFIG_DIR/projects/` when the env var is set and non-empty
2. `$HOME/.config/claude/projects/` (XDG-ish; Claude Code's documented default)
3. `$HOME/.claude/projects/` (Claude Code's legacy path)

Every root that actually contains `projects/` is included — the cascade does NOT short-circuit on the first match. A machine with transcripts in both `~/.config/claude/projects/` (recent) and `~/.claude/projects/` (legacy) has both scanned; dedup on `message.id` collapses duplicates across them.

Empty-string `CLAUDE_CONFIG_DIR` is treated as unset, matching the [credentials.md §Edge cases](credentials.md#edge-cases) pattern.

## Behavior

### Aggregation flow

```text
1. project_roots() → Vec<PathBuf>
2. For each root: walk *.jsonl under projects/**/ (one level deep
   is enough; Claude Code encodes the workspace path in the leaf
   directory name — see research/jsonl-data-source.md §Findings.1).
3. For each file: open via `JsonlTailer::new(path)`.
4. For each line: `serde_json::from_str::<UsageEntry>`. On parse
   error, log at `warn!` with path + line number; do not advance
   aggregation (Tailer advances the offset past it so repeat
   invocations don't re-parse malformed lines).
5. Dedup: track seen `message.id` values in a `HashSet<String>`.
   Collapse to first occurrence; missing-id entries count
   individually.
6. First pass: 5-hour blocks (see below).
7. Second pass: 7-day window sum (see below). Second pass runs on
   the same in-memory entry list — no file re-read.
8. Return JsonlAggregate with the active block (if any) and the
   7-day window. Prior completed blocks are discarded (v0.1 doesn't
   render them; collecting them is deferred to a future spec).
```

### 5-hour block math

Blocks are keyed by UTC-floor-to-hour timestamps, following ccusage's `floorToHour` (`_session-blocks.ts:14`).

```text
For each entry e in chronological order:
  if no current block:
    start = floor_to_hour(e.timestamp)
    current = FiveHourBlock { start, end: start + 5h, ... }
  else if e.timestamp - current.actual_last_activity > 5h:
    close current block (discarded in v0.1; see above)
    open new block starting at floor_to_hour(e.timestamp)
  else:
    accumulate e into current
```

Gap-block semantics (ccusage's `isGap`) are computed but not exposed in v0.1 — they exist solely to make the chronological walk correct when an entry lands >5h after its predecessor.

`usage_limit_reset` is populated from the most recent entry in the active block that carries `usageLimitResetTime`. Older entries' hints are shadowed.

### 7-day window math

```text
window_start = Timestamp::now() - SignedDuration::from_hours(7 * 24)
seven_day.token_counts = sum(e.message.usage) for e where
    e.timestamp >= window_start
```

No block boundaries — just a linear sum. Matches ccusage's `weeklyDateSchema` behavior.

### Malformed line handling

A single malformed line:

- Is logged once at `warn!` level (path + line number + the serde error message)
- Advances the Tailer's `last_offset` past the line terminator, so repeat invocations don't re-encounter it
- Does NOT abort aggregation of later lines in the same file

Rationale per [data-fetching.md](data-fetching.md) §JSONL incremental tail: transcripts are production data written by a separate process; a parser bug or schema drift in one line must not starve the statusline.

### Dedup semantics

```text
seen: HashSet<String> = HashSet::new()
for e in entries:
    match e.message.id {
        Some(id) if seen.contains(&id) => skip,
        Some(id) => {
            seen.insert(id);
            include(e);
        }
        None => include(e),  // missing id = always counted
    }
```

The working set is bounded by the 7-day window: once an entry falls outside that window, its id can be pruned from the set. v0.1 keeps the full set for simplicity; pruning is a future optimization if memory becomes a concern.

## Edge cases

| Case                                                     | Handling                                                                                                                                                                          |
| -------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| No `projects/` directory in any cascade root             | `JsonlError::DirectoryMissing`                                                                                                                                                    |
| `projects/` exists, zero `*.jsonl` files                 | `JsonlError::NoEntries`                                                                                                                                                           |
| `*.jsonl` file unreadable (permission denied)            | Skip the file, log once, continue. Don't error the whole aggregation on one unreadable transcript.                                                                                |
| File truncated mid-read                                  | `JsonlTailer` detects `size < last_size` and resets `last_offset` per [data-fetching.md](data-fetching.md) §JSONL incremental tail                                                |
| Partial trailing line (no `\n`)                          | Tailer doesn't advance past it; re-read on next invocation                                                                                                                        |
| Entry with `timestamp` in the future (clock skew)        | Included in current 5h block if `now - timestamp ≤ 5h`, else skipped                                                                                                              |
| Entry missing `message.usage` entirely                   | Counted toward dedup but contributes zero to `token_counts`                                                                                                                       |
| Entry with `input_tokens: 0` + all caches zero           | Same as above — zero-contribution but still an "activity" for the 5h block boundary                                                                                               |
| Same `message.id` across multiple files (post-migration) | First occurrence wins; others are skipped                                                                                                                                         |
| Symlinks under `projects/`                               | Followed by default (`fs::metadata` follows symlinks). Loop detection is not v0.1's problem — filesystem-level cycles would loop the walker, but Claude Code doesn't create them. |
| Non-UTF-8 bytes in a `*.jsonl` file                      | Treated as a malformed-line class; logged + skipped. Practically unreachable — JSONL is always UTF-8 — but the handling is defined.                                               |

## Testing strategy

- **Unit tests**
  - `project_roots` cascade under every env permutation (`CLAUDE_CONFIG_DIR` set / empty / unset; `HOME` set / unset)
  - 5h block math: entries exactly 5h apart, within 5h, >5h gap emits a new block
  - 7d window: entries inside window, exactly at boundary, outside window
  - Dedup: same `message.id` twice → counted once; missing `message.id` → counted per occurrence
  - `TokenCounts::total` arithmetic including overflow guard (saturating vs. wrapping — see open questions)
  - `JsonlError::code()` taxonomy: four tags, no collisions

- **Integration tests**
  - Multi-file fixture under `tests/fixtures/jsonl/` matching real Claude Code output; aggregate result pinned by `PartialEq` on `JsonlAggregate`
  - Malformed line mid-file: aggregation completes with later entries intact; one warn emission
  - Project root cascade with fixtures in all three paths; dedup collapses across them

- **Snapshot tests**
  - `JsonlError` rendering via `Display`
  - Example-record golden (the block in [§Per-line record schema](#per-line-record-schema)) round-trips losslessly

## Open questions

- ~~**`usageLimitResetTime` provenance.**~~ _Resolved 2026-05-16 (`lsm-ghpj`)._ Verified absent across a 26-version, 123k-record corpus ([research/jsonl-data-source.md §Verification](../research/jsonl-data-source.md#verification-usagelimitresettime-provenance-2026-05-16)). Aggregator continues to deserialize the field defensively; segments rely on `FiveHourBlock::end` for the 5h reset render per ADR-0013.

- **Log rotation.** Claude Code might rotate long-running session transcripts — unverified. If it does, a session's entries span multiple files and dedup across them becomes load-bearing. Current design handles this correctly (dedup is cross-file) but the test fixtures don't explicitly cover the rotation case. File a follow-up if rotation is confirmed.

- **Token-counts overflow.** `u64` saturation at ~1.8e19 is unreachable for any realistic user, but the arithmetic in `TokenCounts::total` uses plain `+` — a pathological transcript with billions of high-count entries could wrap. Saturating arithmetic would be safer at negligible cost; deferring the decision to implementation.

- **Dedup set pruning.** Current design keeps the full 7-day `HashSet<String>` of message IDs. For heavy users, this is on the order of tens of thousands of entries; memory is fine but pruning below the 7d horizon is a straightforward optimization. Left to implementation.

- **Completed 5h blocks.** ccusage returns the full list of historical blocks; v0.1 only exposes the active one. Future segments (usage-history sparkline, burn-rate indicators) may need the history — extending `JsonlAggregate` to carry `completed_blocks: Vec<FiveHourBlock>` is a non-breaking change under `#[non_exhaustive]`.

## Change log

- 2026-05-16 (v0.3): resolved the `usageLimitResetTime` open question (`lsm-ghpj`).
  Empirical scan of a Claude Code 2.1.108–2.1.143 corpus (123k records, 26 versions)
  found zero records carrying the field as a JSON key. Aggregator continues to
  deserialize the field; segments do not consume it. ADR-0013's `FiveHourWindow.ends_at`
  decision stands. Updated [§Functional requirements](#functional) note, the
  [`FiveHourBlock.usage_limit_reset`](#jsonlaggregate) docstring, and the open
  question entry. Research detail at
  [research/jsonl-data-source.md §Verification](../research/jsonl-data-source.md#verification-usagelimitresettime-provenance-2026-05-16).
- 2026-04-22 (v0.2): open question "utilization without tier detection" resolved by
  [ADR-0013](../adrs/0013-jsonl-fallback-carries-token-counts.md) — the orchestrator
  surfaces a distinct [`UsageData::Jsonl`](data-fetching.md#oauth-usage-cache-stack)
  variant carrying raw `TokenCounts`. Documented
  [`FiveHourBlock::usage_limit_reset`] as unverified and unconsumed;
  `lsm-ghpj` tracks verification. See
  [rate-limit-segments.md §JSONL-fallback display](rate-limit-segments.md)
  for the per-segment render table.
- 2026-04-21: initial draft (v0.1). Defines `JsonlAggregate` / `FiveHourBlock` / `SevenDayWindow` / `TokenCounts` / `JsonlError`, per-line record schema, project-root discovery cascade, 5h block math (ccusage `_session-blocks.ts` port), 7d window math, dedup semantics, malformed-line handling. Driving ADRs: ADR-0009, ADR-0010, ADR-0011.
