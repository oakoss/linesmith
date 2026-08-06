---
linesmith-core: major
linesmith: major
---

**BREAKING (pre-1.0): `ModelInfo` gained an `id: Option<String>` field.**

`ModelInfo` is a public, non-`#[non_exhaustive]` struct, so the field breaks downstream crates that construct it: add `id: None` (or the parsed value) to constructions. Exhaustive matches are unaffected — the type has no pattern-matching consumers in practice. Making it `#[non_exhaustive]` would block external construction outright without a smart constructor, so that stays for the `#[non_exhaustive]` audit in `lsm-q16r`.

The field carries the model identifier the Claude Code payload has always sent and `parse_model` discarded (`claude-fable-5`, `us.anthropic.claude-sonnet-4-20250514-v1:0`). `ModelInfo::family()` extracts the family token from it.

**New: the `rate_limit_7d_model` segment.**

Surfaces the weekly usage bucket the OAuth endpoint scopes to a single model — `✧ Fable: 82.0%`. `visibility = "smart"` (default) renders it only while the session is on that model; `visibility = "always"` renders it whenever a scoped bucket exists. Opt in by adding `rate_limit_7d_model` to `[line].segments`.

Per [ADR-0030](docs/adrs/0030-model-scoped-usage-arrives-in-a-limits-array.md), the data now arrives in a `limits` array rather than the `seven_day_opus` / `seven_day_sonnet` / `seven_day_oauth_apps` fields, which are null in the live response. That array is promoted out of the `#[serde(flatten)]` catch-all into typed `UsageLimit` / `LimitScope` / `LimitModel` / `LimitKind` / `LimitSeverity`, threaded through `EndpointUsage` and `CachedData`, and mirrored to rhai plugins as `ctx.usage.data.limits`. Plugins reading it from `ctx.usage.data.unknown_buckets` must move to the typed path; `ctx.status.model.id` is also newly exposed.

A per-item-tolerant deserializer keeps one malformed element from failing the whole response and dropping the endpoint to the JSONL fallback.

`linesmith doctor` stops reporting `limits` as a forward-compat key, and the five codenamed keys observed alongside it on 2026-08-05 (`amber_ladder`, `cinder_cove`, `member_dashboard_available`, `nimbus_quill`, `spend`) are recorded in `docs/research/claude-data-files.md` and added to `RESEARCH_DOCUMENTED_BUCKETS`.
