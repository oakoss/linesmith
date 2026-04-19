# ccstatusline widget internals: rate-limit, effort, session

- Date: 2026-04-18
- Author: Jace Babin (w/ Claude Code)
- Scope: How ccstatusline actually implements the rate-limit, thinking-effort, and session widgets — API endpoint, auth, cache, display formats, fallback cascade. Supersedes the "no HTTP scraping" conclusion in `jsonl-data-source.md`.

## Question

For v0.1 parity with ccstatusline (7.7k⭐ dominant statusline) we need to know exactly how `BlockTimer`, `BlockResetTimer`, `WeeklyUsage`, `WeeklyResetTimer`, and the thinking-effort segment are implemented: where does the data come from, how is it cached, what does the output look like.

## Sources

- `github.com/sirmalloc/ccstatusline` (MIT) — widget + utility sources fetched 2026-04-18
  - `src/widgets/BlockTimer.ts`
  - `src/widgets/BlockResetTimer.ts`
  - `src/widgets/WeeklyUsage.ts`
  - `src/widgets/WeeklyResetTimer.ts`
  - `src/utils/usage-fetch.ts` — HTTP fetcher + caching + auth
  - `src/utils/usage-windows.ts` — block/window math
  - `src/utils/usage-types.ts` — schemas + constants
  - `src/utils/jsonl-metadata.ts` — thinking-effort detection
  - `src/widgets/shared/usage-display.ts` — display-mode state machine

## Findings

### 1. Correction: rate-limit data is HTTP, not JSONL

`jsonl-data-source.md` §Conclusions concluded "no competitor scrapes HTTP — every competitor reads Claude Code's own JSONL transcripts." That applies to **ccusage** (analyzer) but **not to ccstatusline** (statusline). ccstatusline hits an Anthropic OAuth endpoint as its primary rate-limit source; JSONL aggregation is only the fallback when the HTTP path fails.

### 2. The endpoint

```text
GET https://api.anthropic.com/api/oauth/usage
Headers:
  Authorization: Bearer {oauth_access_token}
  anthropic-beta: oauth-2025-04-20
Timeout: 5000ms
```

Response (zod-validated in `usage-fetch.ts`):

```json
{
  "five_hour":  { "utilization": number, "resets_at": "ISO-8601" },
  "seven_day":  { "utilization": number, "resets_at": "ISO-8601" },
  "extra_usage": {
    "is_enabled": boolean,
    "monthly_limit": number,      // cents
    "used_credits": number,       // cents
    "utilization": number
  }
}
```

`utilization` is a percentage (0–100). `resets_at` is an ISO timestamp; the window start is derived as `resets_at - 5h` (or `- 7d` for weekly).

### 3. OAuth token discovery

ccstatusline reads the logged-in Claude Code user's OAuth access token, not a user-supplied API key. Order of attempts:

- **macOS:** Keychain service `Claude Code-credentials` via `security find-generic-password -s ... -w`. Fallback: dump the whole keychain and grep for any service starting with that prefix (handles multiple accounts), parse hex-encoded `mdat` blobs for modification times, sort newest first.
- **Linux / Windows:** read `{claude_config_dir}/.credentials.json` — JSON with shape `{"claudeAiOauth": {"accessToken": string}}`.
- Claude config dir resolves via `CLAUDE_CONFIG_DIR` env override or the usual `~/.claude/` / `~/.config/claude/` search.

If no token: return `{error: 'no-credentials'}`, widget shows `[No credentials]`.

### 4. Caching strategy

Caching is mandatory and multi-tier, because this endpoint is rate-limited and the statusline runs per-prompt:

| Layer       | Path                               | TTL  | Purpose                                      |
| ----------- | ---------------------------------- | ---- | -------------------------------------------- |
| Memory      | module-level `let cachedUsageData` | 180s | intra-process hits                           |
| Disk (data) | `~/.cache/ccstatusline/usage.json` | 180s | cross-invocation cache                       |
| Disk (lock) | `~/.cache/ccstatusline/usage.lock` | 30s  | rate limit: don't retry API more than 1×/30s |

Plus specific error handling:

- **429 response:** honor `Retry-After` header (integer seconds or HTTP date); default 300s backoff. Writes lock with `{blockedUntil, error: 'rate-limited'}`.
- **Timeout / network error:** fall back to stale cache if available; otherwise short-cache the error (30s) so we don't hammer.
- **Auth failure distinct from timeout:** token lookup happens _before_ the lock check so `no-credentials` never gets masked as `timeout`.

### 5. Block/window math

Primary path (`getUsageWindowFromResetAt`):

```text
resetAtMs = Date.parse(usageData.sessionResetAt)   // from HTTP
startAtMs = resetAtMs - 5h                          // derived
elapsedMs = clamp(now - startAtMs, 0, 5h)
remainingMs = 5h - elapsedMs
elapsedPercent = elapsedMs / 5h * 100
remainingPercent = 100 - elapsedPercent
```

Fallback (`getUsageWindowFromBlockMetrics`): same formula but `startAtMs` comes from `BlockMetrics.startTime` (JSONL-derived 5h block start, ccusage-style floor-to-hour).

`resolveUsageWindowWithFallback` tries HTTP first, falls back to JSONL cache. Weekly has no JSONL fallback (`resolveWeeklyUsageWindow`) — if `weeklyResetAt` is missing, the widget hides.

Constants: `FIVE_HOUR_BLOCK_MS = 18_000_000`, `SEVEN_DAY_WINDOW_MS = 604_800_000`.

### 6. Widget display formats

All four timer widgets support three display modes via the `cycleUsageDisplayMode` state machine: `time` (default), `progress` (32-char bar), `progress-short` (16-char bar).

Time-mode formatter `formatUsageDuration(ms, compact, useDays)`:

- `compact=false`: `"1d 3hr 45m"` (space-joined)
- `compact=true`: `"1d3h45m"` (no separator)
- `useDays=false`: `"36hr 30m"` (fold days into hours — used for weekly-reset "hours only" toggle)
- Zero: `"0m"`

Progress-mode formatter:

- Timer widgets (BlockTimer, BlockResetTimer, WeeklyResetTimer): `"Block [███░░░] 73.9%"` — `Math.floor` for fill, percentage with one decimal
- Usage widget (WeeklyUsage): `"Weekly: [████░] 12.0%"` — uses `Math.round` and `[]` wrapping via `makeUsageProgressBar`

Percent clamped to `[0, 100]`. `invert` toggle flips `elapsedPercent ↔ remainingPercent` (so users can show "3h left" as bar fill instead of "2h elapsed").

Empty-state behavior varies by widget:

- **BlockTimer:** always shows — empty-bar `[░░░░░░] 0.0%` or `"Block: 0h"` even without data
- **BlockResetTimer / WeeklyResetTimer:** hide (`return null`) when no window and no error
- **WeeklyUsage:** hide when `weeklyUsage === undefined`, show error message otherwise

Error messages (from `getUsageErrorMessage`):

```text
no-credentials → [No credentials]
timeout         → [Timeout]
rate-limited    → [Rate limited]
api-error       → [API Error]
parse-error     → [Parse Error]
```

### 7. Thinking-effort detection

`jsonl-metadata.ts:getTranscriptThinkingEffort(transcriptPath)` parses the session's JSONL transcript:

1. Read lines back-to-front
2. Find the most recent message whose visible text (ANSI-stripped) starts with `<local-command-stdout>Set model to`
3. Regex: `/^<local-command-stdout>Set model to[\s\S]*? with ([a-zA-Z0-9-]+) effort<\/local-command-stdout>$/i`
4. Normalize against known values: `low | medium | high | xhigh | max`
5. Unknown tokens that match `/^(?=.*[a-z0-9])[a-z0-9-]{2,20}$/` are returned as `{value, known: false}` — forward-compat with future effort names

Effort level comes from the transcript's echo of `/model` output. That's the same signal as Claude Code's in-memory state, because `/model` writes to the transcript when the user changes effort. Downside: stale until the next `/model` invocation writes a new stdout line.

Resolves lsm-7ki.

## Conclusions

1. **ccstatusline's rate-limit source is `api.anthropic.com/api/oauth/usage`.** HTTP is primary, JSONL is fallback. Prior research note got this backwards.
2. **The OAuth token is the user's Claude Code session token**, read from Keychain (macOS) or `.credentials.json` (other). No user-facing API key setup needed — if `claude` is logged in, ccstatusline works.
3. **Caching is non-trivial and non-optional.** 180s data cache, 30s lock file, 429-aware backoff. A statusline running per-prompt _must_ cache or it'll hit rate limits on the usage endpoint itself.
4. **Weekly has no JSONL fallback.** If the endpoint fails permanently, weekly widgets hide entirely. Block widgets still render from the JSONL-derived block start.
5. **Thinking-effort is a transcript parser, not a settings read.** Last `/model` command's stdout line is the source. Stale by design between invocations.
6. **`anthropic-beta: oauth-2025-04-20`** — this endpoint is beta-gated. Ship-stability risk: Anthropic could change the shape or require a newer beta header.

## Implications / actions

- **Promoted to [ADR-0011](../adrs/0011-rate-limit-data-source.md)** — endpoint, auth, cache, JSONL fallback, and credential cascade now codified. Tier detection explicitly deferred (no competitor does it; out of scope for v0.1). lsm-y6m epic scope reshapes to "implement ADR-0011".
- **Reshape lsm-y6m epic scope.** ADR should describe the HTTP-first cascade (endpoint, auth, cache, JSONL fallback, error states), not just JSONL aggregation. File a dedicated bead for the HTTP fetcher + credential reader.
- **New beads needed:**
  - OAuth credential reader (macOS Keychain + cross-platform file fallback)
  - HTTP client wrapper (ureq) with timeout + 429 handling
  - Two-tier cache (memory + file) with lock file for API rate-limit politeness
  - `extra_usage` segment (not in prior parity matrix — ccstatusline has it)
- **Update lsm-4wb (context-window spike)** to note the HTTP endpoint is _not_ part of context-window correctness — those are separate concerns.
- **Close lsm-7ki with a close reason pointing at `jsonl-metadata.ts`** — implementation is ~60 lines of transcript tailing.
- **Amend `jsonl-data-source.md`** with a pointer to this note.
- **Revisit lsm-043 (tier handling).** If the OAuth endpoint works regardless of tier, the free/Pro-Max split may collapse. API-tier users (no Claude Code login) remain unreachable.
- **File a spike** to characterize the endpoint's response for free-tier users (does `utilization` go `null`? Does the whole response differ?).

## Open questions

- **Does `/usage` (Claude Code built-in) hit the same endpoint?** Almost certainly yes given schema shape, but not verified. (lsm-4qd spike will answer this.)
- **Response shape for free-tier users.** Worth a capture.
- **Endpoint stability.** `anthropic-beta: oauth-2025-04-20` is today's beta header. When Anthropic rotates or GAs it, our code breaks unless we track the beta header or accept any `anthropic-beta` value.
- **Rate-limit policy on the endpoint itself.** ccstatusline defaults to 300s backoff after a 429; does Anthropic document the actual limit? (We'd want to cite it in the ADR.)
- **Token refresh.** OAuth access tokens typically expire. ccstatusline reads but doesn't refresh. What happens when the token is stale — `no-credentials` error? 401? Does Claude Code refresh externally?

## Raw data

### API response (TypeScript interface)

```ts
interface UsageApiResponse {
  five_hour?: { utilization?: number | null; resets_at?: string | null };
  seven_day?: { utilization?: number | null; resets_at?: string | null };
  extra_usage?: {
    is_enabled?: boolean | null;
    monthly_limit?: number | null; // cents
    used_credits?: number | null; // cents
    utilization?: number | null;
  };
}
```

### Canonical widget outputs (non-preview, typical values)

```text
BlockTimer        time   Block: 3hr 45m
BlockTimer        prog   Block [████████████░░░░░░░░░░░░░░░░░░░░] 37.5%
BlockResetTimer   time   Reset: 1hr 15m
WeeklyUsage       time   Weekly: 42.3%
WeeklyUsage       prog   Weekly: [██████░░░░░░░░░] 42.3%
WeeklyResetTimer  time   Weekly Reset: 3d 14hr 20m
WeeklyResetTimer  hrs    Weekly Reset: 86hr 20m
```
