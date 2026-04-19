# Claude Code info-category slash commands: data sources

- Date: 2026-04-18
- Author: Jace Babin (w/ Claude Code)
- Scope: Identify the data source behind each built-in Claude Code info command (`/usage`, `/stats`, `/config`, `/cost`, `/status`, `/context`, ...) and flag any whose data linesmith cannot replicate from stdin + JSONL + settings.

## Question

Do any Claude Code built-in info commands surface data that is NOT available in the statusline stdin payload, JSONL transcripts, or local settings? If so, that's a new candidate data source (or a capability gap) for linesmith segments.

## Sources

- `https://code.claude.com/docs/en/commands.md` — slash-command table
- `https://code.claude.com/docs/en/costs.md` — `/cost` semantics
- `https://code.claude.com/docs/en/context-window.md` — `/context` behavior
- `https://code.claude.com/docs/en/statusline.md` — stdin schema (context reference)

## Findings

| Command        | Data source (docs verdict)                                                                      | Shows                                                                                           | Replicable from stdin/JSONL/settings? |
| -------------- | ----------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- | ------------------------------------- |
| `/cost`        | **Local** — computed from session token counts                                                  | Input/output/cache tokens, USD estimate, API duration, wall-clock duration, lines added/removed | Yes — stdin `cost.*` + token fields   |
| `/stats`       | **Local** — JSONL scan across `~/.claude/projects/`                                             | Daily usage patterns, session history, streaks, model preferences                               | Yes — ccusage-style JSONL aggregation |
| `/config`      | **Local** — reads/writes `~/.claude/settings.json` + `~/.claude/settings.local.json`            | Theme, model default, output style, permissions, hooks, MCP config                              | Yes — direct settings read            |
| `/status`      | **Local + probe** — reads settings + session state, may make a lightweight connectivity check   | CLI version, current model, account email, connectivity, plan tier                              | Mostly yes — connectivity probe aside |
| `/context`     | **In-process memory** — per-component breakdown of CLAUDE.md, skills, transcript, tool defs     | Colored grid of token usage by category with optimization suggestions                           | **No** — see Gap #1 below             |
| `/usage`       | **Undocumented** — docs list it but do not specify whether it fetches fresh data or cached only | Plan usage limits and rate-limit status                                                         | **Unclear** — see Gap #2 below        |
| `/extra-usage` | **Undocumented** — configure UI, may validate against API                                       | Extra-usage configuration                                                                       | Unclear                               |

Confidence column reflects what the Claude Code docs actually say, not what seems plausible. `/usage` is described in the commands table with a one-line summary and no implementation detail; the docs are silent on whether it hits a live endpoint.

### Gap #1: `/context` per-component token breakdown

`/context` visualizes context usage by category — CLAUDE.md, skills, transcript, tool definitions, MCP resources. The statusline stdin payload's `context_window.current_usage` only exposes aggregates: `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`. No per-component split.

The per-component data comes from Claude Code's in-process state (loaded skills, parsed CLAUDE.md, MCP tool-definition sizes). It is **not** serialized to JSONL or settings, so linesmith cannot replicate the breakdown externally. The aggregate `context_window.*` fields remain the authoritative signal for a statusline.

**Implication:** no new segment unlocked. "Per-component context breakdown" stays inside Claude Code's `/context` command; linesmith's `context_window` / `context_bar` segments continue to show aggregates.

### Gap #2: `/usage` data source

The docs list `/usage` in the commands table with the description "Show plan usage limits and rate limit status" and no further explanation. Two plausible implementations:

1. **Echo cached stdin data** — rephrase the `rate_limits.{five_hour, seven_day}.{used_percentage, resets_at}` fields already populated on the statusline payload (Pro/Max tiers). In that case `/usage` adds nothing linesmith doesn't already have.
2. **Live API fetch** — call an Anthropic billing/limits endpoint for real-time TPM/RPM headroom. In that case `/usage` exposes a signal neither stdin nor JSONL expose — but we also can't call that endpoint from a statusline process without the user's session credentials and a documented API surface.

The docs don't distinguish these. Verification requires running `/usage` in a live session and comparing output against stdin `rate_limits` snapshots.

**Update after further research:** `ccstatusline-widget-internals.md` §2 identifies `GET https://api.anthropic.com/api/oauth/usage` as the endpoint ccstatusline hits for the same data. Claude Code's `/usage` almost certainly uses the same endpoint given the schema match (`five_hour.{utilization, resets_at}` + `seven_day.{utilization, resets_at}`). Live-invocation confirmation still filed as lsm-4qd.

### Bonus finding: rate-limit schema detail

Docs confirm (statusline.md §context_window schema) that `rate_limits.{five_hour, seven_day}` each expose `{used_percentage, resets_at}`. Already-formatted percentages and a reset timestamp — linesmith doesn't have to compute either. This was not explicit in prior research notes; update `lsm-y6m` ADR when it's written.

## Conclusions

1. **Five of seven info commands are fully replicable** from known sources (stdin, JSONL, settings). `/cost`, `/stats`, `/config`, `/status`, and the parts of `/usage` that echo stdin all map to data linesmith already ingests.
2. **`/context` per-component breakdown is a capability gap, not a data gap** — it's computed inside Claude Code's process and isn't serialized anywhere we can read. Linesmith's context segments stay aggregate-only.
3. **`/usage`'s implementation is undocumented.** The interesting question — does it call a live API endpoint? — can only be answered by live invocation. Filed as a spike.
4. **Rate-limit fields are pre-formatted** (`used_percentage`, `resets_at`). lsm-y6m's rate-limit segments have less derivation work than the jsonl-data-source note implied.

## Implications / actions

- **No new source surfaced from the info-command angle** — every info command the docs describe draws from stdin, JSONL, or settings. But parallel research in `ccstatusline-widget-internals.md` did uncover a fourth source: `GET https://api.anthropic.com/api/oauth/usage`, which ccstatusline hits for authoritative rate-limit data. That's the actual new data source for linesmith, not anything `/usage` exposed on its own.
- **File a live-invocation spike** for `/usage` behavior — verify whether it hits a fresh endpoint. If yes, document the endpoint; if no, close as "echoes stdin."
- **Update lsm-y6m when ADR is written** to reflect the `{used_percentage, resets_at}` schema — rate-limit segments read pre-computed percentages, not raw counts.
- **Close lsm-043** (tier-aware rate-limit fallback research) with an update: the stdin `rate_limits` schema gives Pro/Max users a ready-made percentage, and the JSONL aggregation path only matters for free-tier fallback. API-tier remains out of scope.

## Open questions

- **`/usage` live-vs-cached.** Needs live invocation to confirm — filed as spike.
- **`/extra-usage`.** May touch the billing API; undocumented. Same spike could cover it.
- **`/status` connectivity probe.** Does it call a specific endpoint? Relevant if we ever want a "CC reachable?" signal, but not v0.1.
