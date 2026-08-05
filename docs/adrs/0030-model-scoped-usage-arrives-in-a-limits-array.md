# Model the usage endpoint's `limits` array instead of the named per-model buckets

- Status: accepted
- Date: 2026-08-05
- Deciders: Jace
- Amends: [ADR-0011](0011-rate-limit-data-source.md) — the `UsageApiResponse` shape in [§Endpoint contract](0011-rate-limit-data-source.md#endpoint-contract), the Decision Outcome's clause (b), and the §Consequences bullet naming `seven_day_sonnet` / `seven_day_opus`, all of which describe per-model data in a form the endpoint no longer returns. ADR-0011's choice of endpoint, auth, cache stack, credential cascade, and fallback ordering stand unchanged. Also amends [ADR-0013](0013-jsonl-fallback-carries-token-counts.md)'s restatement of the `EndpointUsage` struct.

## Context and Problem Statement

ADR-0011 chose the OAuth `/api/oauth/usage` endpoint partly because it returns per-model weekly usage "that JSONL aggregation can't produce", and documented that data as three named fields: `seven_day_opus`, `seven_day_sonnet`, `seven_day_oauth_apps`.

All three are now `null`, and the reference capture shows the situation was always thinner than the ADR's prose suggests: on 2026-04-18 (`docs/research/claude-data-files.md`) `seven_day_opus` and `seven_day_oauth_apps` were _already_ null, and `seven_day_sonnet` carried `utilization: 0.0`. Exactly one field ever held a value, and that value was zero. So ADR-0011's clause (b) — per-model data "that JSONL aggregation can't produce" — rested on a single zero-valued bucket even at the time.

The same information now arrives in a `limits` array, where each entry carries a `kind` (`session`, `weekly_all`, `weekly_scoped`), a `percent`, and — for scoped entries — a `scope.model.display_name`. The `session` and `weekly_all` entries cover the same two windows `five_hour` and `seven_day` already provide, so the array is additive rather than a replacement; only the per-model path moved, and it now carries a real value (82%) where the named field never did. Whether the two representations agree field-for-field was not captured — the reference capture is the `limits` array alone — so `rate_limit_5h` / `rate_limit_7d` keep reading the named fields and this ADR claims nothing about reconciling them.

When `seven_day_sonnet` went null is unknown: populated-at-zero on 2026-04-18, null on 2026-08-05, no capture in between.

`limits` currently lands in the `#[serde(flatten)] unknown_buckets` catch-all ADR-0011 added for codenamed buckets. `lsm-zgju` wants to surface the scoped bucket as a segment. How should the response shape represent model-scoped usage?

## Decision Drivers

- ADR-0011's rationale for choosing this endpoint over JSONL rests on per-model data being available. That justification has to remain true, or the endpoint decision itself weakens.
- [rate-limit-segments.md](../specs/rate-limit-segments.md) §Non-functional requires segments to consume modelled fields; the catch-all exists for keys nothing depends on.
- `doctor`'s `endpoint.shape_current` check warns on any `unknown_buckets` key outside its allowlists. A key a shipped segment reads should not be reported as an unrecognized forward-compat surprise.
- The other buckets get clamping and degrade-to-`None` from serde. Reading raw `serde_json::Value` in a render path bypasses all of it.
- We do not control this endpoint. It has now drifted once; the shape decision should say what happens next time rather than being re-litigated.

## Considered Options

- **Read `limits` from `unknown_buckets`** — no shape change; segment pattern-matches untyped JSON
- **Model `limits` as a typed field** — promote it out of the catch-all into `UsageApiResponse`
- **Do nothing and wait** — assume the named fields repopulate

## Decision Outcome

Chosen option: **model `limits` as a typed field**, adding `limits: Option<Vec<UsageLimit>>` to `UsageApiResponse` and `"limits"` to `KNOWN_BUCKETS`.

The field has to be threaded the whole way, not merely added at the parse boundary. Segments never see `UsageApiResponse`: it is copied into `EndpointUsage` by `into_endpoint_usage`, and `EndpointUsage` is what `ctx.usage()` hands them; `CachedData` and both its `From` impls carry it across the 180s disk cache; `build_endpoint_usage` mirrors it to rhai.

Threading it is not the risk it first appears to be. Every one of those sites is an exhaustive struct literal or destructuring with no `..`, so once `limits` is added to the three struct definitions the compiler refuses to build until each hop is wired. `KNOWN_BUCKETS` is pinned by `known_buckets_matches_usage_api_response_fields`, which asserts the parity at test time — though what actually stops the build there is the exhaustive `UsageApiResponse { … }` literal the test constructs. Adding it to only some of the three does not compile either: with `CachedData` left out, `From<CachedData> for UsageApiResponse` is missing a field. The work is mechanical and the compiler drives it.

What the compiler cannot catch is that promotion is lossy relative to the wire. Validation happens at the parse boundary, so `group`, `is_active`, `scope.surface`, and any unrecognized `kind` or `severity` are gone before anything downstream sees the data — a plugin reading `ctx.usage.data.limits[].kind` gets `"unknown"` on every render, cached or fresh, and the cache inherits that loss rather than adding to it. This is the cost of promotion: `unknown_buckets` was lossless precisely because it validated nothing. It is worth paying, but it should be a decision rather than a surprise. A cached response written before this change deserializes to `limits: None`, so the segment hides until the next refresh; that is expected.

Reading it from the catch-all would contradict the spec's own forward-compat requirement, leave `doctor` warning about a load-bearing key, and put untyped value-poking in a render path that every sibling segment reaches through a clamped type. Waiting is not viable: the fields have been null for an unknown period, and the data exists today in a form we can consume.

This also sets the general rule for future drift, which is the part worth keeping: **the catch-all is for keys nothing depends on. The moment a segment depends on a key, it is promoted into the typed model.** The catch-all remains the landing zone for genuinely unrecognized keys — the live response carries several — and is not a supported read path.

### Consequences

- Good, because ADR-0011's per-model rationale becomes true again rather than quietly stale.
- Good, because the segment inherits clamping and per-field degradation instead of hand-rolling validation at render time.
- Good, because `doctor` stops reporting a key the tool depends on as an unknown bucket.
- Good, because the promote-on-dependency rule gives the next drift a decided answer instead of a fresh argument.
- Bad, because a naive `Option<Vec<UsageLimit>>` fails the entire response parse on one malformed element, dropping the endpoint to the JSONL fallback rather than hiding one segment. A per-item-tolerant deserializer is mandatory; [data-fetching.md](../specs/data-fetching.md) carries its contract alongside the `UsageLimit` definition.
- Bad, because the documented shape is restated in several places that must be amended in step or they contradict this ADR: [data-fetching.md](../specs/data-fetching.md) (the `EndpointUsage` struct, the new `UsageLimit` definition, the forward-compat prose, and the cache-file sample), [doctor.md](../specs/doctor.md) (the shape-check row), and [plugin-api.md](../specs/plugin-api.md) (the `ctx.usage` sample, the reader block, and the claim that `unknown_buckets` holds only keys core segments ignore). The plugin surface forces a decision: `ctx.usage.data.unknown_buckets` is exposed to rhai, and `limits` is in that map today, so promoting it silently removes it from plugins that read it. `limits` is therefore mirrored onto the plugin surface as `ctx.usage.data.limits`, typed like the core field. The top-level key moves between two plugin-visible paths rather than disappearing — but the move is not lossless: `group`, `is_active`, and `scope.surface` are readable inside today's untyped array and become unreachable once the typed model drops them, because `unknown_buckets` does not extend into array elements. That loss is accepted; it is the same struct-change cost the promotion rule prices in everywhere else, and `is_active` is one the segment spec wants withdrawn.
- Neutral, because `seven_day_opus` / `seven_day_sonnet` / `seven_day_oauth_apps` stay in the struct as `Option`. They cost nothing while null, and removing them would break if the endpoint ever repopulates them.

### Confirmation

Confirmed when `rate_limit_7d_model` renders from the typed field and `linesmith doctor` no longer lists `limits` among forward-compat keys.

Promoting `limits` does not by itself make `endpoint.shape_current` quiet, and the §Consequences claim above is narrower than it looks. The 2026-08-05 response carries five root keys — `spend`, `amber_ladder`, `cinder_cove`, `nimbus_quill`, `member_dashboard_available` — that are in neither `KNOWN_BUCKETS` nor `RESEARCH_DOCUMENTED_BUCKETS`, the latter still holding only the five 2026-04-18 codenames. Removing `limits` from the WARN's input leaves those five as its entire content, which is the train-the-operator-to-ignore-it failure this ADR argues against. Recording the new capture in `docs/research/claude-data-files.md` and refreshing `RESEARCH_DOCUMENTED_BUCKETS` is a prerequisite for the confirmation above, tracked as `lsm-48ql`.

Revisit if the named per-model fields repopulate — at which point they and `limits` would be two sources for one fact, and this ADR should say which wins.

## Pros and Cons of the Options

### Read `limits` from `unknown_buckets`

- Good, because no shape change and no deserializer work.
- Bad, because it contradicts the forward-compat requirement that the catch-all is ignored by core segments.
- Bad, because `doctor` keeps warning about a key the tool now requires, training the operator to ignore that check.
- Bad, because validation moves into the render path, where the existing clamping and degradation rules do not apply.

### Model `limits` as a typed field

- Good, because it matches how every other consumed bucket is handled.
- Good, because malformed data is handled at the parse boundary rather than per-segment.
- Bad, because a naive `Vec` makes one bad element fail the whole response; the tolerant deserializer is mandatory, not optional polish.

### Do nothing and wait

- Good, because zero work if the fields return.
- Bad, because they have been null for an unknown period with no signal they will return.
- Bad, because it leaves ADR-0011's stated rationale factually wrong in the meantime.

## More Information

- Implementation contract: [rate-limit-segments.md](../specs/rate-limit-segments.md) §`rate_limit_7d_model`
- `UsageLimit`'s field set, the fields deliberately left unmodelled, and the tolerant deserializer: [data-fetching.md](../specs/data-fetching.md) §OAuth usage cache stack
- The matching rule this enables needs `ModelInfo.id`, added in [input-schema.md](../specs/input-schema.md) v0.3
- Tracked by `lsm-zgju`
- The live response also carries `spend`, `amber_ladder`, `cinder_cove`, `nimbus_quill`, and `member_dashboard_available` outside both allowlists. Those stay in the catch-all under the rule above — nothing depends on them — but they need recording as research baseline so `doctor` stays quiet; see §Confirmation and `lsm-48ql`.
