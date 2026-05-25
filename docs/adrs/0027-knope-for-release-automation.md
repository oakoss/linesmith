# Use Knope for release automation, replacing release-plz

- Status: accepted
- Date: 2026-05-23
- Deciders: Jace Babin

## Context and Problem Statement

[ADR-0019](0019-publish-linesmith-core-as-scaffolding-from-v0-1.md) shipped `linesmith-core` and `linesmith-plugin` to crates.io as scaffolding alongside the `linesmith` binary, and chose `release-plz` to handle per-package versioning + CHANGELOG generation + crates.io publish. The v0.1.x cycle ran cleanly. The v0.2.0 major-bump cycle did not.

The failure mode: release-plz computed `linesmith-core` should bump 0.1.3 → 0.2.0 (a breaking change behind a `feat(core)!:` commit), but it did NOT update `linesmith`'s in-manifest pin `linesmith-core = { version = "0.1.3", path = "../linesmith-core" }`. cargo's caret-version semantics then rejected the resolution: `linesmith-core ^0.1.3` excludes `0.2.0` (different major series under `0.x` rules). The release PR shipped a workspace that didn't compile. Workarounds explored:

1. **`release-plz update_dependencies = true`** — release-plz refreshes `Cargo.lock` but not pinned version strings in sibling `Cargo.toml` files. Documented limitation; no config knob exposes the cross-manifest rewrite ([release-plz/release-plz#2199](https://github.com/release-plz/release-plz/issues/2199)).
2. **`version.workspace = true`** at the root + per-crate inheritance — forces every crate to share one version, defeating the per-package versioning ADR-0019 mandates. Restores release-plz's normal-case behavior but at the cost of dragging `linesmith-plugin` (still at 0.1.x) to 0.2.0 unnecessarily.
3. **Manual unblock** — bump pin strings by hand, tag, push. Worked for v0.2.0 (commit 7219fa6) but doesn't scale.

Knope's `[packages.<name>] versioned_files = [{ path, dependency = "<crate-name>" }, ...]` pattern is built around exactly this case: each consumer manifest explicitly declares which package's version it pins, and Knope rewrites that string in place on bump. The `convert-to-monorepo` recipe is canonical for Rust workspaces with internal deps.

How should we automate releases?

## Decision Drivers

- Cross-file dep-pin updates by name (the release-plz gap)
- Per-package independent versioning preserved (ADR-0019 invariant)
- Auto-release on push to main (no manual intervention at major boundaries)
- Changeset-file workflow matches contributor familiarity from TS ecosystems
- Workflow files are visible + auditable in the repo (no opaque external bot dependency unless the user opts in)
- Reuse existing supply-chain posture: OIDC trusted publishing, pinned action SHAs, GitHub App for repo writes (no long-lived PATs)
- cargo-dist continues to own binary builds + Homebrew formula + attestations (ADR-0007 stays load-bearing)

## Considered Options

- **Stay on release-plz** with `version.workspace = true` for lockstep bumps
- **Stay on release-plz** with continued manual unblocks at major boundaries
- **release-plz fork** carrying a local patch for cross-manifest dep-pin updates
- **`cargo-smart-release`** — `cargo-release` fork from gitoxide; conventional-commits-aware
- **`cargo-release` directly** — no PR-based workflow, dispatch-only
- **Knope with `bot.releases = true`** — install the Knope GitHub App to handle release-PR creation server-side
- **Knope with `prepare-release.yml` workflow** (chosen) — explicit workflows in the repo, no external app

## Decision Outcome

Chosen option: **Knope with `prepare-release.yml` + `release.yml` workflows (no Knope GitHub App)**, because:

1. Knope's `dependency = "<crate>"` field is the only surveyed tool that handles cross-manifest dep-pin rewrites natively. The convert-to-monorepo recipe lays out the exact `[packages.<name>]` shape this workspace needs.
2. The workflow-based pattern (rather than `bot.releases = true`) keeps all release automation in `.github/workflows/`, visible to contributors and auditable in PR diffs. It also reuses the existing `BOT_CLIENT_ID` / `BOT_PRIVATE_KEY` GitHub App secrets that release-plz already used — no new secret to provision, no new external dependency to install.
3. The changeset-file workflow (`knope document-change` → `.changeset/*.md`) is a familiar surface from TanStack/Changesets-style TS projects and complements conventional commits rather than replacing them.
4. cargo-dist's `release.yml` only needs two hand-edits to coexist: tag pattern updated to `linesmith/v*` (Knope's monorepo tag format), and the host job's `gh release create` replaced with `gh release upload` + `gh release edit --draft=false --latest` so cargo-dist attaches binaries to the GitHub Release Knope already created. The Homebrew formula push, attestations, and cross-compile matrix stay untouched.

### Implementation

Tooling:

- `.mise.toml` adds `"cargo:knope" = "0.22.4"` for local use (`knope document-change`, `knope prepare-release --dry-run`).
- `.github/workflows/knope-prepare.yml` (new) fires on push to `main`, mints a GitHub App installation token, runs `knope prepare-release --verbose`. Force-pushes to a `release` branch and opens/updates the release PR.
- `.github/workflows/knope-release.yml` (new) fires when the `release` PR merges. Runs `knope release --verbose` (tags each package and creates per-package GitHub Releases with CHANGELOG-derived notes), then publishes the workspace to crates.io via OIDC trusted publishing. Uses `cargo release publish --workspace --execute --no-confirm` rather than bare `cargo publish --workspace`: cargo-release skips crates already at their target version on crates.io, which is load-bearing for single-package release cycles (a `feat(plugins)` cycle only bumps `linesmith-plugin`, but `cargo publish --workspace` would still attempt all three crates and fail on the unchanged `linesmith` and `linesmith-core`).
- `.github/workflows/release-plz.yml` retired.
- `release-plz.toml` and `cliff.toml` retired (Knope's `PrepareRelease` step generates changelogs internally).
- `cog.toml` drops the unused `[changelog]` section (cog only validates commit-message format now).

Cargo manifests:

- `crates/linesmith-core/Cargo.toml`, `crates/linesmith-plugin/Cargo.toml`, `crates/linesmith/Cargo.toml` keep their explicit `version = "X.Y.Z"` fields (per ADR-0019).
- Internal dep pins stay per-crate; Knope rewrites the version string in place via the `dependency` field in `knope.toml`'s `versioned_files`.

`knope.toml`:

- Three `[packages.*]` blocks (one per crate) with `versioned_files`, `changelog`, `scopes`.
- `scopes` restrict commit routing: `core` → `linesmith-core`, `plugins` → `linesmith-plugin`, user-visible binary scopes (`cli`, `tui`, `segments`, `themes`, `config`, `doctor`) → `linesmith`.
- Two Knope workflows per the preview-release-PR recipe's "split the rest of the steps into a second workflow" guidance:
  - `prepare-release`: single `PrepareRelease` step with `allow_empty = true`. Stages version bumps + CHANGELOG entries.
  - `publish-release-pr`: `git switch -c release` → `git commit` → `git push --force --set-upstream origin release` → `CreatePullRequest`.

  `knope-prepare.yml` invokes both back-to-back with a `git diff --quiet HEAD` gate between, so a no-op push to main (no releasable conventional commits since the last per-package tag) lands as a green run without invoking `CreatePullRequest` — which would otherwise error with "no commits between main and release" on the no-op case. Real Knope errors (malformed config, GitHub App token rejection, push blocked by branch protection) still surface as job failures. The bare `--force` on push (not `--force-with-lease`) is intentional: every CI run is a fresh checkout with no local ref to lease against, and the `release` branch is workflow-owned (no concurrent human pushes to lose).

- `release` workflow: `Release` step creates per-package tags `<pkg>/v<version>` and GitHub Releases.
- `document-change` workflow: `CreateChangeFile` for contributors who run `knope document-change` locally.

cargo-dist (`.github/workflows/release.yml`, hand-edits protected by `allow-dirty = ["ci"]`):

- Tag filter: `linesmith/v[0-9]+.[0-9]+.[0-9]+*` (the new Knope format) plus `v[0-9]+.[0-9]+.[0-9]+*` for backward compatibility with the v0.2.0 manual unblock. Drop the bare pattern once Knope owns every release.
- `compute-tag` step's dry-run branch synthesizes `linesmith/v${version}` instead of bare `v${version}` so dispatch/PR runs exercise the same code path as real releases.
- Host job's "Create GitHub Release" step replaced with:

  ```bash
  if gh release view "$TAG" >/dev/null 2>&1; then
    gh release upload "$TAG" artifacts/*
    gh release edit "$TAG" --draft=false $PRERELEASE_FLAG $LATEST_FLAG
  else
    gh release create "$TAG" ... artifacts/*  # legacy `v*` path
  fi
  ```

Doctor (`crates/linesmith/src/doctor/snapshot.rs`):

- `parse_three_part_version` strips per-package tag prefixes (`<pkg>/v<ver>` and legacy `<pkg>-v<ver>`) before parsing. GitHub's `/releases/latest` endpoint can return any package's tag in a multi-package release; without the strip, doctor's update probe emitted a `ParseError` WARN on every release since v0.1.2. The fix covers both the Knope and release-plz tag formats.

### Consequences

- Good, because the v0.2.0 cross-manifest dep-pin failure mode is structurally impossible under Knope's `dependency` field — every consumer's pin string is enumerated in `knope.toml` and rewritten on bump.
- Good, because per-package versioning is preserved without `version.workspace = true`, honoring ADR-0019's per-crate scaffolding posture.
- Good, because the changeset-file workflow (`knope document-change`) gives contributors an explicit way to declare release intent for changes whose conventional-commit shape doesn't fully capture impact (e.g. a `refactor:` commit that's actually breaking).
- Good, because cargo-dist's release.yml hand-edit count goes up by exactly 1 net (the `host` job's release-create→upload swap; the tag-pattern edit was always going to happen on any per-package format migration).
- Good, because the doctor self-update probe now correctly handles all four tag formats users can encounter via `/releases/latest`: bare `v*`, `linesmith-v*` (legacy release-plz), `linesmith/v*` (new Knope), and per-library variants of both.
- Bad, because Knope's `Release` step creates a GitHub Release for every bumped package, including `linesmith-core` and `linesmith-plugin`. Those are scaffolding crates (per ADR-0019) and their releases have no binaries to ship — they exist as CHANGELOG-bearing records on the repo's Releases page. Browsers landing on the Releases page see a longer list than under release-plz.
- Bad, because `/releases/latest` semantics depend on GitHub's "most recently published non-prerelease release marked as latest" rule. The cargo-dist host job's `gh release edit --latest` on the `linesmith/v*` release makes it deterministically latest after the binary build completes, but during the ~10-minute build window, whichever library release Knope created last is "latest". Doctor's probe handles this gracefully (the prefix-strip lets `linesmith-core/v0.2.0` parse cleanly), but the user-facing "latest" briefly points at a library release.
- Bad, because the migration requires one-time creation of `<pkg>/v<ver>` tags pointing at the equivalent release-plz-era tags so Knope's first `PrepareRelease` run can compute "what's changed since the last release" correctly. Documented in `docs/ops/release-runbook.md` §Knope migration tags.
- Neutral, because Knope's `prepare-release` workflow re-runs on every push to `main` (force-pushing the `release` branch each time). Contributors landing back-to-back commits see the release PR update on each push — same UX as release-plz.

### Confirmation

The decision is correct if, after the next routine release (the first one driven by Knope end-to-end):

1. The release PR opens automatically on push to main with all 3 packages' version bumps + CHANGELOG entries.
2. Internal dep pins (`linesmith-core = { version = "...", ... }` in `linesmith/Cargo.toml`, etc.) update in lockstep with each bump.
3. Merging the release PR tags each bumped package as `<pkg>/v<ver>`, creates per-package GitHub Releases, publishes to crates.io in leaf-first order, and triggers cargo-dist to upload binaries + installers to the existing `linesmith/v<ver>` release.
4. `linesmith doctor` on a stale install reports `Newer version available: linesmith/vX.Y.Z` cleanly (no `ParseError` WARN).

Revisit if:

- Knope drops the `dependency` field from `versioned_files` (would force a return to release-plz-style hand workarounds or a fork).
- cargo-dist gains native per-package tag awareness that obviates the `gh release upload`+`edit` hand-edits.
- The Knope `Release` step starts creating draft releases by default for non-asset packages — would let us drop the workaround commentary about user-visible "latest" race during the cargo-dist build window.
- The Knope GitHub App matures enough that `bot.releases = true` becomes lower-friction than the in-repo workflows (less surface to maintain, but more opaque).

## Pros and Cons of the Options

### Stay on release-plz with `version.workspace = true`

- Good, because zero migration cost
- Good, because release-plz's normal-case behavior works (no cross-manifest dep-pin bug fires when all crates share one version)
- Bad, because every minor bump drags every crate, even non-changing ones. `linesmith-plugin` at 0.1.x gets force-bumped to 0.2.0 alongside `linesmith-core`'s breaking change, advertising a SemVer change that didn't actually happen.
- Bad, because crates.io users importing `linesmith-plugin` directly (against ADR-0019's advice) see version churn that doesn't reflect actual code changes.
- Bad, because reverses ADR-0019's per-package versioning posture.

### Stay on release-plz with manual unblocks

- Good, because zero migration cost
- Bad, because every major-bump cycle requires manual cross-manifest fix-up (a release operation that should be automated). v0.2.0 already demonstrated the cost.
- Bad, because the manual path is error-prone — easy to forget a pin update in one of the 3 manifests.

### release-plz fork

- Good, because preserves all existing workflow shape
- Bad, because forking + maintaining a release-plz patch is its own perpetual cost
- Bad, because the upstream maintainer has stated the cross-manifest rewrite is out of scope ([discussion thread linked in lsm-tn1 review-cycle notes])
- Bad, because forks accumulate drift vs upstream; eventually a release-plz feature we want lands and we have to rebase

### cargo-smart-release / cargo-release

- Good, because both are mature Rust tools with active maintenance
- Bad, because neither has the PR-based "release preview" UX that release-plz and Knope share — they're dispatch-driven, which means contributors review the release diff post-merge rather than pre-merge.
- Bad, because neither handles cross-manifest dep-pin updates by package name; same gap as release-plz.
- Bad, because changeset-file workflow is not part of either tool's surface.

### Knope with `bot.releases = true`

- Good, because release-PR creation happens server-side; no `knope-prepare.yml` workflow needed (drops one file)
- Good, because no `PAT` / GitHub App token to manage for prepare-release (the bot handles its own auth)
- Bad, because requires installing the `knope-dev/knope-bot` GitHub App on `oakoss/linesmith` — adds an external dependency and a third-party app with `contents: write` scope
- Bad, because release-PR creation logic lives outside the repo (in the Knope bot's hosted code), reducing auditability
- Bad, because the bot's behavior is versioned independently of `knope.toml` — a bot update could change PR shape without a corresponding repo commit

### Knope with `prepare-release.yml` (chosen)

- Good, because all release logic is in `.github/workflows/` — visible, diffed in PRs, auditable
- Good, because reuses the existing `BOT_CLIENT_ID` / `BOT_PRIVATE_KEY` GitHub App from the release-plz setup — no new secrets to provision
- Good, because cargo-dist hand-edit count goes up by 1, not 5+ (which the `bot.releases` route would require for asset-upload-to-draft-release coordination)
- Bad, because two workflow files (knope-prepare + knope-release) to maintain instead of one
- Bad, because the prepare-release workflow re-fires on its own `chore: prepare release` commit unless the `if:` guard catches it — typo-fragile (same caveat the recipe explicitly calls out)

## More Information

- **Knope monorepo recipe** (canonical reference for the `dependency` field + `<pkg>/v<ver>` tag format): <https://github.com/knope-dev/knope/blob/main/docs/src/content/docs/recipes/convert-to-monorepo.md>
- **Knope preview-release-PR recipe** (basis for the `knope-prepare.yml` + `knope-release.yml` shape): <https://github.com/knope-dev/knope/blob/main/docs/src/content/docs/recipes/1-preview-releases-with-pull-requests.md>
- **Knope's own `knope.toml`** (per-package config as seen in production): <https://github.com/knope-dev/knope/blob/main/knope.toml>
- **release-plz cross-manifest dep-pin gap**: surfaced during v0.2.0 unblock (commit c1feace's `ci(release-plz): force workspace-version lockstep for major bumps` — the workaround we're now reverting)
- Related ADRs:
  - [ADR-0007](0007-cargo-dist-distribution.md) — cargo-dist stays the binary-build specialist; this ADR doesn't change the distribution mechanism, only the version-bump conductor
  - [ADR-0017](0017-release-workflow-pr-validation.md) — path-filtered `pull_request:` trigger on `release.yml` stays as-is. The path filter excludes Knope's `knope.toml` / `knope-prepare.yml` / `knope-release.yml` (and previously excluded `release-plz.toml` / `release-plz.yml`) for the same reason: triggering the 7-target build matrix on those files would be false coverage (release.yml's matrix doesn't execute Knope's code paths).
  - [ADR-0019](0019-publish-linesmith-core-as-scaffolding-from-v0-1.md) — per-package versioning + linesmith-core as published scaffolding. This ADR preserves both invariants; Knope's `dependency` field is what makes them simultaneously achievable.
- Driving research: `docs/research/release-workflow-patterns.md` gets a Knope-vs-release-plz comparison section under this ADR.
- Driving spec: `docs/specs/release-process.md` is rewritten to describe the Knope-based contract; `docs/ops/release-runbook.md` is rewritten to describe the day-to-day flow including changeset authoring.
- Migration follow-ups (filed separately): one-time creation of `<pkg>/v<ver>` tags pointing at the existing release-plz-era tags; deprecation of `linesmith-v*` / `linesmith-core-v*` / `linesmith-plugin-v*` tag patterns once `/releases/latest` consistently returns the new format.
