# Claude Code JSONL data source + ccstatusline widget catalog

- Date: 2026-04-18
- Author: Jace Babin (w/ Claude Code)
- Scope: How existing Claude Code statusline tools source rate-limit, token, and session-block data, and what widget set the dominant tool ships.

> **Correction (2026-04-18):** this note's Conclusion #1 ("no competitor scrapes HTTP") is accurate for `ccusage` (the analyzer) but **wrong for `ccstatusline`** (the statusline). ccstatusline hits `GET https://api.anthropic.com/api/oauth/usage` as its primary rate-limit source; JSONL aggregation is only the fallback when the HTTP path fails. See `ccstatusline-widget-internals.md` for the endpoint, auth flow, caching, and widget formats.

## Question

1. How do competitors obtain rate-limit / usage data that Claude Code's statusline stdin payload doesn't include (5h / 7d windows, free-tier usage, historical tokens)?
2. What is the canonical per-line schema of Claude Code's JSONL transcripts?
3. What widgets does `sirmalloc/ccstatusline` (the dominant statusline) ship, and which are worth mirroring in linesmith?

## Sources

- `github.com/ryoppippi/ccusage` — MIT-licensed, canonical Claude Code usage analyzer (TypeScript, monorepo)
  - `apps/ccusage/src/_types.ts` — valibot schemas for JSONL records + statusline hook payload
  - `apps/ccusage/src/data-loader.ts` — `$HOME/.claude/projects/` discovery + JSONL parsing
  - `apps/ccusage/src/_session-blocks.ts` — 5-hour billing-block aggregation
- `github.com/sirmalloc/ccstatusline` — dominant TypeScript statusline (7.7k⭐)
  - `src/widgets/*.ts` — widget catalog
  - `src/utils/context-window.ts` — context-window metric derivation

## Findings

### 1. The data source is local JSONL, not HTTP headers

Every community statusline tool that surfaces rate-limit or historical token data reads Claude Code's own transcript files. No tool intercepts HTTP traffic.

**Path convention** (`apps/ccusage/src/data-loader.ts`): read from `$HOME/.claude/projects/*/*.jsonl`. The `CLAUDE_PROJECTS_DIR_NAME` constant resolves to `projects/` under the Claude Code data dir. Multiple candidate roots are scanned: the `CLAUDE_CONFIG_DIR` env override, then `~/.config/claude/`, then `~/.claude/`. Each root contributes any `projects/` it finds.

**Project-path encoding**: a file at `.../projects/-Users-alice-code-myrepo/session.jsonl` represents the `/Users/alice/code/myrepo` workspace — the `-`-joined path is Claude Code's own encoding.

### 2. Per-line JSONL record schema

From the `usageDataSchema` in `apps/ccusage/src/data-loader.ts`:

```text
{
  timestamp: ISO8601 string,        // message time, UTC
  message: {
    usage: {
      input_tokens: number,
      output_tokens: number,
      cache_creation_input_tokens?: number,
      cache_read_input_tokens?: number,
    },
    model?: string,                 // e.g. "claude-opus-4-7"
    id?: string,                    // messageId, used for dedup across rewrites
    content?: [...],                // full message body (ignored for aggregation)
  },
  costUSD?: number,                 // present when Claude Code has run cost calc
}
```

Additional shape (from `LoadedUsageEntry` in `_session-blocks.ts`):

- `version?: string` — Claude Code version that wrote the line
- `usageLimitResetTime?: Date` — Claude API usage-limit reset hint, when present

### 3. 5-hour billing blocks

`DEFAULT_SESSION_DURATION_HOURS = 5` (`_session-blocks.ts:8`). Blocks are keyed by UTC-floored-to-hour timestamps (`floorToHour`, line 14). A `SessionBlock`:

```text
{
  id: ISO-string-of-block-start,
  startTime: Date,
  endTime: startTime + 5h,          // for normal blocks
  actualEndTime?: Date,             // last activity in block
  isActive: boolean,
  isGap?: boolean,                  // idle stretch, no activity
  entries: LoadedUsageEntry[],
  tokenCounts: { input, output, cache_creation, cache_read },
  costUSD: number,
  models: string[],
  usageLimitResetTime?: Date,
}
```

Aggregation scans entries chronologically; a new block starts when the gap from the previous entry exceeds 5 hours (gap-block emitted for the idle stretch).

### 4. 7-day / weekly

Weekly aggregation is a second pass over the same JSONL entries. `weeklyDateSchema` validates `YYYY-MM-DD`-shaped week anchors. No separate data source.

### 5. Claude Code statusline hook JSON

`statuslineHookJsonSchema` in `_types.ts` confirms the stdin payload Claude Code hands the statusline binary:

```text
{
  session_id: string,
  transcript_path: string,        // file path to the session's JSONL
  cwd: string,
  model: { id, display_name },
  workspace: { current_dir, project_dir },
  version?: string,
  cost?: {
    total_cost_usd: number,
    total_duration_ms?: number,
    total_api_duration_ms?: number,
    total_lines_added?: number,
    total_lines_removed?: number,
  },
  context_window?: {
    total_input_tokens: number,
    total_output_tokens?: number,
    context_window_size: number,
  },
}
```

`transcript_path` points linesmith directly at the current session's JSONL file, so single-session widgets don't need the `projects/` glob at all.

### 6. ContextPercentage vs ContextPercentageUsable

`ccstatusline/src/utils/context-window.ts` computes two quantities from `context_window.current_usage`:

- `usedTokens = input + output + cache_creation + cache_read` — **total** tokens accounted for
- `contextLengthTokens = input + cache_creation + cache_read` — tokens that consume the model's **read budget** (output doesn't)

The two `ContextPercentage*` widgets key off different denominators and tokens:

- **ContextPercentage**: `usedTokens / context_window_size` — "how full is the session overall?"
- **ContextPercentageUsable**: `contextLengthTokens / context_window_size` — "how much read budget remains?" — the number that matters post-`/compact`, where output tokens from pre-compact messages no longer count against what the model can still read.

`lsm-4wb` (the context-window correctness spike) should reproduce this split and verify whether our current `context_window` segment reports the usable number or the total.

### 7. ccstatusline widget catalog (v0.1 parity subset)

From `sirmalloc/ccstatusline/src/widgets/`:

| Widget                                        | Purpose                            | Source                         | Linesmith mapping                                                       |
| --------------------------------------------- | ---------------------------------- | ------------------------------ | ----------------------------------------------------------------------- |
| `ContextBar`                                  | Visual fill bar for context window | stdin `context_window.*`       | `context_bar` (lsm-r1w)                                                 |
| `ContextPercentage`                           | Used % of total                    | stdin                          | existing `context_window` segment                                       |
| `ContextPercentageUsable`                     | Usable-read-budget %               | stdin, derived                 | new segment (follow-up from lsm-4wb)                                    |
| `ContextLength`                               | Raw used-token count               | stdin                          | `tokens_*` (lsm-ua8 covers)                                             |
| `SessionClock`                                | Elapsed since session start        | stdin `cost.total_duration_ms` | `session_duration` (lsm-z0y)                                            |
| `SessionCost`                                 | USD spend                          | stdin `cost.total_cost_usd`    | existing `cost` segment                                                 |
| `SessionUsage`                                | Total tokens this session          | stdin                          | subsumed by `tokens_*`                                                  |
| `TokensInput` / `Output` / `Cached` / `Total` | Per-token-kind counts              | stdin `current_usage.*`        | `tokens_input` / `_output` / `_cached` / `_total` (lsm-ua8 4-way split) |
| `BlockResetTimer`                             | Countdown to 5h block reset        | JSONL aggregate                | part of `lsm-y6m` epic                                                  |
| `BlockTimer`                                  | 5h usage %                         | JSONL aggregate                | part of `lsm-y6m` epic                                                  |
| `WeeklyResetTimer`                            | Countdown to 7d reset              | JSONL aggregate                | part of `lsm-y6m` epic                                                  |
| `WeeklyUsage`                                 | 7d usage %                         | JSONL aggregate                | part of `lsm-y6m` epic                                                  |

ccstatusline's full widget list (45+ total) also covers git (13 widgets — scoped to `lsm-8jl`), custom text / symbols / commands, vim mode, output style, input/output speed meters, free memory, and a clickable OSC-8 Link. Everything outside git is v0.2+ polish for linesmith.

## Conclusions

1. **There is no API-header-interception architecture to build.** Every competitor reads Claude Code's own JSONL transcripts. That makes `lsm-y6m` a much simpler implementation than the epic originally implied.
2. **ccusage's schema is the de-facto spec.** We should port the `usageDataSchema` validation to Rust (serde + optional zod-equivalent checks) rather than re-deriving from bytes. MIT license permits it.
3. **The `transcript_path` field** in the statusline hook JSON already points at the current session's JSONL — no file discovery needed for single-session widgets; only needed for multi-session 5h/7d aggregation.
4. **Context-window correctness for `/compact` is almost certainly the "usable read budget" distinction.** `lsm-4wb` should confirm by reproducing the ccstatusline split.
5. **ccstatusline's widget names are natural for linesmith segments.** Adopting `session_duration` over `SessionClock` is fine (more CLI-literate), but `context_bar`, `tokens_input/output/cached/total` mirror their naming directly.

## Implications / actions

- **lsm-y6m** — ADR scope narrows from "how do we scrape" to "how do we parse + aggregate + cache". Follow-up: write the ADR citing this note.
- **lsm-4wb** — spike should reproduce `ContextPercentage` vs `ContextPercentageUsable`. If the distinction matters for Claude Opus 4.7 / 1M context / post-compact, file a bead for a `context_usable` segment.
- **lsm-ua8** — 4-way token split confirmed as the right shape (match ccstatusline).
- **lsm-r1w** — `ContextBar` mapping confirmed.
- **lsm-z0y** — `session_duration` aligns with `SessionClock`; source is stdin, no JSONL reads needed.
- **Four new rate-limit segments** implied for v0.1 parity with ccstatusline: `block_timer`, `block_reset_timer`, `weekly_usage`, `weekly_reset_timer`. File under `lsm-y6m` epic when the ADR is written.

## Open questions

- **Log rotation.** ccusage reads all files under `projects/` — do Claude Code sessions share a single JSONL or rotate? The `session_id` field + `transcript_path` suggest one file per session, but long-running sessions may rotate. Needs verification for lsm-y6m.
- **Multi-machine roaming.** If a user runs Claude Code on two machines, do the JSONLs sync? Not ccusage's concern; not linesmith's either in v0.1.
- **`usageLimitResetTime`.** ccusage pulls this from some entries — source unclear from the schema (Claude Code may emit it only when the user has actually been rate-limited). Worth mapping when we implement the reset-timer widgets.
- **Dedup.** ccusage dedupes on `message.id` — implies the JSONL can contain duplicate entries (retries? rewrites on edit?). Our parser must follow suit.
- **Performance.** ccusage reads every JSONL on every invocation. For a statusline called on every prompt, we'll need a cache (time-windowed; invalidate on file mtime change). Out of scope for this note.
