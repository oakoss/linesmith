# Claude Code data files: where state actually lives

- Date: 2026-04-18
- Author: Jace Babin (w/ Claude Code)
- Scope: Complete map of every file, directory, and Keychain entry Claude Code reads or writes. Built from live filesystem inspection of an active Max-tier installation. Resolves source-of-truth questions raised in `cc-info-commands.md` and `ccstatusline-widget-internals.md`.

## Question

For each piece of state linesmith might want to surface — model, effort, tier, MCP servers, session metadata, project history — _where exactly does Claude Code persist it_? Stdin gives us a session snapshot; everything beyond that requires reading from disk or Keychain. This note enumerates the storage layout so segment authors know which file to open.

> **Companion doc:** `data-fetching-strategy.md` covers _how_ to read these efficiently — per-source cost matrix, mtime-based caching, JSONL incremental tail, OAuth cache stack, and the segment-driven lazy-load model.

## Sources

- Live filesystem inspection on macOS, Claude Code 2.1.114, Max-tier account, session `80dc0097-94e7-472e-b4c9-203167111c1e` on 2026-04-18
- `~/.claude/`, `~/.claude.json`, macOS Keychain service `Claude Code-credentials`
- Confirmed against `/status`, `/config`, `/usage`, `/mcp`, `/model`, `/effort`, `/stats` outputs

## Findings

### 1. Per-user settings cascade — three tiers, not two

`/status` reports "Setting sources: User settings, Shared project settings, Project local settings". The actual files:

| Tier                    | Path                                         | Owner               | Notes                                                                                             |
| ----------------------- | -------------------------------------------- | ------------------- | ------------------------------------------------------------------------------------------------- |
| User settings           | `~/.claude/settings.json`                    | user-global         | Theme, hooks, permissions, env vars, enabledPlugins, statusLine, alwaysThinkingEnabled, autoDream |
| Shared project settings | `{project_root}/.claude/settings.json`       | committed in repo   | Project conventions, intended to be shared with collaborators                                     |
| Project local settings  | `{project_root}/.claude/settings.local.json` | local-only override | Per-machine overrides, gitignored                                                                 |

ccstatusline's prior research noted only the first and third tiers; the **shared project** tier was undocumented. Linesmith's config-loading should support all three and document the cascade.

### 2. `~/.claude.json` — the per-user state file

Single 100-200kb JSON file at `~/.claude.json` (note: not under `~/.claude/`). Contains everything CC needs to remember about a user across sessions. Top-level keys we observed:

```text
$schema, additionalModelCostsCache, additionalModelOptionsCache,
anonymousId, autoConnectIde, autoUpdaterStatus, autoUpdates,
cachedDynamicConfigs, cachedExtraUsageDisabledReason,
cachedGrowthBookFeatures, cachedStatsigGates,
claudeAiMcpEverConnected, claudeCodeFirstTokenDate,
clientDataCache, companion, deepLinkTerminal,
disabledMcpjsonServers, effortCalloutDismissed,
effortCalloutV2Dismissed, enabledPlugins, env,
extraKnownMarketplaces, fallbackAvailableWarningThreshold,
feedbackSurveyState, firstStartTime,
hasAvailableSubscription, hasCompletedOnboarding,
hasOpusPlanDefault, hooks, includeCoAuthoredBy,
installMethod, lastReleaseNotesSeen, mcpServers,
numStartups, oauthAccount, opus47LaunchSeenCount,
permissions, projects, recommendedSubscription,
skillUsage, statusLine, subscriptionNoticeCount,
syntaxHighlightingDisabled, theme, thinkingMigrationComplete,
tipsHistory, toolUsage, userID
```

**The two most useful blocks for linesmith:**

#### `oauthAccount` — identity + tier derivation

```json
{
  "accountUuid": "uuid",
  "emailAddress": "user@example.com",
  "organizationUuid": "uuid",
  "hasExtraUsageEnabled": false,
  "billingType": "stripe_subscription",
  "accountCreatedAt": "ISO-8601",
  "subscriptionCreatedAt": "ISO-8601",
  "displayName": "Display",
  "organizationRole": "admin",
  "workspaceRole": null,
  "organizationName": "Org Name"
}
```

Source for `/status`'s "Email" and "Organization" lines. Tier ("Claude Max account") is _not_ a single field — it's derived from `billingType` + `hasOpusPlanDefault` + (presumably) `recommendedSubscription`. There's no `tier: "max"` shortcut.

#### `projects` — per-project session state

Keyed by absolute path (`/Users/x/.code/myrepo`). Each value:

```text
allowedTools, approvedMcpjsonServers, dontCrawlDirectory,
exampleFiles, exampleFilesGeneratedAt,
hasCompletedProjectOnboarding, hasTrustDialogAccepted,
hasTrustDialogHooksAccepted, lastAPIDuration,
lastAPIDurationWithoutRetries, lastCost, lastDuration,
lastFpsAverage, lastFpsLow1Pct, lastGracefulShutdown,
lastLinesAdded, lastLinesRemoved, lastModelUsage,
lastSessionId, lastSessionMetrics, lastToolDuration,
lastTotalCacheCreationInputTokens, lastTotalCacheReadInputTokens,
lastTotalInputTokens, lastTotalOutputTokens,
lastTotalWebSearchRequests, mcpContextUris, mcpServers,
projectOnboardingSeenCount, reactVulnerabilityCache,
rejectedMcpjsonServers
```

**`lastModelUsage`** is the highest-leverage field — pre-aggregated per-model breakdown of the _previous_ session in this project:

```json
{
  "claude-opus-4-6": {
    "inputTokens": 19419,
    "outputTokens": 16490,
    "cacheReadInputTokens": 6597296,
    "cacheCreationInputTokens": 160800,
    "webSearchRequests": 0,
    "costUSD": 4.812993
  },
  "claude-haiku-4-5-20251001": { ... },
  "claude-sonnet-4-6": { ... }
}
```

Enables a "previous session" or "session-over-session delta" segment without scanning JSONL.

### 3. `~/.claude/sessions/{pid}.json` — live process directory

Each running Claude Code instance writes a small JSON file keyed by its PID:

```json
{
  "pid": 83985,
  "sessionId": "80dc0097-94e7-472e-b4c9-203167111c1e",
  "cwd": "/path/to/project",
  "startedAt": 1776533165745,
  "version": "2.1.114",
  "kind": "interactive",
  "entrypoint": "cli"
}
```

Use cases for linesmith:

- Detect concurrent CC sessions on the same machine (count files in this dir)
- Map a PID back to its sessionId without parsing JSONL
- Reconcile against stale entries (CC may not always clean up on crash)

### 4. `~/.claude/projects/*/*.jsonl` — transcripts

Already covered in `jsonl-data-source.md`. One JSONL per session, one directory per workspace (with the `-`-encoded path). Authoritative for everything per-message: tokens, model, cost, timestamps, thinking blocks, `/model` and `/effort` stdout echoes.

### 5. macOS Keychain — OAuth credentials

Service `Claude Code-credentials`, account = `$USER`. Read via:

```bash
security find-generic-password -s "Claude Code-credentials" -w
```

Returns a JSON blob:

```json
{
  "claudeAiOauth": {
    "accessToken": "<token>",
    "refreshToken": null,
    "expiresAt": null,
    "scopes": [
      "user:file_upload",
      "user:inference",
      "user:mcp_servers",
      "user:profile",
      "user:sessions:claude_code"
    ],
    "subscriptionType": null
  }
}
```

**Important:** `subscriptionType` is **null for Max users**. CCometixLine's `OAuthCredentials` struct defines it as `Option<String>` — that's correct, but anyone hoping to use it for tier detection will get nothing. Tier comes from `oauthAccount.billingType` + `hasOpusPlanDefault` instead.

On Linux/Windows the same JSON lives at `{CLAUDE_CONFIG_DIR}/.credentials.json` or `~/.claude/.credentials.json`. The macOS install we tested **did not** have a file fallback — Keychain only.

### 6. `~/.claude/` directory layout

```text
~/.claude/
├── .anthropic/        # Anthropic-internal state
├── backups/           # Pre-update settings backups
├── cache/             # General cache
├── commands/          # Custom slash commands
├── config/            # Additional config
├── debug/             # Debug logs (mode 700)
├── downloads/         # Downloaded artifacts
├── file-history/      # File-edit history (mode 700)
├── ide/               # IDE integration state
├── paste-cache/       # Clipboard cache
├── plugins/           # Installed plugins (mode 700)
├── projects/          # JSONL transcripts (per workspace)
├── session-env/       # Per-session env captures
├── sessions/          # PID-keyed live session files (mode 700)
├── shell-snapshots/   # Shell environment snapshots
├── skills/            # Installed skills
├── statsig/           # Feature-flag SDK cache
├── tasks/             # Task state
├── teams/             # Teammate config
├── telemetry/         # Telemetry buffer
├── todos/             # Todo state
├── usage/             # (older usage cache?)
├── settings.json      # User settings (tier 1 above)
├── history.jsonl      # Cross-session prompt history (~4MB observed)
└── security_warnings_state_*.json
```

`debug/`, `file-history/`, `plugins/`, and `sessions/` are mode 700 (user-only). The rest are 755.

**Files of interest for linesmith** beyond the ones already covered:

- `history.jsonl` — cross-session prompt history. Could enable "n recent prompts" segment (privacy-sensitive — opt-in).
- `cache/` — appears unused or rotated. ccstatusline writes its own cache to `~/.cache/ccstatusline/` (XDG path) regardless. linesmith should follow XDG too (`~/.cache/linesmith/`) — keeping our cache out of `~/.claude/` avoids interfering with CC's own state.

### 7. Effort persistence — exhaustive search verdict

We searched every persistent location for the current effort level:

- `~/.claude/settings.json` → only `alwaysThinkingEnabled: true` (boolean toggle, not a level)
- `~/.claude.json` top level → `effortCalloutDismissed`, `effortCalloutV2Dismissed`, `unpinOpus47LaunchEffort` — all UI/feature flags, no current-level field
- `~/.claude.json` `projects.{path}` → no effort field
- `~/.claude.json` `additionalModelOptionsCache` → empty (`[]`) on this install — likely where model-specific defaults _would_ be stored
- `~/.claude/sessions/{pid}.json` → PID, sessionId, cwd, startedAt, version, kind, entrypoint. No model/effort.

**Verdict:** the current effort level is **not persisted to any file linesmith can read**. Sources:

1. **JSONL transcript tailing** — ccstatusline's approach. Reliability holes: misses `/effort`-only invocations (different stdout marker), misses "Kept model as X" no-op runs, misses "Cancelled" runs that write nothing, returns `undefined` on session start until first `/model` write.
2. **Hardcoded default fallback** — assume `xhigh` (the documented default) when no transcript signal exists.
3. **`additionalModelOptionsCache`** — speculative; may surface if user explicitly sets a per-model effort default.

Linesmith's effort segment should combine sources: parse JSONL backwards for both `/model` and `/effort` markers (two regexes), fall back to a configured default if neither is found.

### 8. The `~/.claude/settings.json` `env` block

Small on this install:

```json
{
  "CLAUDE_CODE_NO_FLICKER": "1",
  "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "1",
  "ENABLE_TOOL_SEARCH": "true"
}
```

CCometixLine reads `HTTPS_PROXY`/`HTTP_PROXY` from this block as a CC-specific proxy override (separate from process env). Confirmed shape — any string-valued env var the user adds here gets merged into CC's process env.

## Conclusions

1. **Three-tier settings cascade**, not two. `{project_root}/.claude/settings.json` (shared project) is the previously-undocumented middle layer.
2. **`~/.claude.json` is the per-user state file**, distinct from `~/.claude/settings.json`. It's where MCP servers, project history, OAuth account info, and feature flags live.
3. **Tier detection is derived, not direct.** No `subscription_type` or `tier` field exists. linesmith must compute from `oauthAccount.billingType` + `hasOpusPlanDefault` + endpoint response.
4. **`lastModelUsage` per project** unlocks pre-aggregated per-model history without JSONL scanning. Cheaper than ccusage's full-history aggregation when only the last session matters.
5. **Live session directory `~/.claude/sessions/`** is keyed by PID and gives sessionId↔PID mapping for free. Useful for parallel-CC detection.
6. **Effort is not persisted** to any file linesmith can read. JSONL tailing is the only path, with documented reliability holes.
7. **macOS Keychain `Claude Code-credentials`** is the OAuth source on Mac. No file fallback observed in this install.

## Implications / actions

- **Settings loader (lsm-jgn or similar):** support all three cascade tiers. Document the precedence.
- **Tier-detection helper (lsm-043):** combine `oauthAccount.billingType` + `hasOpusPlanDefault` rather than hoping for `subscription_type`. File ADR slice for the heuristic.
- **Effort segment (lsm-7ki was closed prematurely):** the JSONL approach has known holes; reopen with the dual-source plan (parse `/model` AND `/effort` markers, fall back to configured default).
- **MCP segment (lsm-8jl epic? or new):** read `mcpServers` from `~/.claude.json` + project-level `mcpServers` from `projects.{path}`. Show static config count; dynamic connection state remains a capability gap.
- **Previous-session segment (new):** read `lastModelUsage` from `~/.claude.json` `projects.{cwd}`. Cheap, pre-aggregated, per-model.
- **Parallel-session segment (new):** count files in `~/.claude/sessions/` to surface "3 CC sessions running" to user.
- **Credential reader:** Keychain on macOS via `security` subprocess (matching ccstatusline + CCometixLine), file fallback on Linux/Windows. No `subscription_type` extraction; that field is null for Max users.
- **Reference this doc from `lsm-y6m` ADR** when written — file/Keychain layout is foundational to the rate-limit segment.

## Open questions

- **Where does CC store the global default effort level** that backs the "(default)" suffix in `/model`? Hardcoded in binary? Migrated into `additionalModelOptionsCache` only after first override? Worth a fresh-install probe.
- **Schema of `additionalModelOptionsCache`** when populated — what does it look like after a user sets a per-model effort default?
- **`thinkingMigrationComplete`** — looks like a one-time migration flag. What did it migrate from?
- **`statsig/`, `cachedStatsigGates`, `cachedGrowthBookFeatures`** — feature-flag SDKs. Could explain why some features (e.g., `/extra-usage`) appear or hide. Probably out of scope for v0.1.
- **`history.jsonl` privacy.** Cross-session prompt history could enable interesting segments but is sensitive. Defer to user demand.

## Raw data

### Live `/api/oauth/usage` response (Max-tier user, 2026-04-18)

```json
{
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
}
```

Anthropic-internal codename buckets (`omelette`, `cowork`, `iguana_necktie`) appear to be forward-compat fields for unreleased features. linesmith's parser must use `serde(default)` + `Option<T>` everywhere and not error on unknown fields.

### Live `/api/oauth/usage` response (Max-tier user, 2026-08-05)

Two things changed against the 2026-04-18 capture above. The named
per-model buckets went empty — `seven_day_sonnet` joined
`seven_day_opus` and `seven_day_oauth_apps` at `null` — and the same
information reappeared in a `limits` array. [ADR-0030](../adrs/0030-model-scoped-usage-arrives-in-a-limits-array.md)
promotes that array out of the forward-compat catch-all into the typed
response model.

The array, verbatim:

```json
[
  {
    "group": "session",
    "is_active": false,
    "kind": "session",
    "percent": 5,
    "resets_at": "2026-08-05T19:50:00.333776+00:00",
    "scope": null,
    "severity": "normal"
  },
  {
    "group": "weekly",
    "is_active": false,
    "kind": "weekly_all",
    "percent": 60,
    "resets_at": "2026-08-08T13:59:59.333803+00:00",
    "scope": null,
    "severity": "normal"
  },
  {
    "group": "weekly",
    "is_active": true,
    "kind": "weekly_scoped",
    "percent": 82,
    "resets_at": "2026-08-08T14:00:00.334182+00:00",
    "scope": {
      "model": { "display_name": "Fable", "id": null },
      "surface": null
    },
    "severity": "warning"
  }
]
```

Note `percent` is an integer here where `utilization` is a float above,
and `scope.model.id` is null — which is why matching a session's model
to a bucket goes through the id's family token rather than an id-to-id
comparison (see [rate-limit-segments.md](../specs/rate-limit-segments.md)
§`rate_limit_7d_model`).

`session` and `weekly_all` agree with the named `five_hour` /
`seven_day` buckets exactly — same percentage, and the same `resets_at`
to the microsecond:

```text
five_hour  16.0%  resets 2026-08-06T00:50:00.576583Z
session    16     resets 2026-08-06T00:50:00.576583+00:00
seven_day  64.0%  resets 2026-08-08T14:00:00.576602Z
weekly_all 64     resets 2026-08-08T14:00:00.576602+00:00
```

So the array is additive rather than a replacement, and
`rate_limit_5h` / `rate_limit_7d` can keep reading the named fields.
[ADR-0030](../adrs/0030-model-scoped-usage-arrives-in-a-limits-array.md)
records this as uncaptured; it is captured now.

`is_active` is also worth pinning: the `weekly_scoped` bucket carried
`true` in one capture and `false` in another, taken hours apart on the
same account with the same model in use. That is the evidence behind not
modelling it — it is a server-side judgement about the account, and
nothing constrains it to agree with the local session.

The same response carried five new root keys alongside the April
codenames. Values below are normalized — the shapes are verbatim, the
numbers are not this account's:

```json
{
  "amber_ladder": null,
  "cinder_cove": null,
  "nimbus_quill": null,
  "member_dashboard_available": false,
  "spend": {
    "auto_reload": null,
    "balance": null,
    "can_purchase_credits": false,
    "can_toggle": false,
    "cap": { "credits": { "amount_minor": 0, "exponent": 2 }, "money": null },
    "disabled_reason": null,
    "disclaimer": "Usage credits cover you when you hit your plan limits. […]",
    "enabled": true,
    "limit": { "amount_minor": 0, "currency": "USD", "exponent": 2 },
    "percent": 0,
    "severity": "normal",
    "used": { "amount_minor": 0, "currency": "USD", "exponent": 2 }
  }
}
```

`spend` is the one worth watching: it carries money fields in minor
units with an explicit `exponent`, and a `percent`/`severity` pair
mirroring the `limits` entries. Nothing reads it today, so it stays in
the catch-all under ADR-0030's promote-on-dependency rule.

### Per-project `lastModelUsage` shape

```json
{
  "claude-opus-4-6": {
    "inputTokens": 19419,
    "outputTokens": 16490,
    "cacheReadInputTokens": 6597296,
    "cacheCreationInputTokens": 160800,
    "webSearchRequests": 0,
    "costUSD": 4.812993
  }
}
```

Keys are model IDs as they appear in JSONL. `costUSD` is locally computed (matches CCometixLine's `total_cost_usd` / ccusage's local-cost behavior).
