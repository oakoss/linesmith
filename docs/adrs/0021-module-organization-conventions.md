# Adopt module organization conventions for the linesmith workspace

- Status: proposed
- Date: 2026-05-02
- Deciders: Jace Babin

## Context and Problem Statement

The linesmith workspace has accumulated a handful of files in the 1300–2400 LOC range (`segments/builder.rs` 2397, `input.rs` 2280, `data_context/cascade.rs` 1820, `credentials.rs` 1490, `jsonl.rs` 1459, `segments/mod.rs` 1320, `segments/git_branch.rs` 1355) and a 5h/7d rate-limit segment family with high duplication. Some of this is real domain complexity, some is unfinished organization. Without a written convention, every PR re-litigates "split or not?" and patterns drift across crates after the workspace split in [ADR-0018](0018-cargo-workspace-core-plugin-cli.md).

How should we organize Rust modules in this workspace so that files stay AI-navigable, tests stay close to code, and reviewers don't have to re-derive structure conventions per PR?

## Decision Drivers

- AI-navigability: a contributor (human or agent) should be able to open one file and understand its surface without scrolling 1500+ lines.
- Testability: tests should live next to the code they verify; the rule should not actively make this harder.
- Idiomatic Rust: where the community has converged, follow it. Where it hasn't, pick one form and stop relitigating.
- Mechanical enforceability: prefer rules that `cargo`/`clippy` can check over rules that exist only in prose.
- Compatibility with the published-as-scaffolding posture from [ADR-0019](0019-publish-linesmith-core-as-scaffolding-from-v0-1.md) and consume-from-origin from [ADR-0020](0020-keep-cli-as-linesmith-bridge-in-core.md): internal reorganization is free; the cross-crate seam is not.

## Considered Options

- **Codify a 10-rule convention set** with mechanical enforcement via `[workspace.lints]` where possible (this ADR).
- **Codify a larger 12+ rule style guide** including doc-comment policy, feature-flag naming, newtype patterns, etc.
- **Stay ad-hoc**, decide per-PR.

## Decision Outcome

Chosen option: **codify a 10-rule convention set, lint-enforced where possible**, because the smaller surface stays maintainable, the mechanical-enforcement bias keeps prose from rotting, and the rules are scoped to organization (not language style) so they don't overlap with existing ADRs (parsing in [0014](0014-best-effort-parse-with-segment-isolation.md), workspace split in [0018](0018-cargo-workspace-core-plugin-cli.md), plugins in [0004](0004-rhai-for-plugins.md)).

The rules below were validated against canonical Rust projects (tokio, axum, hyper, sqlx, reqwest, clap, ripgrep, starship); the research is summarized in [More Information](#more-information). Where the community is divided, this ADR says so instead of claiming consensus.

### The 10 rules

**1. File-size smell trigger: ~400 LOC of code (excluding tests).** Files crossing this threshold should be reviewed for whether they cover ≥3 concepts; if so, graduate per rule 3. The number is a project preference, not a Rust convention — mature crates routinely exceed it (axum's `extract/path/mod.rs` is 923 LOC, hyper's `error.rs` is 621 LOC). Treat ~400 LOC as the "look at this file" threshold, not a hard cap.

**2. Tests live inline by default; sibling `tests.rs` files only after folder graduation.** The default is `#[cfg(test)] mod tests { ... }` at the bottom of the implementation file — the idiomatic Rust pattern documented in The Rust Book. The only sibling form supported is `mod tests;` pointing at a `tests.rs` next to `mod.rs` inside a folder module. Flat `.rs` files with overgrown test blocks stay inline until rule 3 graduates them to folders; the test extraction comes free with the folder graduation. Do **not** use `#[path = "<file>_test.rs"] mod tests;` — that form is non-idiomatic, fights rust-analyzer navigation, and is mechanically forbidden by Confirmation criterion 3.

**3. Folder trigger: ≥3 distinct concepts in one module.** Graduate a `.rs` file into a folder with `mod.rs` when it accumulates 3 or more sibling-level concepts (e.g., the rate-limit family has formatters, config, window-resolution, error-rendering, and per-window segment structs — that's a folder). LOC alone is not a trigger; concept count is. Single big-but-focused files stay flat. For the linesmith codebase specifically, rule 3 drives most of the planned splits; rule 1's LOC trigger is an early-warning flag that prompts a rule-3 evaluation, not a split mandate of its own.

**4. Folder shape: `mod.rs` is mostly re-exports + module declarations, with one explicit exception for registry/dispatcher modules.** Inside a folder, `mod.rs` should read like a table of contents: `pub use` re-exports of the folder's public surface, `mod foo;` declarations, and at most a small dispatcher when the folder _is_ a registry (e.g., a segment-name → constructor lookup). Implementation lives in named feature/concept files inside the folder. The dispatcher exception covers cases like starship's `modules/mod.rs` (centralized routing) and any future segment-registry pattern in linesmith.

**5. Errors live in `error.rs` at the appropriate level — crate root for cross-cutting errors, module root when scoped to a single module's domain.** Pick the level deliberately; don't scatter error types across many files within one module. Both `error.rs` (file form) and `error/` (folder form, when the error type splits across multiple files) are acceptable. Use **singular `error.rs`**, not `errors.rs`, to match ecosystem convention (hyper, reqwest, sqlx, fd, bat all use singular). The existing `crates/linesmith-core/src/data_context/errors.rs` is module-scoped and well-placed; it migrates to singular `error.rs` as part of the rule-5 sweep. Research showed crate-wide errors dominate in mature Rust crates (6 of 9 sampled use a single root `error.rs`); per-module errors remain correct when a module's domain stands on its own (data fetching is exactly that case in linesmith-core).

**6. Use the most restrictive visibility that compiles. Enforced via lints, not prose.** Default to private. Escalate one step at a time: `pub(super)` (folder-only), `pub(crate)` (crate-only), `pub` (consumer-facing). `pub(crate)` is the workhorse; `pub(super)` is available but rarely needed. Mechanical enforcement via `[workspace.lints]` (see Companion conventions below):

```toml
[workspace.lints.rust]
unreachable_pub = "warn"

[workspace.lints.clippy]
redundant_pub_crate = "warn"
```

`unreachable_pub` flags items declared `pub` that aren't reachable from the crate root — i.e., should be `pub(crate)`. `clippy::redundant_pub_crate` flags `pub(crate)` items inside a private module — i.e., should be plain `pub`. Together they pin the visibility ladder without hand-policing.

**7. No `lib/` directory. `util(s)` allowed only when narrow + concept-named, never as a `misc.rs` dumping ground.** A top-level `lib/` directory conflicts conceptually with `lib.rs` and is rare in the Rust ecosystem. A `util` module is acceptable when its scope is narrow and its purpose is named (e.g., tokio's `util/` is async-runtime helpers, not generic utilities). What's prohibited: a `utils.rs` or `utils/` whose contents are a grab-bag with no concept binding them. When ≥2 callers need a helper, name the module after what the helper _does_ (`time`, `json`, `path`), not after its grab-bag status.

**8. `lib.rs` discipline: module declarations + re-exports + crate-level docs only.** Domain types live in concept-named modules, not in `lib.rs`. `lib.rs` may carry: crate-root rustdoc (`//!` comments), `#![...]` crate-level attributes (lint config, edition signals), `mod foo;` declarations, `pub use` re-exports defining the crate's public surface, and at most one or two convenience entry points (e.g., a `run()` orchestrator). Anything else — domain types, helpers, large impl blocks — moves to a sibling file.

**9. `mod.rs` over `<name>.rs` for folder modules.** Rust 2018+ supports both `parser/mod.rs` and `parser.rs + parser/`. The community is split (cargo#14120 is an open debate). linesmith uses `mod.rs` consistently and will continue to. Mixing both forms within a crate is the real anti-pattern; pick one and hold the line. New folder modules use `mod.rs`. Existing `<name>.rs + <name>/` patterns (none currently) would migrate to `mod.rs` form.

**10. `#[non_exhaustive]` selectively, not universally.** Apply to: error enums **whose variants you expect to grow** (so adding a variant isn't a major bump), public config structs with all-pub fields (so adding a knob isn't a major bump), and other enums you expect to grow. **Do not apply** to closed-set types whose membership is fixed by domain (e.g., a 2-variant `Direction { Left, Right }`) — universal application forces downstream wildcard arms even where they're semantically wrong. RFC 2008 frames the attribute as selective. Mature crates whose error types stay open via a different mechanism (e.g., wrapping `Box<dyn Error>` as a source field, like reqwest's `Error` and hyper's `Error`) don't need `#[non_exhaustive]`. linesmith's existing `UsageError`, `GitError`, `CredentialError`, and friends expose variants directly and are growth-prone — they correctly carry `#[non_exhaustive]` today and continue to.

### Companion conventions (not numbered rules; mechanical or scoped)

- **Workspace lints config.** All member crates inherit lint config via `[workspace.lints]` in the root `Cargo.toml` and `[lints] workspace = true` in each member crate's `Cargo.toml`. Stable since Rust 1.74; linesmith's MSRV is 1.85, so available immediately. This is where rule 6 is actually enforced. Recommended baseline:

  ```toml
  [workspace.lints.rust]
  unsafe_code = "forbid"
  unreachable_pub = "warn"

  [workspace.lints.clippy]
  redundant_pub_crate = "warn"
  ```

  **Migration shape, not greenfield.** Three per-crate configs already exist (`linesmith-core/Cargo.toml`, `linesmith-plugin/Cargo.toml`, `linesmith/Cargo.toml`) with `unsafe_code = "forbid"`. `linesmith-plugin` additionally has a `[lints.clippy] disallowed_methods = "allow"` _override_ that opts out of the workspace-level consume-from-origin enforcement (which lives in workspace `clippy.toml` per [ADR-0020](0020-keep-cli-as-linesmith-bridge-in-core.md); the plugin crate is the home of the disallowed method, so the lint would fire on its own implementation). Migration consolidates the shared rules into the workspace block while preserving the plugin-crate clippy override.

  Adding `missing_docs = "warn"` on published crates (`linesmith-core`, `linesmith-plugin`) is encouraged but out of scope for this ADR.

- **Test fixture sharing.** Unit-test fixtures used by multiple `mod tests` blocks in the same module live in `<module>/test_support.rs` with `pub(super)` visibility, gated `#[cfg(test)]`. Cross-module fixtures (within one crate) live in `crates/<crate>/src/test_support/`, also `#[cfg(test)]`-gated. Integration-test fixtures live in `tests/common/mod.rs` (folder form, **not** `tests/common.rs` — the file form gets compiled as its own integration-test binary, a known footgun). Cross-crate fixtures escalate to a `linesmith-test-support` dev-only crate listed under `[dev-dependencies]`.

- **No prelude modules.** linesmith does not export a `prelude::*`. Modern community guidance (corrode.dev, tokio's removal of theirs) is "don't add a prelude unless every consumer truly needs the same 5+ traits in scope." linesmith's import surface stays small enough that explicit imports cost less than a prelude would.

- **`examples/` directory.** Encouraged for the `linesmith` binary crate (runnable demos via `cargo run --example foo`). Not required for `linesmith-core` / `linesmith-plugin` (library-only, no end-user-facing demos to ship). Out of scope for this ADR; tracked separately if/when it lands.

### Consequences

- Good, because the file-size smell trigger + folder rule prevent the 2000+ LOC files from re-emerging without explicit review.
- Good, because lint-enforced visibility (rule 6) catches violations in CI rather than at review time, where reviewers might miss them.
- Good, because the consume-from-origin posture from [ADR-0020](0020-keep-cli-as-linesmith-bridge-in-core.md) plus the `lib.rs` discipline (rule 8) plus folder-as-façade (rule 4) compose into a single navigation contract: open `lib.rs` → see the public surface → drill into named modules.
- Good, because the inline-tests default (rule 2) preserves the Rust idiom and matches every contributor's muscle memory; the sibling-when-large escape hatch covers the overgrown cases.
- Bad, because the conventions cost some upfront refactoring work (the rate-limit family folder, the `data_context/{cascade,credentials,jsonl}` folders, the `segments/builder/` split).
- Bad, because rule 5 requires renaming `errors.rs` → `error.rs` in two existing files (`crates/linesmith-core/src/data_context/errors.rs`, `crates/linesmith-plugin/src/errors.rs`). Cosmetic but touches every import-site. The existing per-module placement of `data_context/errors.rs` stays correct — only the filename changes.
- Bad, because rule 2 (inline tests by default) collides with five existing files where the test block already dominates: `data_context/credentials.rs` (95% tests), `layout.rs` (86%), `segments/builder.rs` (75%), `data_context/cascade.rs` (75%), `input.rs` (49%). Plus four more in the 300–900 LOC test range. Each of these is a flat `.rs` file today; under rule 2, sibling-file test promotion requires graduating to a folder first (`foo.rs` → `foo/mod.rs` + `foo/tests.rs`). The follow-up bead therefore drives folder graduations, not a mechanical test-only sweep — tracked as bead 3 with explicit scope, overlapping with beads 4–6 where those folders are already planned.
- Neutral, because the `mod.rs` choice (rule 9) imports an open community debate. Future contributors may push for `<name>.rs` form; this ADR pins the choice and supersedes such proposals unless rewritten.
- Neutral, because the `#[non_exhaustive]` rule (rule 10) means some currently-marked types may be candidates for un-marking, and some unmarked types may be candidates for marking. Tracked as follow-up bead 7.

### Confirmation

This ADR is confirmed when:

1. The lint config from the workspace lints section lands in the root `Cargo.toml` and CI is green.
2. At least one folder graduation lands following rule 4 shape (the `segments/rate_limit/` folder is the first planned application — see follow-up beads).
3. `grep -rE '#\[path = "[^"]+_test\.rs"\]' crates/` returns no matches — rule 2's no-`#[path]` constraint is mechanically verifiable in CI.

Revisit if:

- Files routinely exceed ~600 LOC code-side after legitimate splits — rule 1's number is too low.
- The `mod.rs` choice causes friction with rust-analyzer or new contributors — revisit rule 9 in a new ADR.
- Rule 5 produces type-circularity headaches in `linesmith-plugin` or `linesmith-core` — a per-module error type is needed after all.

## Pros and Cons of the Options

### Codify a 10-rule convention set (chosen)

- Good, because the ruleset is small enough to remember and review against.
- Good, because rules 6 and 9 collapse into lint config rather than prose to police.
- Good, because the rules are validated against real Rust projects, not invented from scratch.
- Bad, because some conventions (rule 1's 400 LOC number, rule 9's `mod.rs` choice) are project preferences that won't carry community weight in arguments with new contributors.

### Codify a larger 12+ rule style guide (rejected)

- Good, because more rules cover more ambiguity upfront.
- Bad, because the community feedback was that 12+ rules drift toward style-guide territory, where rules get ignored or partially applied.
- Bad, because doc-comment policy, feature-flag naming, and newtype patterns each deserve their own ADR rather than getting buried in an organization ADR.

### Stay ad-hoc, decide per-PR (rejected)

- Good, because no upfront cost.
- Bad, because the existing 1500-2400 LOC files are the consequence of staying ad-hoc; the cost shows up at every review.
- Bad, because the workspace split in [ADR-0018](0018-cargo-workspace-core-plugin-cli.md) created seams where conventions diverge across crates without a written rule.

## More Information

### Validated against (research, 2026-05-02)

The 10 rules were each checked against canonical Rust projects via web research. Summary of findings that drove the final shape:

- **Tests inline (rule 2):** The Rust Book documents `#[cfg(test)] mod tests { ... }` as the convention. Zero of {tokio, axum, hyper, sqlx, reqwest, starship, ripgrep, clap, anyhow, thiserror} use the `#[path]` sibling-file form. Inline by default; promote to sibling without `#[path]` when needed.
- **Errors at appropriate level (rule 5):** {hyper, reqwest, sqlx-postgres, sqlx-core, fd, bat} use a single crate-root `error.rs`; {clap (folder form), bevy_ecs (folder form), axum (per-extractor rejection types)} use module/feature-scoped errors. Both patterns are valid; rule 5 picks the level deliberately rather than imposing one.
- **`pub(super)` rare in practice (rule 6):** Effective Rust Item 22 ranks `pub(crate)` above `pub(super)`. Mature crates use `pub(crate)` as the workhorse.
- **`utils/` contested, not absent (rule 7):** {tokio, tower, cargo, axum, reqwest, starship, bat} have some form of `util(s)`. {ripgrep, fd, hyper, sqlx, bevy_ecs, anyhow, thiserror, clap} don't. Roughly even split. The quality bar is "narrow + named-by-purpose," not absence.
- **`lib.rs` discipline (rule 8):** Strongest consensus of all 10 rules. {tokio (656 LOC, ~60% docs), serde, clap (42 LOC), reqwest (374 LOC), axum} all keep `lib.rs` as module-decls + re-exports + docs.
- **`mod.rs` vs `<name>.rs` (rule 9):** Split. cargo#14120 is an open debate. linesmith already uses `mod.rs`; pinning the choice avoids drift.
- **`#[non_exhaustive]` selective (rule 10):** RFC 2008 frames it as selective. {reqwest, hyper, sqlx-postgres} don't mark their error types; clap does. Apply to types expected to grow.

### References

- [The Rust Book §11.3 — Test Organization](https://doc.rust-lang.org/book/ch11-03-test-organization.html)
- [The Rust Reference — Visibility and Privacy](https://doc.rust-lang.org/reference/visibility-and-privacy.html)
- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- [Effective Rust Item 22 — Minimize Visibility](https://effective-rust.com/visibility.html)
- [RFC 2008 — non_exhaustive](https://rust-lang.github.io/rfcs/2008-non-exhaustive.html)
- [Cargo: workspace.lints (1.74+)](https://doc.rust-lang.org/cargo/reference/workspaces.html#the-lints-table)
- [rust-lang/cargo#14120 — module file naming debate](https://github.com/rust-lang/cargo/issues/14120)
- [corrode.dev — Don't Use Preludes And Globs](https://corrode.dev/blog/dont-use-preludes-and-globs/)

### Related ADRs

- [ADR-0014](0014-best-effort-parse-with-segment-isolation.md) — parse-and-degrade convention; complements rule 5 (crate-wide errors).
- [ADR-0018](0018-cargo-workspace-core-plugin-cli.md) — workspace split that created cross-crate seams.
- [ADR-0019](0019-publish-linesmith-core-as-scaffolding-from-v0-1.md) — published-as-scaffolding posture; internal reorg is free.
- [ADR-0020](0020-keep-cli-as-linesmith-bridge-in-core.md) — consume-from-origin; complements rule 4 (folder façade).

### Follow-up work (not part of this ADR)

Filed as beads, all gated on this ADR's acceptance:

1. **lsm-v1ec — Workspace lints config (migration).** Consolidate per-crate `unsafe_code = "forbid"` into `[workspace.lints]`; add `unreachable_pub = "warn"` and `clippy::redundant_pub_crate = "warn"`. Preserve `linesmith-plugin`'s `disallowed_methods = "allow"` per-crate override.
2. **lsm-zou5 — Rule 5 file renames + placement check.** `data_context/errors.rs` → `data_context/error.rs`; `linesmith-plugin/src/errors.rs` → `linesmith-plugin/src/error.rs`. Update import sites. Verify `linesmith-plugin/src/error.rs`'s crate-root placement still fits rule 5 (cross-cutting vs module-scoped); bead closes either confirming the placement or filing a follow-up if it should change.
3. **lsm-8reb — Folder graduations for `input.rs` and `layout.rs`** to extract their inline test blocks per rule 2. The other inline-test-heavy files (`credentials.rs`, `cascade.rs`, `segments/builder.rs`, `jsonl.rs`) are covered by beads 4–6 — their test extraction comes free with their planned folder splits. Mid-tier files (`config.rs`, `git_branch.rs`, `segments/mod.rs`) stay inline unless their concept count earns a folder under rule 3.
4. **lsm-oszc — `segments/rate_limit/` folder graduation** + DRY candidates 1+4 from the 2026-05-02 architecture review (`UsageWindow` seam).
5. **lsm-iash — `segments/builder/` split** (the 2397 LOC monster).
6. **lsm-p50v — `data_context/{cascade,credentials,jsonl}/` folder graduations**, tightly coupled to `lsm-0a4e` env-cascade refactor.
7. **lsm-q16r — `#[non_exhaustive]` audit pass** against rule 10. Goal: confirm growth-prone enums carry the attribute; un-mark closed-set enums where it currently appears unnecessarily.
