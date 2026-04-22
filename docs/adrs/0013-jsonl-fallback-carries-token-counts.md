# JSONL fallback carries raw token counts, not synthesized percentages

- Status: accepted
- Date: 2026-04-22
- Deciders: Jace
- Amends: [ADR-0011](0011-rate-limit-data-source.md) — specifically the "Return JSONL-derived UsageData tagged as estimated" step in [§Fallback cascade](0011-rate-limit-data-source.md#fallback-cascade) and the implicit flat-struct + `UsageSource` tag shape the cascade relied on. ADR-0011's endpoint, auth, cache, and credential-cascade decisions stand unchanged.

## Context and Problem Statement

ADR-0011 declared the OAuth `/api/oauth/usage` endpoint the primary rate-limit data source, with JSONL transcript aggregation as the terminal fallback. The fallback step says: "Return JSONL-derived `UsageData` tagged as estimated." When we went to implement that step, the concrete type shape didn't carry over cleanly. `UsageBucket.utilization` requires a `Percent`, the JSONL aggregator produces raw token counts, and tier detection was explicitly deferred by ADR-0011 §Tier handling. How should the cascade surface JSONL-backed data when it cannot honestly produce a percentage?

## Decision Drivers

- Correctness-first is the project's third differentiator. Shipping a confidently-wrong number is worse than shipping honest partial data or hiding.
- Tier detection is out of scope per ADR-0011 §Tier handling. We cannot compute a percentage without it.
- Ecosystem precedent: neither ccstatusline nor CCometixLine synthesizes a percentage under JSONL. ccstatusline emits error tags (`[No credentials]`, `[Rate limited]`); CCometixLine hides the segment entirely.
- Plugin authors and segment code should not be able to accidentally read Endpoint-shaped data when the cascade returned JSONL-shaped data. The old flat-struct + `UsageSource` tag permitted that statically.

## Considered Options

- **Hardcoded conservative tier ceiling.** Pick Max-tier caps (5h ≈ 300M tokens, 7d ≈ 1.2B) and compute `%` against them. Under-reports for Pro/Free users.
- **Distinct data variant carrying raw token counts.** Split `UsageData` into `Endpoint(...)` and `Jsonl(...)` enum arms; segments branch on variant and render tokens under JSONL.
- **Token side-channel on the existing struct.** Add `Option<u64>` token-count fields next to `utilization: Percent`; segments prefer tokens when `source == Jsonl`.

## Decision Outcome

Chosen option: **distinct enum variant**, because (a) it's the only option that refuses to invent data the cascade doesn't have, (b) the unit change (`22%` → `~420k`) makes the mode switch visible to users — a second signal beyond the `~` stale-marker prefix that survives `NO_COLOR` and 16-color terminals, (c) it matches ccstatusline's philosophy of never synthesizing a percentage even though their implementation goes a different direction (error tag vs useful fallback), and (d) it eliminates the impossible-in-practice states the flat struct permitted (`UsageData { source: Jsonl, extra_usage: Some(...) }` compiles but cannot occur).

### Shape

```rust
#[non_exhaustive]
pub enum UsageData {
    Endpoint(EndpointUsage),
    Jsonl(JsonlUsage),
}

#[non_exhaustive]
pub struct EndpointUsage {
    pub five_hour:            Option<UsageBucket>,
    pub seven_day:            Option<UsageBucket>,
    pub seven_day_opus:       Option<UsageBucket>,
    pub seven_day_sonnet:     Option<UsageBucket>,
    pub seven_day_oauth_apps: Option<UsageBucket>,
    pub extra_usage:          Option<ExtraUsage>,
    pub unknown_buckets:      HashMap<String, serde_json::Value>,
}

/// `seven_day` is always populated (zero-valued on empty transcripts);
/// `five_hour` is `None` when the current 5h block has no activity.
pub struct JsonlUsage {
    pub(crate) five_hour: Option<FiveHourWindow>,
    pub(crate) seven_day: SevenDayWindow,
}

pub struct FiveHourWindow {
    pub(crate) tokens:  TokenCounts,   // four-category breakdown owned by aggregator; segments call `.total()`
    pub(crate) ends_at: DateTime<Utc>, // invariant: ends_at == block.start + Duration::hours(5)
}

pub struct SevenDayWindow {
    pub(crate) tokens: TokenCounts,    // no reset_at — rolling window, no hard reset
}
```

`UsageSource` is deleted; the `UsageData` variant IS the provenance tag. `#[non_exhaustive]` sits on `UsageData` (future variant room) and `EndpointUsage` (upstream Anthropic can ship new bucket categories). JSONL-side structs lock their fields to the aggregator contract and don't need it. `JsonlUsage`, `FiveHourWindow`, `SevenDayWindow` use `pub(crate)` fields + smart constructors (`JsonlUsage::new`, `FiveHourWindow::new`, `SevenDayWindow::new`) so the aggregator owns the invariants.

### Cascade return

Step 7 of ADR-0011 §Fallback cascade changes from:

> 5. JSONL fallback: ... Return JSONL-derived UsageData tagged as estimated → done.
> 6. If JSONL also fails: Return the original error recorded earlier.

to:

> 7. JSONL fallback: scan transcripts, aggregate into 5h blocks and 7d window, and return `Ok(UsageData::Jsonl(...))`.
> 8. If JSONL also empty or errors: surface the original error (`NoCredentials`, `Timeout`, ...).

The `Ok` vs `Err` shift is the user-visible behavior change. Previously the segment saw `Err(NoCredentials)` and rendered `[No credentials]` even when JSONL had data; now it sees `Ok(UsageData::Jsonl(...))` and renders `~5h: 420k`.

### Per-segment render in JSONL mode

The shape decision drives these effects:

| Segment               | Endpoint mode       | JSONL mode                                      |
| --------------------- | ------------------- | ----------------------------------------------- |
| `rate_limit_5h`       | `5h: 22%`           | `~5h: 420k` (shape change: tokens, not percent) |
| `rate_limit_7d_reset` | `7d reset: 2d 14hr` | **hidden** (rolling window has no hard reset)   |
| `extra_usage`         | `extra: $12.50`     | **hidden** (transcripts carry no overage data)  |

Full per-segment table (all six render rules) lives in [rate-limit-segments.md §JSONL-fallback display](../specs/rate-limit-segments.md); that spec owns segment behavior and must be the single source of truth.

`FiveHourBlock.usage_limit_reset` (from [jsonl-aggregation.md](../specs/jsonl-aggregation.md)) is deliberately NOT consumed by segments; `lsm-ghpj` tracks verification before wiring. Until then, `rate_limit_5h_reset` uses `FiveHourWindow.ends_at` (= `block.start + 5h`), matching ccstatusline's `getUsageWindowFromBlockMetrics`.

### Consequences

- Good, because the enum variant + `~` prefix give two signals for the mode switch; the unit change (`22%` → `~420k`) survives `NO_COLOR`, 16-color terminals, and a user-set `stale_marker = ""`. A Pro-tier user hitting JSONL fallback sees `~5h: 420k` rather than `5h: 7%` synthesized against a Max ceiling they don't have.
- Good, because `extra_usage` and per-model segments become impossible to accidentally render under JSONL. Old shape permitted `UsageData { source: Jsonl, extra_usage: Some(...) }` through the type system.
- Good, because `TokenCounts` preserves the aggregator's per-category breakdown. Future burn-rate or cache-hit-ratio segments don't need a re-plumbing pass.
- Good, because linesmith diverges from ccstatusline (error tags) and CCometixLine (hides entirely) in the direction of more-useful output. We ship partial data where both peers do not — and users running on Free/Pro/offline get a functional rate-limit line where they currently get nothing useful.
- Bad, because shipping tokens where users expect a percentage is a vocabulary mismatch. Users coming from ccstatusline will see `~5h: 420k` and have to learn the new shape. The divergence is worth it (correctness over familiarity), but it is a cost.
- Bad, because segments now carry two match arms in their render path. The `rate_limit_format` module grows a `format_jsonl_tokens` helper alongside `format_percent`. Modest complexity cost, but the branches genuinely render different shapes, so collapsing them would re-introduce the "which mode is this?" ambiguity we're trying to remove.
- Bad, because `rate_limit_7d_reset` hides under JSONL. Users who configured that segment will see it disappear silently during endpoint failures. We accept this because there is no honest derivation for a 7d reset from a rolling JSONL window; faking one is the exact failure mode this ADR rejects.
- Neutral, because `UsageSource` is deleted. Plugins that read `ctx.usage.source` break; pre-1.0, no plugin ecosystem exists, and the plugin-API ctx mirror ([plugin-api.md](../specs/plugin-api.md)) exposes the variant as a tagged-map `#{ kind: "endpoint" | "jsonl", ... }` following the same shape conventions used for every other `Result`-shaped accessor.

### Confirmation

Revisit if:

- Users report confusion over the `420k` vs `22%` shape divergence strong enough that tier-ceiling synthesis would be better UX than honesty. (A new ADR resolving tier detection would also close this loop.)
- `usageLimitResetTime` provenance is verified (follow-up bead) — `FiveHourWindow.ends_at` may then be replaced by or augmented with the Claude Code-provided timestamp.
- A concrete plugin-author use case emerges for per-category token breakdown (`TokenCounts`) under JSONL; confirms the `u64`-vs-`TokenCounts` choice in this ADR.

## Pros and Cons of the Options

### Hardcoded conservative tier ceiling

- Good: zero type changes; segments keep their single-match render path.
- Bad: synthesizes a wrong number for every Pro/Free user on the fallback path.
- Bad: rotates silently when Anthropic adjusts tier caps. No forcing function to catch the drift.
- Bad: ecosystem precedent explicitly against this — ccstatusline considered it and chose error tags; CCometixLine never built JSONL fallback for usage at all.

### Distinct enum variant carrying raw token counts (chosen)

- Good: unit change signals the mode switch; correctness survives tier-cap drift.
- Good: impossible-in-practice states become unrepresentable.
- Good: future segments (burn-rate, cache-hit ratio) get the per-category breakdown for free.
- Bad: two match arms per segment; modest complexity lift.
- Bad: user vocabulary shifts under fallback.

### Token side-channel on existing struct

- Good: smallest type-diff; adds one `Option<u64>` field.
- Bad: two "magnitude" fields with mutually-exclusive semantics on one struct. Easy to misuse — future contributor reaches for `utilization` out of habit, silently renders a Max-tier ceiling percentage against JSONL data.
- Bad: provenance still lives in the `UsageSource` tag that this ADR otherwise deletes.

## More Information

- [ADR-0011](0011-rate-limit-data-source.md) — primary data-source ADR amended here. Endpoint, auth, credential cascade, cache stack, tier handling scope all stand unchanged.
- [ADR-0010](0010-data-fetching-architecture.md) — `DataContext` + `OnceCell` lazy-fetch pattern unchanged.
- [specs/data-fetching.md](../specs/data-fetching.md) §OAuth fallback cascade and §OAuth usage cache stack carry the `UsageData` type declaration and the cascade step language.
- [specs/rate-limit-segments.md](../specs/rate-limit-segments.md) §JSONL-fallback display carries the per-segment render table.
- [specs/jsonl-aggregation.md](../specs/jsonl-aggregation.md) is the data source the new `JsonlUsage` wraps; `FiveHourBlock.usage_limit_reset` remains declared there but unconsumed.
- Research: `docs/research/ccstatusline-widget-internals.md` (error-tag behavior on failure), `docs/research/jsonl-data-source.md` §`usageLimitResetTime` (unverified provenance).
- Tracked beads: `lsm-xhu` (implementation); `lsm-ghpj` (verify `usageLimitResetTime` provenance before wiring `FiveHourBlock.usage_limit_reset` into the reset segment).
