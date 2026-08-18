# Treat auth failures as non-transient in the usage cache stack

- Status: accepted
- Date: 2026-08-17
- Deciders: Jace
- Amends: [ADR-0011](0011-rate-limit-data-source.md) — the §Cache stack bullets naming a flat 30s disk-lock TTL with 429 as the sole extended-backoff case, and the "serve stale cached data on transient errors" bullet. ADR-0011's choice of endpoint, auth, credential cascade, JSONL fallback ordering, and 180s cache TTL stand unchanged.

## Context and Problem Statement

ADR-0011 §Cache stack describes one backoff class and one stale-serve rule: a 30s disk lock, 429 as the single case earning a longer (300s) window, and "stale-while-revalidate: serve stale cached data on transient errors rather than hiding the segment".

Both bullets assume every endpoint failure is transient. Auth failures are not. A `403` (a token lacking the `user:profile` scope) clears only when the user signs in again, and a `401` clears only when Claude Code's next request refreshes the token — neither is bounded by waiting.

Two consequences followed from treating them as transient. `interpret_status` had no `403` arm, so a scope failure rendered `[Network error]` and pointed users at their connection. And because `CacheStore::read` bounds staleness only against clock skew, a warm cache meant a persistent auth failure served the same payload indefinitely — usage accruing behind numbers that never move, with the error never rendering at all.

How should the cache stack treat failures that cannot resolve on their own?

## Decision Drivers

- A status line that shows stale numbers forever is worse than one that shows an error
- `/api/oauth/usage` rate-limits aggressively; re-probing a dead endpoint every 30s risks a 429 cascade that degrades the whole line ([ADR-0013](0013-jsonl-fallback-carries-token-counts.md))
- The rendered error must distinguish "check your network" from "sign in again" — different user actions
- The JSONL fallback is independent of token validity, so a user with broken auth should still see real local data
- linesmith is stateless per invocation: nothing at this layer can stop retrying entirely, only lengthen the interval

## Considered Options

- **Classify 403 only** — give `403` its own error variant and rendering, leave the cache stack's backoff and stale-serve rules untouched
- **Classify 403 and carve auth failures out of both rules** — additionally give non-refreshable auth failures a longer backoff and exclude them from stale-serve
- **Add a terminal "give up" state** — persist a marker that stops probing entirely until cleared

## Decision Outcome

Chosen option: **classify 403 and carve auth failures out of both rules**, because classification alone fixes only the misleading string while leaving the more damaging failure in place: with a warm cache the segment silently reports numbers that can no longer be refreshed, and the `[Forbidden]` string it just gained would never render. Auth failures therefore skip stale-serve and clear the cache — joining the exception `401` already had in implementation but which ADR-0011 never recorded — and `403` takes a 300s `DEFAULT_AUTH_BACKOFF` rather than the 30s transient TTL.

`401` keeps the 30s TTL. The asymmetry is deliberate: Claude Code refreshes an expired token on its next request, so a short interval is what makes recovery fast, whereas a `403` needs the user to act and a short interval buys nothing but endpoint load.

The terminal give-up state was rejected as out of proportion: it needs new persistent state and a way to clear it, and the interval change already cuts probes from ~2900/day to ~288/day.

### Consequences

- Good, because an auth failure now renders as one — `[Forbidden]` or `[Unauthorized]` — instead of `[Network error]`
- Good, because the segment can no longer be pinned to unrefreshable numbers with nothing marking them stale
- Good, because `401`'s undocumented stale-serve exception is now recorded rather than surviving as implementation-only behavior
- Bad, because a user who re-authenticates early waits out the remaining 300s window: `lock_active` short-circuits before credentials are re-read, so nothing detects the fix
- Bad, because the endpoint is still probed indefinitely, just less often — stateless invocation makes a true terminal state impossible here
- Neutral, because JSONL renders throughout the backoff window, so the cost is estimated-instead-of-exact rather than a blank segment

### Confirmation

Confirmed by `endpoint_403_does_not_serve_stale_cache` and `active_forbidden_lock_rejects_stale_cached_data`, which fail if either carve-out regresses.

Revisit if the recovery-latency cost proves worse in practice than the endpoint load it buys — an mtime check on the credentials file would let `lock_active` release early, at the cost of a stat per render.

## More Information

- Implementation contract and error table: [rate-limit-segments.md](../specs/rate-limit-segments.md)
- Plugin-facing error codes gain `"Forbidden"`: [plugin-api.md](../specs/plugin-api.md)
- Prior art: Orca's `classifyClaudeOAuthUsageError` splits 403 into a terminal `missing-scope` (when the message names `user:profile`) and a recoverable stale-token case. linesmith collapses both into one variant — the user action is the same either way, and `doctor` already maps 403 to a single advice string
