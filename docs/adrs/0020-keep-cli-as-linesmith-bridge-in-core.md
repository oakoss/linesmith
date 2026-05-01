# Keep CLI crate as `linesmith` and the plugin bridge in `linesmith-core`

- Status: accepted
- Date: 2026-05-01
- Deciders: Jace Babin

## Context and Problem Statement

[ADR-0018](0018-cargo-workspace-core-plugin-cli.md) decided to split the codebase into `linesmith-core`, `linesmith-plugin`, and `linesmith-cli`, with `ctx_mirror.rs` (the `DataContext → rhai::Map` adapter) placed in `linesmith-cli` so the integration layer would live where both `core` and `plugin` are already in scope and `core` would not have to depend on `rhai`. [ADR-0019](0019-publish-linesmith-core-as-scaffolding-from-v0-1.md) supersedes ADR-0018's publish posture, but it did not revisit the architectural partition.

When the partition was actually implemented (bead `lsm-v2o3`), two of ADR-0018's structural choices changed:

1. **The CLI crate was not renamed to `linesmith-cli`.** The binary already ships on crates.io as `linesmith` (`cargo install linesmith`); renaming would have broken the documented install path. ADR-0019 mentions this in passing ("binary packaged at `crates/linesmith` to keep the published crate name `linesmith`") but does not record it as a discrete decision.
2. **`ctx_mirror.rs` and the `RhaiSegment` / `output` bridge layer stayed in `linesmith-core`, not `linesmith-cli`.** Moving them to the cli would have cascaded `crates/linesmith-core/src/segments/builder.rs` (which constructs `RhaiSegment` instances inline as part of the segment-build flow) and `crates/linesmith-core/src/plugins/segment.rs` (the Segment-trait adapter, which depends on `core::segments::{RenderContext, RenderResult, Segment, SegmentError}`) out of `core` too. Per the user's design call during the implementation: "cli should use, not define" — extended to "core should hold the bridge."

The driving question: **Where should `ctx_mirror.rs` and the consumer-side bridge live, and what should the CLI crate be called?**

## Decision Drivers

- The original concern that pushed `ctx_mirror` to cli (avoid forcing `core` to depend on `rhai`) is moot: `linesmith-core` already depends on `rhai` because the segment system, theme, and credentials layers all use rhai-adjacent types. The "heavy dep that pure-data consumers don't want" framing in ADR-0018 §47 turned out to be incorrect — there is no realistic pure-data consumer of `linesmith-core` that wouldn't pull `rhai` transitively.
- `core::segments::builder` constructs `RhaiSegment` instances directly during the segment build flow (`builder.rs:423` calls `RhaiSegment::from_compiled`). Putting `RhaiSegment` in `cli` would force the builder to either: (a) move out of `core` too, breaking the "core owns the segment system" invariant, (b) emit plugin-id placeholders that the cli resolves separately, requiring a new two-pass build flow, or (c) take a callback for plugin construction, complicating the public API. None of these is cheaper than leaving the bridge in `core`.
- "CLI should use, not define" — the cli crate should consume primitives from `core` and `plugin`, not define new ones. `RhaiSegment` is a primitive (it implements `Segment`); it belongs with the other Segment implementations in `core`.
- The CLI crate name on crates.io is a published-API decision, not an internal layout decision. Once published as `linesmith`, the install path is sticky.
- `linesmith-plugin`'s public surface stays free of linesmith domain types regardless of where `ctx_mirror` lives — the plugin crate exposes `rhai::Map` + `Vec<String>`, and the consumer (`core` or `cli`) wraps those into domain types. Nothing about ADR-0018's "publishable plugin host" pitch depends on `ctx_mirror`'s home.

## Considered Options

- **Move `ctx_mirror` + `RhaiSegment` + `output` to `linesmith-cli` (per ADR-0018), and rename the cli crate to `linesmith-cli`.** Forces the segment builder out of `core` or splits the build flow; breaks the published install path.
- **Move `ctx_mirror` + `RhaiSegment` + `output` to `linesmith-cli` (per ADR-0018), but keep the cli crate name as `linesmith`.** Same builder-cascade problem as above; minus the install-path breakage.
- **Keep `ctx_mirror` + `RhaiSegment` + `output` in `linesmith-core` as the bridge layer, and keep the cli crate name as `linesmith`** (chosen). Plugin crate is rhai-pure; core owns the bridge; cli is a thin consumer.

## Decision Outcome

Chosen option: **Keep the bridge layer in `linesmith-core`, and keep the cli crate name as `linesmith`**, because the original justification for moving `ctx_mirror` to cli (avoiding a `rhai` dep on `core`) does not match the actual dep graph; the segment builder requires `RhaiSegment` to live alongside the other `Segment` implementations; and the cli crate name is already a sticky published-API surface that doesn't carry an architectural argument for renaming.

### Crate partition (refined)

- **`linesmith-core`** — `input/`, `data_context/`, `segments/`, `theme/`, `layout/`, `config/`, `presets/`, `runtime/`, plus the **plugin bridge layer**: `plugins/ctx_mirror.rs` (the `DataContext → rhai::Map` adapter), `plugins/segment.rs` (the `RhaiSegment` adapter implementing the `Segment` trait), `plugins/output.rs` (decoder for plugin return shapes into `RenderedSegment`). Re-exports `linesmith-plugin`'s public types so existing call sites resolve through `crate::plugins::*`. Workspace-internal in v0.1; published as scaffolding per ADR-0019.
- **`linesmith-plugin`** — `engine.rs` (rhai construction + caps + host abort markers), `errors.rs` (`PluginError` variants for compile/runtime/timeout/resource-exceeded/malformed-return/malformed-data-deps/unknown-data-dep/id-collision), `discovery.rs` (dir walker + `// @data_deps` header parser), `header.rs` (validates dep names against `KNOWN_DEPS`), `registry.rs` (compile + index + collision detect). **No linesmith domain types in the public surface**; declared deps cross the boundary as `Vec<String>` and the consumer maps strings back to its own dep enum. Workspace-internal in v0.1; published as scaffolding per ADR-0019.
- **`linesmith`** — `driver.rs`, `cli.rs`, `main.rs`, `doctor/`. Produces the `linesmith` binary. Crate name retained for crates.io install-path stability. Never publishes as a library.

### Dependency graph (refined)

```text
linesmith ──→ linesmith-core ──→ linesmith-plugin
```

Linear chain: `linesmith` (cli) depends on `linesmith-core` only; `linesmith-core` depends on `linesmith-plugin`. The cli does not have a direct dep on `linesmith-plugin`. Plugin types reach the cli through `linesmith-core::plugins::*` re-exports.

### Consequences

- Good, because `linesmith-plugin`'s public surface is rhai-pure: `rhai::Map`, `rhai::Engine`, `Vec<String>` for declared deps, `PluginError` / `CompiledPlugin` / `PluginRegistry` typed against rhai primitives only. ADR-0018's "publishable plugin host" pitch survives unchanged.
- Good, because `core::segments::builder` keeps constructing `RhaiSegment` inline; the segment build flow stays a single pass with no plugin-id placeholders or callback indirection.
- Good, because the cli crate stays small: `driver.rs`, `cli.rs`, `main.rs`, `doctor/` — no plugin-bridge logic to maintain.
- Good, because `linesmith` stays as the published crate name; `cargo install linesmith` keeps working; existing READMEs and ADRs that mention the install path don't rot.
- Good, because `linesmith-core::plugins::build_engine` can be a thin wrapper that installs the `LINESMITH_LOG`-respecting warn emitter before delegating to `linesmith-plugin::build_engine` — every entry point that goes through `linesmith-core` (CLI, library `run` family, doctor, direct API consumers) gets the bridge automatically.
- Bad, because `linesmith-core` carries the rhai dep, but this was already true under any partition that keeps the segment system in `core` (the trait the bridge implements).
- Bad, because the bridge layer in `core` is the seam where plugin-context conversion bugs surface; debugging requires understanding both the plugin-side rhai surface and the core-side `DataContext` types. Mitigated by the bridge being small (~3 files) and well-tested.
- Neutral, because the cli does not gain a direct dep on `linesmith-plugin`, but it can still reach plugin types via `linesmith-core::plugins::*` re-exports.

### Confirmation

The refinements are correct if, six months after landing:

- `linesmith-plugin`'s public surface contains no `linesmith-core` types (verified by `cargo check -p linesmith-plugin` standalone — fails the build if violated).
- A third-party Rust statusline maintainer (real, not hypothetical) can embed `linesmith-plugin` without pulling `linesmith-core`. ADR-0018's confirmation criterion #3 is the test.
- The cli crate name stability is confirmed by zero install-path-related issues against the `linesmith` crate on crates.io after the v0.2 release.

Revisit if:

- A future feature requires the cli to define a primitive that core can't host (would re-open the "cli should use, not define" call).
- `linesmith-plugin` finds a real third-party consumer who needs `ctx_mirror`-shaped functionality, which would suggest the bridge should also be published as a separate `linesmith-plugin-ctx` crate.

## More Information

- Implementation that drove this refinement: bead `lsm-v2o3` (extract `linesmith-plugin`).
- Related ADRs:
  - [ADR-0018](0018-cargo-workspace-core-plugin-cli.md) — original workspace-split decision; this ADR supersedes its `Crate partition` and `Dependency graph` sections.
  - [ADR-0019](0019-publish-linesmith-core-as-scaffolding-from-v0-1.md) — publish posture; unchanged by this ADR.
- Code-level details captured in commit `291cd09` (`refactor(plugins): extract linesmith-plugin from linesmith-core`).
