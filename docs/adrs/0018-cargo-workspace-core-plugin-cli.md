## Split linesmith into a Cargo workspace with `core`, `plugin`, and `cli` crates

- Status: superseded by [ADR-0019](0019-publish-linesmith-core-as-scaffolding-from-v0-1.md)
- Date: 2026-04-30
- Deciders: Jace Babin

## Context and Problem Statement

The `linesmith` crate has grown to ~46k lines across `data_context/`, `segments/`, `theme/`, `layout/`, `plugins/`, `input/`, `config/`, `presets/`, `doctor/`, and `driver.rs`. The `doctor` slice (epic `lsm-l35`) repeatedly surfaced friction at module boundaries: doctor snapshot helpers had to mirror runtime predicates because the runtime versions were buried in `driver.rs` (`load_plugins`, `xdg_segments_dir`, `user_themes_dir`, `build_theme_registry`, `load_config`); the credentials cascade reads `std::env::var_os` directly, forcing doctor to take three `EnvVarState` parameters to compensate; and the rhai plugin engine — the project's main differentiator vs. ccstatusline / CCometixLine / claude-powerline — is locked inside the binary crate where no other Rust statusline tool can embed it. How should the codebase be partitioned so the segment / theme / data layer can be tested without process-env mutation, the plugin host can eventually be published for third-party consumers, and the CLI surface stays separate from reusable primitives?

## Decision Drivers

- Doctor's `lsm-efmu` parity rule keeps producing follow-up beads (`lsm-2it2`, `lsm-8sus`, `lsm-x9vo`) because the runtime predicates it must mirror are private to `driver.rs`; the cleanest fix is to lift them into a shared crate doctor can call directly
- Plugin host (rhai engine + caps + dep-gated context + error classification) is the project's largest unique architectural contribution; no other Rust statusline ships a real plugin API (see [`docs/research/competitor-landscape.md`](../research/competitor-landscape.md) §Conclusions point 1), so a publishable `linesmith-plugin` crate is the natural v0.2+ play
- Compile-time isolation: editing `driver.rs` should not invalidate cache for the segment-rendering layer; the current monolith rebuilds everything
- Test surface: pure modules (data_context, segments, theme, layout) want hermetic testability without the cli's process-env reads or ratatui dep dragging into the same compile unit
- Future MSRV flexibility: `linesmith-plugin` could ship with a looser MSRV than `linesmith-cli` (which is pinned by ratatui 0.30, dialoguer)
- Per-process execution model (ADR-0012) and `panic = "abort"` (ADR-0007) are unaffected by the split; the workspace ships one binary regardless of crate count

## Considered Options

- Stay single-crate; address friction with `pub(crate)` reshuffles
- Split now into `core` + `cli` only; keep `plugins/` inside `core`
- Split now into `core` + `plugin` + `cli`, all workspace-internal indefinitely
- Split now into `core` + `plugin` + `cli`, publish all three with v0.1
- **Split now into `core` + `plugin` + `cli`, all workspace-internal in v0.1; publish `linesmith-plugin` after v0.2 once the API has earned stability under linesmith's own use** (chosen)

## Decision Outcome

Chosen option: **Split now into `core` + `plugin` + `cli`, all workspace-internal in v0.1; publish `linesmith-plugin` after v0.2**, because the architectural payoff (doctor parity, compile isolation, testability) is independent of the publish decision, and decoupling the two lets each be made on its own timeline. Workspace-internal crates carry no SemVer cost — refactor freely until the public API has earned stability through real consumption (linesmith's own binary in v0.1, then a window of post-v0.2 production validation before publishing the plugin host). The split also creates a natural home for the `runtime` / `bootstrap` predicates currently buried in `driver.rs`, which would otherwise stay private and continue forcing the doctor parity-rule maintenance.

### Crate partition

- **`linesmith-core`** — `input/`, `data_context/`, `segments/`, `theme/`, `layout/`, `config/`, `presets/`, the `runtime` predicates lifted out of `driver.rs`. Workspace-internal in v0.1; potential publish post-v1.0 if a third-party consumer asks.
- **`linesmith-plugin`** — `engine.rs` (rhai wrapping + caps + host abort markers), `errors.rs` (Compile / Runtime / Timeout / ResourceExceeded / MalformedReturn / MalformedDataDeps / UnknownDataDep / IdCollision), `discovery.rs` (dir walker + `// @data_deps` header parser), `registry.rs` (compile + index + collision detect). Public API takes `rhai::Map` constructed by the consumer; no linesmith domain types in the surface. Workspace-internal in v0.1 + v0.2; publish to crates.io after v0.2.
- **`linesmith-cli`** — `driver.rs`, `cli.rs`, `main.rs`, `doctor/`, and `ctx_mirror.rs` (the linesmith-specific `DataContext → rhai::Map` adapter that wires `core` to `plugin`). Produces the `linesmith` binary. Never publishes as a library.

### Dependency graph

```text
linesmith-cli ──→ linesmith-core
       │              ↑
       └────→ linesmith-plugin
                   (no dep on core)
```

`ctx_mirror` lives in `cli` because it is the integration layer that knows about both `core::DataContext` and `plugin::PluginEngine`. Putting it in `core` would force `core` to depend on `rhai` (heavy dep that pure-data consumers don't want); putting it in `plugin` would couple `plugin` to linesmith's domain types (defeats the publishable-host pitch).

### Consequences

- Good, because doctor snapshot helpers can call the `linesmith-core` runtime predicates directly instead of mirroring them; closes the long tail of parity-rule beads (`lsm-efmu` lineage)
- Good, because the credentials cascade and XDG-derivation helpers move to a single home with explicit env-snapshot inputs, eliminating direct `std::env::var_os` reads from the testable layer
- Good, because `linesmith-plugin` becomes a publishable rhai plugin host primitive — the project's main architectural differentiator becomes available to other Rust statusline tools post-v0.2
- Good, because compile-time isolation: editing `driver.rs` no longer rebuilds the segment-rendering layer; CI matrix parallelizes per crate
- Good, because each crate can declare its own MSRV when published (`plugin` likely looser than `cli`)
- Bad, because workspace setup adds CI complexity (clippy + test per crate); cargo-dist multi-crate config; per-crate Cargo.toml inheritance discipline
- Bad, because the v0.1 timeline absorbs ~3 weeks of focused refactor work before user-visible features land
- Bad, because `ctx_mirror` straddles the `core` / `plugin` seam; bugs in plugin-context conversion now span two crates' boundaries
- Neutral, because `linesmith` (the binary name) stays unchanged; only the crate-on-crates.io picture changes

### Confirmation

The split is correct if, six months after landing:

- doctor's parity-rule follow-up beads (currently `lsm-2it2`, `lsm-8sus`, `lsm-x9vo`) have closed without spawning new mirrors
- cold `cargo build` in `linesmith-core` finishes meaningfully faster than the pre-split monolith. Baseline measured 2026-04-30 on the single-crate shape after `cargo clean`: 17.95s wall (91.4s user @ 571% cpu, debug profile, M-series Mac). Re-measure under the same conditions after the split lands; "meaningfully faster" = 25%+ wall-clock reduction since most touches will be to one crate
- A third-party Rust statusline maintainer (real, not hypothetical) has filed an issue or PR against `linesmith-plugin` after v0.2 publishes

Revisit if:

- The cross-crate refactor surface area becomes the dominant friction (every change touches three Cargo.toml files), suggesting the boundaries are wrong
- `linesmith-plugin` finds zero external consumers in the year after v0.2 publishes; the publish decision should reverse to "yank, keep workspace-internal"

## Pros and Cons of the Options

### Stay single-crate

- Good, because zero refactor cost and zero CI complexity
- Bad, because the doctor parity-rule maintenance keeps growing; every new doctor check that mirrors a runtime predicate is a new drift surface
- Bad, because the plugin host stays bundled with linesmith; other Rust statusline tools can't embed it without copying the rhai engine + caps + error-classification work themselves
- Bad, because the testability gap surfaced in `lsm-x9vo` (no transport injection on `snapshot_update_probe`) keeps reappearing in sister functions (`resolve_credentials`, `JsonlTailer`)

### Split into `core` + `cli` only; keep `plugins/` inside `core`

- Good, because half the architectural win at half the migration cost
- Bad, because `core` would need a heavy `rhai` dep that pure-data consumers don't want (any future Rust tool wanting just the segment engine pays the rhai cost)
- Bad, because `linesmith-plugin` never becomes a standalone primitive; the differentiator stays buried

### Split now into three crates, all workspace-internal indefinitely

- Good, because lowest commitment — refactor freely forever
- Bad, because forecloses on the plugin-host-as-published-primitive value entirely; the differentiator never reaches third-party tools

### Split now into three crates, publish all three with v0.1

- Good, because earliest possible third-party adoption signal
- Bad, because SemVer-locks `linesmith-core` while linesmith is still iterating on segment / data_context shapes; every internal refactor becomes a breaking-change discussion
- Bad, because doubles the publish cost without proportional benefit (most third-party demand will land on `linesmith-plugin`, not `linesmith-core`)

### Split now into three crates, publish `linesmith-plugin` post-v0.2 (chosen)

- Good, because the architectural split is independent of the publish decision; the workspace yields the testability + isolation wins immediately
- Good, because the plugin API earns stability through linesmith's own use before being committed to externally
- Good, because the publish posture stays reversible: if no third-party demand materializes by mid-v0.2, the plugin crate stays workspace-internal at zero additional cost
- Bad, because three workspace crates is genuine CI complexity; the team / solo maintainer has to keep three Cargo.toml files coherent

## More Information

- Architecture review session that produced this split: design-tree grilling on 2026-04-30 covering crate boundaries, ctx_mirror placement, publish timing, MSRV flexibility, cargo-dist compatibility
- Related ADRs:
  - [ADR-0003](0003-segment-widget-system.md) — the segment / widget system that lives in `linesmith-core`
  - [ADR-0004](0004-rhai-for-plugins.md) — rhai engine choice that becomes `linesmith-plugin`
  - [ADR-0007](0007-cargo-dist-distribution.md) — cargo-dist still ships one binary; multi-crate workspace is supported
  - [ADR-0010](0010-data-fetching-architecture.md) — data_context architecture that lives in `linesmith-core`
  - [ADR-0012](0012-per-process-execution.md) — per-process model unaffected by the split
- Driving deepening candidates from the architecture sweep:
  - Candidate #1: credentials env-snapshot (lifts `FileCascadeEnv` to `pub`)
  - Candidate #2: collapse the XDG cascade duplicated across `cache::default_root`, `driver::xdg_segments_dir`, `driver::user_themes_dir`, `doctor::xdg_subdir`
  - Candidate #4: extract the `runtime` / `bootstrap` predicates currently buried in `driver.rs`
- Open follow-ups (file as beads after the ADR lands):
  - Reserve `linesmith-core`, `linesmith-plugin`, `linesmith-cli` on crates.io with stub `0.0.0` placeholder releases (defends against name-squatting; `[package.publish] = false` keeps them local until v0.2)
  - Decide whether `ctx_mirror` lives at `linesmith-cli/src/ctx_mirror.rs` or under `linesmith-cli/src/plugins/ctx_mirror.rs`; the second namespacing signals "cli's plugin-integration concerns" more clearly
