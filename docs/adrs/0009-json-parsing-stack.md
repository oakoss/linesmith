# Use serde_json with partial structs for JSON parsing; JSON for our own caches

- Status: accepted
- Date: 2026-04-18
- Deciders: Jace

## Context and Problem Statement

linesmith reads JSON from several sources per prompt: stdin payload (~10KB), `~/.claude/settings.json`, `~/.claude.json` (~125KB), JSONL transcripts (MB+), a live OAuth endpoint response (~1KB), and it writes its own cache file (~1KB, per 180s). Cold-start budget is <20ms and the stripped binary target is 3-5MB. What parsing library should we adopt, and what format should we write our own caches in?

## Decision Drivers

- Cold-start budget <20ms — parser init and parse cost both matter
- Binary size target 3-5MB stripped — SIMD parsers ship CPU-feature fallback code that adds 400-700KB
- Inspectability for cache debugging — users should be able to `cat ~/.cache/linesmith/usage.json` when something looks wrong
- OSS contributor friendliness — default patterns contributors recognize without reaching for unusual crates
- Schema-migration story when our own cache structs change between versions
- Correctness under forward-compat (Anthropic's OAuth endpoint ships unreleased fields under internal codenames)

## Considered Options

- **`serde_json`**: the ubiquitous baseline. Safe, serde-integrated, ~4 transitive deps
- **`simd-json`**: SIMD port of simdjson. 2-4× faster on medium-to-large documents; ~15 transitive deps; ~400-700KB binary cost
- **`sonic-rs`**: ByteDance-maintained SIMD parser. Best independent benchmarks; less mature (862⭐, no declared MSRV)
- **`json-rust`**: DOM-only, no serde integration. Last release 2024-04; de-facto legacy
- **Binary formats for our own caches**: `bincode`, `rkyv`, `postcard`, `rmp-serde`. Faster decode, worse inspectability, brittle schema migration

## Decision Outcome

Chosen option: **`serde_json` + partial-deserialization structs for all JSON parsing; pretty-printed JSON with a `schema_version` field for our own caches**, because (a) 7 of 7 Rust CLIs in our survey that parse JSON at all use `serde_json` (zero use simd-json or sonic-rs), (b) the `~/.claude.json` hot path wins more from a narrow partial struct than from swapping parsers (~80% work drop via `#[serde(other)]` vs ~2-3ms from SIMD), (c) our ~1KB cache is too small for binary formats to pay off their debuggability cost, and (d) `serde_json`'s transitive-dep count and binary impact stay well under targets.

### Consequences

- Good, because contributors see a conventional Rust stack (`serde`, `serde_json`, derive) with no surprises
- Good, because `#[serde(default)]` + `Option<T>` on every field of the OAuth response struct gives us forward-compat with Anthropic's codenamed buckets (`omelette_promotional`, `iguana_necktie`, etc.) for free
- Good, because pretty-printed JSON caches with a `schema_version` field let us treat schema mismatch as cache-miss without writing custom migration code
- Good, because inspectability makes debugging "why is my rate-limit segment wrong" a `cat` away, not a `hexdump`
- Good, because binary-size impact (~150-250KB over serde baseline) leaves headroom in the 3-5MB target
- Bad, because we give up the theoretical 2-4× parse speedup of SIMD parsers on the 125KB `~/.claude.json`
- Bad, because the partial-struct pattern requires matching Anthropic's JSON field names exactly (drift risk if upstream renames fields; mitigated by `#[serde(rename = "...")]`)
- Neutral, because promoting to `simd-json` later is a 1-line Cargo feature-flag change; no architectural lock-in

### Confirmation

Revisit if:

- Cold-start measurements show `~/.claude.json` parse consuming >5ms p50 after the partial-struct implementation lands — signal that parser swap could help
- Binary size exceeds 5MB despite release-profile tuning (LTO, strip, panic=abort, codegen-units=1)
- `serde_json` unmaintenance or major-version break introduces migration friction

The measured threshold for promoting `simd-json` from opt-in (behind `--features fast-json`) to default: >2ms p50 improvement on the 125KB workload after partial-parse is already in place.

## Pros and Cons of the Options

### `serde_json`

- Good: ubiquitous, safe, serde-integrated, ~4 transitive deps, MSRV 1.68
- Good: 9 of 11 surveyed Rust CLIs use it; contributor-familiar
- Good: `#[serde(default)]` + partial structs handle forward-compat cleanly
- Bad: measurably slower than SIMD parsers on large inputs (but our inputs are small)

### `simd-json`

- Good: 2-4× faster than serde_json on MB-scale inputs
- Good: Rust port of simdjson (well-tested C++ parent)
- Bad: ~400-700KB binary cost from shipping SSE4.2/AVX2/NEON/fallback code paths
- Bad: destructive parse API (`&mut [u8]`) — harder to compose than serde_json's `from_str`/`from_reader`
- Bad: 0 adoption across 11 surveyed Rust CLIs
- Bad: requires SIMD-capable hardware detection at runtime (adds cold-start cost)

### `sonic-rs`

- Good: best independent benchmarks (edges out simd-json)
- Good: serde-compatible
- Bad: less mature (862⭐, no declared MSRV)
- Bad: ByteDance-maintained — additional supply-chain consideration
- Bad: same binary-size and workload-size caveats as simd-json

### `json-rust`

- Good: smaller crate size (~106KB)
- Bad: no serde integration — incompatible with the rest of our type definitions
- Bad: last release 2024-04, effectively unmaintained
- Bad: DOM-only API forces manual tree walking

### Binary formats for caches (`bincode`, `rkyv`, `postcard`, `rmp-serde`)

- Good: 3-50× faster decode depending on format
- Bad: not inspectable — debugging stale cache requires a parser roundtrip
- Bad: brittle schema migration — adding a non-optional field breaks existing caches
- Bad: `bincode` upstream repo archived 2025-08-15; maintenance story unclear
- Bad: ~1KB cache size means absolute time savings are negligible (5-15 µs per read)

## More Information

- Primary source: [`docs/research/json-parsing-stack.md`](../research/json-parsing-stack.md) — full survey, benchmark numbers, and 11-CLI comparison
- Supporting: [`docs/research/data-fetching-strategy.md`](../research/data-fetching-strategy.md) — per-source cost matrix that justifies the partial-struct emphasis
- Supporting: [`docs/research/claude-data-files.md`](../research/claude-data-files.md) — confirms `~/.claude.json` ~125KB size and schema
- Supporting: [`docs/research/ccometixline-rust-patterns.md`](../research/ccometixline-rust-patterns.md) — ccstatusline peer project also uses `serde_json`
- Related: [ADR-0010](0010-data-fetching-architecture.md) — data-fetching architecture depends on this parsing strategy
- Open follow-up: `cargo bench` on the 125KB workload once scaffold compiles; revisit parser choice if numbers diverge from estimates
- sonic-rs self-reported benchmarks: <https://github.com/cloudwego/sonic-rs>
- Cross-format serialization shootout: <https://github.com/djkoloski/rust_serialization_benchmark>
