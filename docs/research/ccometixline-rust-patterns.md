# CCometixLine: Rust-peer cross-check and implementation patterns

- Date: 2026-04-18
- Author: Jace Babin (w/ Claude Code)
- Scope: Confirm CCometixLine (~2.7k⭐ Rust/TOML Claude Code statusline) uses the same rate-limit data source as sirmalloc/ccstatusline, and catalog Rust-specific implementation choices worth adopting or avoiding.

## Question

Does CCometixLine (the closest architectural peer to linesmith — Rust, TOML config) reveal a rate-limit data source that `ccstatusline-widget-internals.md` didn't already document? What Rust-specific patterns does it use for HTTP, caching, credentials, and binary size that linesmith should adopt or explicitly reject?

## Sources

- `github.com/Haleclipse/CCometixLine` — MIT, master branch, commit `a73b1665` (pushed 2026-03-14), 2,709⭐
  - `Cargo.toml` — dependencies
  - `src/utils/credentials.rs` — OAuth token reading
  - `src/core/segments/usage.rs` — HTTP fetch + cache + widget render
  - `src/core/segments/cost.rs` — stdin cost reader
- Prior doc `ccstatusline-widget-internals.md` for comparison

## Findings

### 1. Data source is identical to ccstatusline

CCometixLine hits `GET {api_base_url}/api/oauth/usage` (default `https://api.anthropic.com`) with:

```text
Authorization: Bearer {oauth_access_token}
anthropic-beta: oauth-2025-04-20
User-Agent: claude-code/{npm_version}
```

Same endpoint, same beta header, same auth scheme. Response shape is identical: `{five_hour: {utilization, resets_at}, seven_day: {utilization, resets_at}}` (CCometixLine doesn't decode the `extra_usage` object, but the wire payload is the same).

**Verdict:** no new data source. The OAuth endpoint is the de-facto standard for Claude Code rate-limit tooling.

### 2. Side-by-side: ccstatusline vs CCometixLine

| Aspect             | ccstatusline (TS)                                    | CCometixLine (Rust)                          | Notes for linesmith                                        |
| ------------------ | ---------------------------------------------------- | -------------------------------------------- | ---------------------------------------------------------- |
| Endpoint           | `/api/oauth/usage`                                   | `/api/oauth/usage`                           | Same                                                       |
| Base URL           | hardcoded `api.anthropic.com`                        | configurable per segment (default same)      | Adopt CCometixLine's config knob                           |
| Beta header        | `oauth-2025-04-20`                                   | `oauth-2025-04-20`                           | Same                                                       |
| Timeout            | 5000ms                                               | configurable, default 2s                     | Prefer CCometixLine's tighter default                      |
| HTTP client        | Node `https` module                                  | `ureq` 3.0 (sync, pure-Rust)                 | Confirms our crate pick                                    |
| JSON               | `zod` runtime validation                             | `serde` derive                               | Confirms our crate pick                                    |
| Cache format       | JSON                                                 | pretty-printed JSON                          | Compact is fine; pretty wastes bytes                       |
| Cache location     | `~/.cache/ccstatusline/usage.json`                   | `~/.claude/ccline/.api_usage_cache.json`     | Prefer XDG; stay out of `~/.claude/`                       |
| Cache TTL          | 180s                                                 | 300s                                         | 180-300s both reasonable                                   |
| Lock file          | yes (30s, prevents API spam)                         | **no**                                       | Adopt ccstatusline's lock — per-prompt invocation needs it |
| 429 handling       | respects `Retry-After`, 300s default                 | **none visible**                             | Adopt ccstatusline's approach                              |
| Stale-on-failure   | yes                                                  | yes                                          | Standard                                                   |
| Keychain (macOS)   | dump + prefix-match (multi-account)                  | single `security find-generic-password` call | ccstatusline's is more robust                              |
| Credentials file   | `{CLAUDE_CONFIG_DIR}/.credentials.json`              | same + fallback to `~/.claude/`              | Both correct                                               |
| User-Agent         | unspecified                                          | `claude-code/{npm view ...}` per cache miss  | **Avoid** — subprocess on fast path                        |
| Proxy              | `HTTPS_PROXY` env                                    | `~/.claude/settings.json` `env` block        | Support both (superset)                                    |
| Widget model       | 4 widgets (block, block-reset, weekly, weekly-reset) | 1 combined segment (5h% + 7d reset)          | Offer both — separate widgets + combined option            |
| Error states       | explicit: `[No credentials]`, `[Timeout]`, etc.      | silent `None` (hides segment)                | Adopt ccstatusline's visible errors                        |
| Icon UX            | plain text                                           | Nerd Font circle-slice icons (8-step)        | Interesting option for `icon = true` config                |
| Memory cache layer | yes                                                  | no                                           | File-only is fine for single-invocation CLI                |
| Cargo size flags   | n/a                                                  | **none** (default profile)                   | linesmith should set `lto`, `codegen-units=1`, `strip`     |

### 3. Anti-pattern to avoid: NPM subprocess for User-Agent

CCometixLine shells out to `npm view @anthropic-ai/claude-code version` every cache miss to build the `User-Agent: claude-code/{version}` header. NPM cold-starts can add 300-500ms on a network lookup; even cached, the Node startup alone is ~50ms. This is miscategorized work in a fast-path binary.

**linesmith:** hardcode `User-Agent: linesmith/{CARGO_PKG_VERSION}` at compile time with `env!()`. Zero runtime cost. If spoofing Claude Code's UA is ever desired, make it a config option, not the default, and derive the version via a cached file read rather than NPM.

### 4. Cache hygiene details worth copying

From ccstatusline (better hygiene than CCometixLine):

- **Lock file with TTL:** `~/.cache/linesmith/usage.lock` with 30s blockedUntil. Prevents per-prompt API spam when a run is mid-flight.
- **Error-vs-data TTLs are separate:** 30s for error cache, 180-300s for data cache. Short enough to recover from transient errors, long enough to avoid hammering on permanent ones.
- **429 `Retry-After`:** honor integer-seconds and HTTP-date formats. Default 300s backoff when header missing.
- **Auth-failure distinct from timeout:** resolve credentials _before_ lock check so `no-credentials` isn't masked as `timeout`.

From CCometixLine:

- **Config-driven base URL and TTL:** `api_base_url`, `cache_duration`, `timeout` are segment options in the config. Useful for self-hosters / proxies. Adopt.

### 5. Release profile gaps in CCometixLine's Cargo.toml

CCometixLine ships with default release profile — no `lto`, no `strip`, no `panic = "abort"`, no `codegen-units = 1`. This leaves 20-40% binary-size and startup-time gains on the table for a tool that runs per-prompt.

**linesmith:** apply the standard CLI release profile:

```toml
[profile.release]
lto = true
codegen-units = 1
strip = true
panic = "abort"
```

### 6. Widget composition: offer both models

ccstatusline exposes four widgets so users can mix-and-match (e.g., only BlockTimer + WeeklyUsage, no reset timers). CCometixLine exposes one combined segment (`{5h%} · {7d-reset}`) that's easier to add but less flexible.

Linesmith should offer both: separate `rate_limit_5h`, `rate_limit_5h_reset`, `rate_limit_7d`, `rate_limit_7d_reset` segments _and_ a combined `rate_limit` convenience segment that mirrors CCometixLine's default. Presets can pick the combined one by default; power users can compose.

## Conclusions

1. **The OAuth endpoint is canonical.** Both dominant Claude Code statusline tools hit `api.anthropic.com/api/oauth/usage` with the same beta header. There is no alternative endpoint or scraping path worth investigating.
2. **CCometixLine confirms ureq + serde + chrono as the Rust stack.** No surprises against our current plan.
3. **ccstatusline has better cache hygiene; CCometixLine has better config surface.** Linesmith should take lock-file + 429 handling from ccstatusline and config-driven URL/TTL/timeout from CCometixLine.
4. **Release-profile tuning is a freebie.** CCometixLine leaves 20-40% size gains on the table. linesmith should bake `lto + strip + codegen-units=1 + panic=abort` into the release profile from day one.
5. **User-Agent should be compile-time.** Avoid CCometixLine's NPM subprocess anti-pattern.
6. **Expose both widget models.** Four separate segments (flexibility) + one combined segment (convenience) covers both user populations.

## Implications / actions

- **Update lsm-y6m ADR scope (when drafted)** to cite the universal endpoint, the combined-vs-separate widget decision, and the cache hygiene requirements.
- **File new beads** for implementation slices:
  - `ureq`-based usage fetcher with configurable base URL, timeout, and beta header
  - Two-tier cache (memory + file) with lock file, matching ccstatusline's TTL split
  - 429 handling with `Retry-After` parser
  - Credential reader: macOS Keychain (with multi-account fallback like ccstatusline) + `.credentials.json` file fallback
  - Combined `rate_limit` segment mirroring CCometixLine
- **Update Cargo.toml** with the release-profile block — cheap one-line PR worth shipping independently of the rate-limit epic.
- **Close lsm-kau** with the comparison matrix as close-reason.
- **Skip further competitor research** for claude-powerline, claudia-statusline, felipeelias, claude-hud. The OAuth endpoint is universal; only implementation details vary, and the ccstatusline + CCometixLine pair covers the design space well enough for linesmith v0.1.

## Open questions

- **Keychain multi-account on macOS.** ccstatusline dumps the keychain and picks the newest `Claude Code-credentials*` entry. CCometixLine uses `-a $USER` to scope by user. If a machine has multiple Claude accounts under one Unix user, ccstatusline's approach wins. Worth verifying which pattern is safer.
- **`extra_usage` object.** CCometixLine doesn't decode it. ccstatusline does (cents-denominated `monthly_limit`/`used_credits`). Worth a dedicated segment in v0.1 if it proves useful for paid users.
- **`npm view` fallback when User-Agent actually matters.** Does the endpoint care about the UA string? If it rate-limits unknown UAs, we'd need to look Claude-Code-ish. Haven't tested.

## Raw data

### Observed CCometixLine config knobs

```text
segment.options:
  api_base_url   string   default "https://api.anthropic.com"
  cache_duration u64      default 300  (seconds)
  timeout        u64      default 2    (seconds)
```

### Observed Cargo.toml dependency set (relevant subset)

```toml
ureq = { version = "3.0", features = ["json"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
chrono = { version = "0.4", features = ["serde"] }
dirs = "6.0"
toml = "1.0"
```

### Circle-slice icon mapping (from CCometixLine)

```text
 0-12%  → U+F0A9E (circle_slice_1)
13-25%  → U+F0A9F
26-37%  → U+F0AA0
38-50%  → U+F0AA1
51-62%  → U+F0AA2
63-75%  → U+F0AA3
76-87%  → U+F0AA4
88-100% → U+F0AA5 (full circle)
```
