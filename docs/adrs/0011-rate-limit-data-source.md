# Use the Anthropic OAuth usage endpoint for rate-limit data, with JSONL and credential-cascade fallbacks

- Status: accepted
- Amended by: [ADR-0013](0013-jsonl-fallback-carries-token-counts.md) — §Fallback cascade step 5 and the implicit `UsageData` shape. Endpoint, auth, cache, and credential-cascade decisions stand.
- Amended by: [ADR-0030](0030-model-scoped-usage-arrives-in-a-limits-array.md) — the `UsageApiResponse` shape in §Endpoint contract, the Decision Outcome's clause (b), and the §Consequences bullet naming `seven_day_sonnet` / `seven_day_opus`. Those fields are null (two of them already were at capture time); the data arrives in a `limits` array. Endpoint, auth, cache, and credential-cascade decisions stand.
- Amended by: [ADR-0031](0031-auth-failures-are-not-transient-errors.md) — the §Cache stack bullets naming a flat 30s disk-lock TTL with 429 as the sole extended-backoff case, and the "serve stale cached data on transient errors" bullet. Auth failures are not transient: they skip stale-serve, and 403 takes a 300s backoff. Endpoint, auth, credential-cascade, and JSONL fallback decisions stand.
- Date: 2026-04-18
- Deciders: Jace

## Context and Problem Statement

Rate-limit visibility is the single most-requested feature in Claude Code statuslines (44+ reactions on anthropics/claude-code#8412). The underlying data has several potential sources: HTTP endpoints, Claude Code's stdin payload, JSONL transcript aggregation, local credentials. Each has different reliability and auth requirements. Which source(s) should linesmith use for the 5h/7d usage segments, and how should credentials be read?

## Decision Drivers

- Must work for any Claude Code user with valid OAuth credentials, regardless of plan tier
- Must match or exceed ccstatusline + CCometixLine on correctness (these two cover ~100% of the existing Claude Code statusline user base)
- Must not break Claude Code: credential reading and file access must respect CC's writers
- Rate-limit endpoint is itself rate-limited: cache hygiene is load-bearing
- Windows + Linux credential paths must work even if v0.1 only targets macOS initially (don't paint ourselves into a corner)

## Considered Options

- **OAuth `/api/oauth/usage` endpoint only**: what ccstatusline does in its primary path
- **JSONL 5h-block aggregation only**: what ccusage does (offline analyzer); ccstatusline's fallback
- **OAuth endpoint + JSONL fallback**: combined approach. Hit the endpoint when auth is available, fall back to JSONL when it isn't
- **Anthropic billing API**: different endpoint, would require console auth; unproven for this use case

## Decision Outcome

Chosen option: **OAuth `/api/oauth/usage` endpoint as primary source, with JSONL 5h-block aggregation as fallback**, because (a) every Claude Code statusline tool with non-trivial adoption uses this endpoint (ccstatusline, CCometixLine both verified), (b) we captured the live endpoint on 2026-04-18 and confirmed it returns authoritative usage data including per-model weekly buckets (`seven_day_sonnet`, etc.) that JSONL aggregation can't produce, and (c) the JSONL fallback preserves a usable rate-limit segment when auth fails (users without OAuth credentials, revoked tokens, offline).

### Endpoint contract

```text
GET https://api.anthropic.com/api/oauth/usage
Headers:
  Authorization: Bearer {oauth_access_token}
  anthropic-beta: oauth-2025-04-20
  User-Agent: linesmith/{CARGO_PKG_VERSION}
Timeout: 2 seconds (configurable via segment options)
```

Response shape (with forward-compat Option wrappers because Anthropic ships codenamed unreleased-feature buckets):

```rust
struct UsageApiResponse {
    five_hour:              Option<UsageBucket>,
    seven_day:              Option<UsageBucket>,
    seven_day_opus:         Option<UsageBucket>,
    seven_day_sonnet:       Option<UsageBucket>,
    seven_day_oauth_apps:   Option<UsageBucket>,
    extra_usage:            Option<ExtraUsage>,
    #[serde(flatten)]
    unknown_buckets:        HashMap<String, serde_json::Value>, // omelette_*, iguana_*, etc.
}
```

### Credential source cascade

1. **macOS primary:** `security find-generic-password -s "Claude Code-credentials" -w` (Keychain service name matches Claude Code's convention)
2. **macOS multi-account fallback:** if the primary call returns no usable token, dump the Keychain and scan for any service whose name begins with `Claude Code-credentials` (multi-account machines store per-account entries with suffixes); parse the `mdat` modification-time blob for each match and prefer the most recently modified token. This matches ccstatusline's behavior and prevents picking a stale account's token.
3. **Linux / Windows:** read `{CLAUDE_CONFIG_DIR}/.credentials.json` when the env var is set; otherwise search `~/.config/claude/.credentials.json` (XDG layout) then `~/.claude/.credentials.json`. First file found wins.
4. If no token from any path: return `UsageData { error: NoCredentials }`; segment renders `[No credentials]`.

Credential reads are memoized for the process lifetime ([ADR-0010](0010-data-fetching-architecture.md)). The `security` subprocess is the single biggest non-network cost; we pay it once.

### Tier handling (out of scope)

No competitor statusline (ccstatusline, CCometixLine, claudia-statusline, claude-powerline) detects plan tier or renders tier-specific labels. They display raw `utilization` percentages and reset times from the endpoint, which are meaningful regardless of tier. linesmith v0.1 follows the same approach: display endpoint values as-is; no tier detection, no "Max"/"Pro"/"Free" labels. Users who want to know their tier can check `/status` inside Claude Code.

Tier-aware behavior (e.g., conditional segment formatting, plan-specific warnings) is deferred to v0.2+. If we ever want it, the derivation can pull from `~/.claude.json`'s `oauthAccount` block (`billingType`, `hasOpusPlanDefault`), but that's a future ADR gated on a real product need.

### Cache stack (per [ADR-0010](0010-data-fetching-architecture.md))

- Memory: module-level `OnceCell<UsageData>`, 180s TTL
- Disk data: `~/.cache/linesmith/usage.json`, 180s TTL, pretty-printed JSON with `schema_version` ([ADR-0009](0009-json-parsing-stack.md))
- Disk lock: `~/.cache/linesmith/usage.lock`, 30s TTL — prevents concurrent CC sessions from spamming the endpoint
- 429 `Retry-After`: honor both integer-seconds and HTTP-date formats; default 300s backoff if missing
- Stale-while-revalidate: serve stale cached data on transient errors rather than hiding the segment

### Fallback cascade

Credentials are resolved **before** the lock check so `NoCredentials` is never masked as `Timeout`. JSONL is the terminal fallback: every failure mode above it (no credentials, active lock with no stale cache, 429, timeout, network error) falls through to JSONL rather than returning early. Only when JSONL itself yields nothing does the original error surface.

```text
fetch_usage_data():
  1. If cached fresh data exists → return it
  2. Read credentials (memoized via the cascade in §"Credential source cascade")
     - No token → record NoCredentials error; skip to JSONL fallback (step 5)
  3. If lock file active:
     - Return stale cached data if present → done
     - Otherwise skip to JSONL fallback (step 5)
  4. Hit endpoint with 2s timeout:
     - 200 → parse, cache, return
     - 429 → honor Retry-After; return stale cache if present, else skip to JSONL fallback
     - timeout/network error → return stale cache if present, else skip to JSONL fallback
  5. JSONL fallback (5h/7d derivation):
     - Scan ~/.claude/projects/*/*.jsonl backwards per ccusage math
     - Aggregate tokens into 5h blocks; derive 7d similarly (no per-model split, no extra_usage)
     - Return JSONL-derived UsageData tagged as estimated → done
  6. If JSONL also fails (no transcripts, parse errors, directory missing):
     - Return the original error recorded earlier (NoCredentials, Timeout, RateLimited, NetworkError)
     - Segment renders the matching error message ([No credentials], [Timeout], etc.)
```

JSONL-derived values should be flagged visibly (e.g., `~` prefix or dimmed color) so users know the endpoint wasn't reached; exact rendering deferred to the segment spec.

### Consequences

- Good, because linesmith ships with rate-limit accuracy matching the dominant Claude Code statusline tools on day one
- Good, because per-model buckets (`seven_day_sonnet`, `seven_day_opus`) unlock segments ccstatusline + CCometixLine don't decode
- Good, because forward-compat `Option` wrappers + `#[serde(flatten)]` catch-all prevents schema drift from breaking the parser when Anthropic ships unreleased codenamed features
- Good, because JSONL fallback means the segment stays functional when auth fails — never hides silently
- Good, because process-lifetime credential memoization eliminates the `security` subprocess from the hot path after the first invocation
- Good, because skipping tier detection matches every surveyed competitor and eliminates an entire class of "my tier shows wrong" bugs before it starts
- Bad, because we depend on Anthropic's undocumented `anthropic-beta: oauth-2025-04-20` header; when they rotate the beta, we'll need to track it
- Bad, because JSONL fallback can't produce per-model weekly buckets; those segments hide when only the fallback is available
- Neutral, because API-tier users (raw API key, no OAuth session) have no reachable data source and are explicitly out of scope for rate-limit segments (cost segment still works)

### Confirmation

Revisit if:

- Anthropic rotates or GAs `oauth-2025-04-20` header: code must track whatever replaces it
- Users report the endpoint returns null/empty for their tier: may need JSONL-only path for that case
- A product need emerges for tier-aware segment behavior: write a new ADR for tier derivation
- ccstatusline or CCometixLine migrate to a different endpoint: cross-check to understand why

## Pros and Cons of the Options

### OAuth endpoint only

- Good: authoritative data, matches Claude Code's own `/usage` command output
- Bad: breaks when auth is unavailable (free users without OAuth, revoked tokens, offline)
- Bad: single point of failure for a high-value segment

### JSONL aggregation only

- Good: no network, no auth — works in all scenarios
- Bad: inaccurate — 5h blocks are approximated by token timestamps, not actual Anthropic-side quotas
- Bad: no per-model weekly buckets, no `extra_usage` data, no authoritative reset times
- Bad: diverges from what Claude Code's own `/usage` shows, confusing users

### OAuth endpoint + JSONL fallback (chosen)

- Good: best of both — authoritative when auth works, graceful when it doesn't
- Good: matches ccstatusline's pattern; battle-tested
- Bad: two code paths to maintain (endpoint client + JSONL aggregator)

### Anthropic billing API

- Good: might expose more data (e.g., monthly spend)
- Bad: unknown auth story; likely requires console login not OAuth
- Bad: no competitor uses it; zero evidence it's the right surface for statuslines

## More Information

- Primary source: [`docs/research/ccstatusline-widget-internals.md`](../research/ccstatusline-widget-internals.md) — endpoint, auth, cache stack, widget formats
- Supporting: [`docs/research/claude-data-files.md`](../research/claude-data-files.md) — credential file layout, tier derivation, live endpoint capture
- Supporting: [`docs/research/ccometixline-rust-patterns.md`](../research/ccometixline-rust-patterns.md) — Rust-peer confirmation of identical endpoint
- Supporting: [`docs/research/jsonl-data-source.md`](../research/jsonl-data-source.md) — 5h block aggregation math (used in fallback)
- Depends on: [ADR-0009](0009-json-parsing-stack.md), [ADR-0010](0010-data-fetching-architecture.md)
- Will drive: [`specs/rate-limit-segments.md`](../specs/rate-limit-segments.md) — segment-level contracts for `rate_limit_5h`, `rate_limit_7d`, `rate_limit_5h_reset`, `rate_limit_7d_reset`, `extra_usage`
- Will drive: [`specs/credentials.md`](../specs/credentials.md) — per-OS credential reader specifications
- Tracked beads: lsm-y6m (epic), lsm-4qd (`/usage` live-invocation spike, partially resolved by 2026-04-18 endpoint capture), lsm-4lw (worktree kind, not rate-limit specific), lsm-7ki (effort detection, separate segment, not rate-limit). lsm-043 (tier handling) closed as out of scope; see "Tier handling (out of scope)" above.
- Open follow-up: Windows Credential Manager path (v0.2). Tier-aware segment behavior is deferred to v0.2+ and gated on a concrete product need.
