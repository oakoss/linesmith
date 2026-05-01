# Publish `linesmith-core` to crates.io as scaffolding from v0.1

- Status: accepted
- Date: 2026-05-01
- Deciders: Jace Babin

## Context and Problem Statement

[ADR-0018](0018-cargo-workspace-core-plugin-cli.md) decided to split the codebase into `linesmith-core`, `linesmith-plugin`, and `linesmith-cli` (the binary, packaged at `crates/linesmith` to keep the published crate name `linesmith`). Its §"Decision Outcome" stated `linesmith-core` would be **workspace-internal in v0.1, with potential publish post-v1.0**, and §"Open follow-ups" planned to "Reserve `linesmith-core`, `linesmith-plugin`, `linesmith-cli` on crates.io with stub `0.0.0` placeholder releases ... `[package.publish] = false` keeps them local until v0.2."

Implementation of the workspace split (`lsm-1nh0`) discovered that this plan is **incompatible with cargo's publish workflow**. `linesmith` (the binary crate) was already published at v0.1.1 on crates.io, and the established install path (`cargo install linesmith`) is documented in the README and used by current consumers. Codex review of the workspace split commit caught the conflict as a P1 release blocker: `cargo publish -p linesmith` fails because `linesmith-core` is path-only with no version AND marked `publish = false`.

The driving question: **Can `linesmith` continue publishing to crates.io if `linesmith-core` stays workspace-internal?**

Empirical answer: **No.** Verified on cargo 1.94.1 in this workspace.

The path-only-dep failure mode (the original conflict the ADR resolves):

```text
$ cargo package -p linesmith --allow-dirty
error: failed to verify manifest at .../crates/linesmith/Cargo.toml
Caused by:
  all dependencies must have a version requirement specified when packaging.
  dependency `linesmith-core` does not specify a version
```

The "obvious fix" of adding `version = "0.1.1"` to the path dep without removing `publish = false` from `linesmith-core` runs the registry-resolution check next:

```text
$ cargo package -p linesmith --allow-dirty
error: failed to prepare local package for uploading
Caused by:
  no matching package named `linesmith-core` found
  location searched: crates.io index
  required by package `linesmith v0.1.1`
```

Both errors fire at manifest validation, before verification or upload. No flag (`--no-verify`, `--allow-dirty`, `--dry-run`) bypasses either. `[patch.crates-io]` is dev-only per cargo docs ("you cannot use this feature to tell Cargo how to find local unpublished crates"). `cargo vendor` doesn't bundle source into published `.crate` tarballs. No mainline cargo feature lets a published crate carry a `publish = false` transitive dep. Cargo 1.90's `cargo publish --workspace` enables atomic publishing of interdependent crates but every published crate still must satisfy the version-requirement rule.

Survey of comparable Rust workspaces — `serde`, `tokio`, `ratatui` — confirms the universal pattern: any `publish = false` member is a leaf (xtask, fuzz, benches, examples, integration test harnesses), never a library that a published binary depends on. `ripgrep` and `clap` go further and publish every workspace member (so the question doesn't even arise). The remaining "single-binary CLI" projects worth comparing — `starship`, `bat`, `eza` — are not workspaces at all (single-package crates), so they exemplify a different distribution model. No mainstream Rust project surveyed ships a published binary that depends on a `publish = false` library crate. The cargo error enforces the only viable design.

## Decision Drivers

- `cargo install linesmith` must keep working — current users and the README depend on this path
- Cargo workspace constraint: a published binary can't depend on a `publish = false` library crate via path-only
- ADR-0018's architectural payoff (compile isolation, doctor parity, future plugin host extraction) is independent of the publish decision and should not be sacrificed
- Solo-maintainer SemVer pressure should be minimized — `linesmith-core` is internal scaffolding the maintainer reshapes during v0.x development, not a stable consumer API
- Precedent matters: the `serde_derive` / `tokio-macros` / `clap_derive` pattern is well-understood in the Rust ecosystem and exactly fits this situation

## Considered Options

- Drop crates.io publishing for `linesmith` entirely (distribute via cargo-dist + Homebrew + GitHub Releases only)
- Revert the workspace split; defer until v0.2 when more architectural payoff justifies whatever publish reorganization happens then
- Bundle `linesmith-core` source into `linesmith` at publish time via `cargo vendor` or a custom build script
- **Publish `linesmith-core` to crates.io alongside `linesmith` as workspace-internal scaffolding** (chosen)

## Decision Outcome

Chosen option: **Publish `linesmith-core` to crates.io alongside `linesmith` as workspace-internal scaffolding**, because (a) it is empirically the only viable option that preserves both `cargo install linesmith` and the workspace split, and (b) it follows the established `serde_derive` / `tokio-macros` / `clap_derive` precedent for "helper crate that lives on crates.io to satisfy cargo's transitive-dep rule, not to advertise a stable consumer API."

### Implementation

- `linesmith-core` removes `publish = false`; description signals scaffolding intent: _"Internal core engine for linesmith. No SemVer guarantee for direct dependents — depend on the `linesmith` binary or accept breakage between minor versions."_
- Root `Cargo.toml` declares `linesmith-core = { path = "crates/linesmith-core", version = "0.1.1" }` in `[workspace.dependencies]` so cli inherits the shape via `linesmith-core.workspace = true`
- `linesmith-core` stays at 0.x indefinitely — the universally-recognized "may break between minor versions" signal — until linesmith itself reaches v1.0
- Releases use cargo 1.90+ `cargo publish --workspace` (or release-plz's dependency-ordered publishing) to push both crates atomically

### Consequences

- Good, because `cargo install linesmith` keeps working unchanged
- Good, because the workspace split lands without further blockers
- Good, because the scaffolding pattern is well-precedented; existing tooling (docs.rs, lib.rs, crates.io browsers) handles the "internal helper crate" idiom gracefully
- Good, because release-plz publishes per package; expected behavior is leaf-first dependency-ordered publishing without additional configuration. First-release verification is tracked in `lsm-8tam`
- Bad, because `linesmith-core` accumulates a public crates.io footprint earlier than ADR-0018 envisioned — a third-party who imports it directly may file issues expecting SemVer stability the description disclaims
- Bad, because every minor version bump publishes both crates even when only one changed (mitigated: linesmith-core's API churns frequently during v0.x, so the bumps are usually warranted)
- Neutral, because `linesmith-plugin` (when extracted in `lsm-v2o3`) faces the same constraint: it'll need to publish from its first release rather than staying internal until v0.2 as ADR-0018 §"Open follow-ups" suggested. That trajectory needs its own ADR amending ADR-0018's `linesmith-plugin` posture, separate from this one

### Confirmation

The decision is correct if, six months after landing:

- `cargo install linesmith` continues to install successfully across releases
- No third-party crate has filed an issue depending on `linesmith-core` directly (the description disclaimer is doing its job)
- The release pipeline publishes both crates atomically without manual intervention

Revisit if:

- A third-party Rust consumer files an issue requesting `linesmith-core` API stability — that's the signal to either harden the API for v1.0 or rename to clarify it's not a public consumer surface (e.g., `linesmith-internal`)
- Cargo gains a "private workspace dep on published crate" feature in some future release, removing the constraint that forced this decision

## Pros and Cons of the Options

### Drop crates.io publishing for `linesmith`

- Good, because `linesmith-core` stays workspace-internal exactly as ADR-0018 planned
- Good, because no SemVer obligation on internal scaffolding
- Bad, because `cargo install linesmith` stops working for new versions — a breaking change for current users
- Bad, because README + docs + community expectations all need updating
- Bad, because Rust's standard CLI install path (`cargo install`) is the most discoverable distribution channel

### Revert the workspace split

- Good, because the publish situation reverts to single-crate simplicity
- Good, because no SemVer pressure on internal modules
- Bad, because the architectural payoff (compile isolation, doctor parity guarantees, plugin host extraction prep) is lost
- Bad, because the work in `lsm-vsab`, `lsm-e4m1`, and `lsm-1nh0` becomes a wasted detour — and the same publish question reappears whenever the split is retried

### Bundle source via cargo vendor or build script

- Good, because `linesmith-core` could theoretically stay private
- Bad, because no mainline tooling supports this — would require custom maintenance burden
- Bad, because no Rust workspace surveyed uses this approach; we'd be inventing a non-idiomatic distribution model
- Bad, because debug experience suffers (stack traces point at vendored copies, source-server discovery breaks)

### Publish `linesmith-core` as scaffolding (chosen)

- Good, because empirically the only viable option per cargo's constraints
- Good, because well-precedented: `serde_derive` (~53M downloads/month per crates.io as of 2026-05-01), `tokio-macros`, `clap_derive`, and `pin-project-internal` all ship to crates.io as helper crates whose stability is governed by their parent crate. Their disclaimer is implicit (the convention is "do not depend on directly"); `linesmith-core` strengthens it with an explicit description-level "no SemVer guarantee" warning
- Good, because `cargo install linesmith` and `cargo install --path crates/linesmith` both keep working
- Good, because the workspace split is preserved end-to-end
- Bad, because `linesmith-core` is publicly listed on crates.io earlier than the maintainer wants, even though the description disclaims SemVer stability
- Bad, because users skimming crates.io may briefly conflate the two crates before reading the description

## More Information

- Supersedes [ADR-0018](0018-cargo-workspace-core-plugin-cli.md) per the strict immutability rule in `AGENTS.md`. The publish-posture change is the load-bearing reversal; ADR-0018's underlying architectural decisions (3-crate split, dependency graph, `ctx_mirror` placement, MSRV flexibility) carry forward into this ADR's implementation guidance and remain authoritative even though ADR-0018's status now reads "superseded".
- Related ADRs:
  - [ADR-0007](0007-cargo-dist-distribution.md) — cargo-dist still ships one binary; this ADR doesn't change the distribution channel for binary releases
  - [ADR-0017](0017-release-workflow-pr-validation.md) — release-plz handles per-package publishing; verifying multi-crate ordering on the first release is tracked in `lsm-8tam`
- Empirical reproduction (cargo 1.94.1, this workspace, 2026-05-01): `cargo package -p linesmith --allow-dirty` fails as quoted above — both with the original path-only dep and after the obvious "just add a version" fix that leaves `linesmith-core` at `publish = false`. `cargo package --workspace --allow-dirty` succeeds for both crates only after threading `version = "0.1.1"` into the path dep AND dropping `publish = false`. Note: `cargo package` is local-only — it builds and verifies the `.crate` tarball without uploading to crates.io. Actual publishing waits for the next release-plz run.
- Cargo documentation cited:
  - [Specifying Dependencies — Multiple locations](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#multiple-locations)
  - [The Manifest Format — `publish` field](https://doc.rust-lang.org/cargo/reference/manifest.html#the-publish-field)
  - [Overriding Dependencies — Working with `[patch]`](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html)
- Relevant follow-up beads:
  - `lsm-8tam` — Verify release-plz publishes `linesmith-core` before `linesmith` on the next release
  - `lsm-v2o3` (open from ADR-0018 epic) — `linesmith-plugin` extraction will face the same publish constraint and inherit this ADR's resolution
