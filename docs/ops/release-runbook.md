# Release Runbook

Step-by-step playbook for cutting a linesmith release. Assumes the prereqs
in [`docs/specs/release-process.md`](../specs/release-process.md) are already
set up (GitHub App on `oakoss/linesmith` + `oakoss/homebrew-tap`, org secrets
`BOT_CLIENT_ID` / `BOT_PRIVATE_KEY`, crates.io trusted publishing configured
for each of the three crates with `knope-release.yml` as the workflow and
no environment requirement).

See the spec for the full contract (target matrix, supply-chain posture,
semver rules, edge cases). This file is the "what to type, in order."

## Who cuts releases

Any maintainer with push access to `main` and the ability to merge PRs into
`oakoss/linesmith`. No crates.io credentials needed: the release pipeline
uses OIDC trusted publishing. No Homebrew credentials needed: the workflow
mints a GitHub App installation token on the fly.

## Normal release flow

Every release lands by merging the Knope release PR. `knope-release.yml` (`prepare` job)
opens / force-updates the PR automatically on each non-`release` PR
merge to `main` that contains a releasable change (any `feat` / `fix` /
`perf` matching a package's claimed scopes, or a non-empty changeset
file under `.changeset/`). PR merges without releasable changes land
as green no-op runs via the `git diff --quiet HEAD` gate between the
two Knope invocations.

### 1. Pre-flight (local)

Before merging the release PR, verify main is healthy:

```sh
mise run check      # fmt + lint + cargo check + clippy + schema:check
mise run test       # full test suite
mise run bench      # benchmark; >10% regression vs prev release = investigate
```

A `cargo audit` step for open advisories against `Cargo.lock` is intended
here; wiring it into mise is tracked under `lsm-d4z` so the runbook
doesn't reference an uninstalled tool.

Optionally preview what Knope will produce on the next release:

```sh
knope prepare-release --dry-run
```

This writes the version bumps + CHANGELOG entries to the working tree
without committing. Discard with `git restore .` once reviewed.

If any check fails, fix on `main` first. The release PR should be mergeable
against a healthy branch.

### 2. Review the Knope release PR

`knope-release.yml` (`prepare` job) opens it against `main` (branch: `release`; no label by
default). Click in and verify:

- **Per-package bumps** match the change shapes. Pre-1.0 rules: `feat` on a
  scope claimed by package X → minor bump for X (treated as breaking per
  semver posture); `fix` / `perf` → patch bump for X. Unscoped `feat`/`fix`
  bumps every package per Knope's "no scope = all packages" semantics — flag
  if that wasn't the intent.
- **Internal dep-pin updates** are in lockstep with each bump. A
  `linesmith-core` 0.2.0 → 0.3.0 bump should rewrite the pin in
  `crates/linesmith/Cargo.toml`'s `linesmith-core = { version = "0.3.0", path = ... }`.
  If a pin is stale, the workspace won't compile; investigate
  `knope.toml`'s `versioned_files` entries.
- **Per-crate CHANGELOG entries** read cleanly. The root `CHANGELOG.md` (for
  `linesmith`), `crates/linesmith-core/CHANGELOG.md`, and
  `crates/linesmith-plugin/CHANGELOG.md` get their own entries. Commit
  subjects are grouped under Features / Bug Fixes / Performance. `docs`,
  `chore`, `test`, `ci` commits are omitted (visible only in git log).
- **Changeset files** under `.changeset/` are deleted (Knope consumed them).
- **No surprise files** in the diff. Should be `Cargo.toml`, `Cargo.lock`,
  per-crate `Cargo.toml`s, per-crate `CHANGELOG.md`s, and any deleted
  `.changeset/*.md` files. Anything else is a red flag.

If the version, dep-pin, or CHANGELOG is wrong, open a fix PR (or a
changeset PR via `knope document-change`); when that PR merges to
`main`, `knope-release.yml`'s `prepare` job updates the release PR.

### 3. Merge

Merge the PR with the squash strategy (the repo's only enabled merge
method). On merge, `knope-release.yml` fires and:

1. **`release` job** — runs `knope release --verbose`. Tags each bumped
   package as `<crate>/v<version>` (e.g. `linesmith/v0.3.0`,
   `linesmith-core/v0.3.0`). Creates a GitHub Release per tag with
   CHANGELOG-derived notes.

Those tag pushes then fire a **second** `knope-release.yml` run:

2. **`publish` job** — mints a crates.io OIDC token via
   `rust-lang/crates-io-auth-action`, installs `cargo-release`, runs
   `cargo release publish --workspace --execute --no-confirm`.
   `cargo-release` checks each member against crates.io and skips
   crates already at their target version, then publishes the rest in
   leaf-first dependency order (`linesmith-plugin` → `linesmith-core`
   → `linesmith`). Load-bearing for single-package release cycles
   where only one crate bumped — bare `cargo publish --workspace`
   would error on the unchanged siblings.

Publish runs in its own tag-triggered run rather than alongside the
`release` job because crates.io rejects OIDC token requests from
`pull_request_target`. A multi-package release pushes several tags and
so starts several publish runs; the dedicated `knope-publish`
concurrency group holds one running plus one pending and evicts anything
queued in between, which is safe because each run publishes the whole
workspace.

**Expect one run per bumped crate's tag, plus the merge run** — so two
for a single-package cycle, four for a full three-crate release. Of
those three tag runs, expect one **cancelled** (evicted from the
concurrency queue, not a failure) and two green — the first publishes
everything and the second no-ops, because `cargo-release` skips
already-published members. If you only see the merge run, publish never
fired; recover with `gh workflow run "Knope Release" -f mode=publish`.

The `linesmith/v<version>` tag push (from step 1) also triggers
`release.yml` (cargo-dist), independently of the publish run.

### 4. Watch the cargo-dist workflow

```sh
gh run watch --exit-status
# or open Actions tab: gh workflow view release.yml --web
```

`release.yml` jobs, in order:

1. `plan` — reads `dist-workspace.toml`, computes the 6-target matrix
   (per the platform list in the spec).
2. `build-local-artifacts` (matrix) — 6 parallel legs; `fail-fast: false`
   so successful legs still upload artifacts if a sibling fails. Each leg
   emits a SLSA attestation via `actions/attest-build-provenance` in-step.
3. `build-global-artifacts` — generates shell/PowerShell installer scripts
   and aggregate `sha256.sum`.
4. `host` — uploads all artifacts to the existing `linesmith/v<version>`
   GitHub Release (`gh release upload`), then marks it
   `--draft=false --latest`.
5. `publish-homebrew-formula` — pushes `Formula/linesmith.rb` to
   `oakoss/homebrew-tap` (skipped automatically on prereleases).
6. `announce` — completion signal.

Total runtime: typically 10-15 min.

### 5. Verify (10-min budget)

Spot-check from a machine that doesn't have the repo cloned:

```sh
# crates.io (works for all three crates; the user-facing one is `linesmith`)
cargo install linesmith --locked
linesmith --version       # should match the linesmith/v<version> tag

# Shell installer (macOS / Linux)
curl -LsSf https://github.com/oakoss/linesmith/releases/latest/download/linesmith-installer.sh | sh

# Homebrew (macOS / Linux)
brew install oakoss/tap/linesmith

# Provenance
gh attestation verify "$(command -v linesmith)" --owner oakoss

# Doctor's update probe sees the new release cleanly (no parse WARN)
linesmith doctor --plain | grep -A2 "Self update"
```

### 6. Announce (optional)

Draft a short post summarizing the user-visible changes. The GitHub Release
body for the `linesmith/v<version>` tag is auto-populated by Knope
(CHANGELOG-derived) + cargo-dist (install snippets + download table);
narrative context can live in a separate social post or pinned issue.

## Documenting a change (contributor flow)

When the conventional-commit subject doesn't fully capture release impact
(e.g. a `refactor:` commit that's actually breaking, or a `feat:` whose
effect on individual packages isn't obvious from the scope), author a
changeset file:

```sh
knope document-change
```

Knope prompts interactively for:

1. **Which packages this affects** (multi-select from `linesmith-core`,
   `linesmith-plugin`, `linesmith`).
2. **Bump level per package** (`major`, `minor`, `patch`).
3. **A description** (free-form markdown — what changed, why, migration
   notes if breaking).

The result is a markdown file under `.changeset/` like:

```markdown
---
linesmith-core: minor
---

Rename Segment trait's render() method to compose(). Existing plugin
scripts continue to work via a deprecation shim that emits a
linesmith-warn on load; the shim is removed in linesmith-core 0.3.
```

Commit the changeset file alongside your code change. `knope prepare-release`
consumes it on the next release cycle and deletes it.

## Pre-release (RC) flow

Pre-release tags (`linesmith/vX.Y.Z-rc.N`, `-alpha.N`, `-beta.N`) are cut
manually, bypassing the Knope release-PR flow:

```sh
git tag -a linesmith/v0.3.0-rc.1 -m "v0.3.0-rc.1"
git push origin linesmith/v0.3.0-rc.1
```

`release.yml` fires on the tag, cargo-dist auto-detects the prerelease
suffix and:

- Creates a GitHub pre-release (the `host` job's fallback `gh release create`
  path covers this — no Knope-created release exists).
- Uploads all 6 target binaries.
- **Skips** the Homebrew formula push.
- **Skips** the `--latest` flag on the release edit.
- Does NOT publish to crates.io (Knope's release/publish workflow isn't
  involved).

Testers install via the shell installer or
`cargo install linesmith --git <url> --tag linesmith/v0.3.0-rc.1`.

## Dry-run

`release.yml` accepts `workflow_dispatch` and path-filtered `pull_request:`
triggers per ADR-0017. Both synthesize an ephemeral `linesmith/v<version>`
tag from `crates/linesmith/Cargo.toml`'s current version and run the full
cross-compile matrix without creating a GitHub Release. Auto-fires when a
PR touches `release.yml`, `dist-workspace.toml`, `Cargo.toml`,
`crates/*/Cargo.toml`, or `Cargo.lock` — catches cross-compile breakage from
dependency bumps before merge.

For Knope-side dry-runs (preview the next release PR's content):

```sh
knope prepare-release --dry-run
```

Knope writes the version bumps + CHANGELOG entries to the working tree but
skips the commit / push / PR steps. Discard with `git restore .` once
reviewed.

## Rollback

If a release ships broken artifacts:

1. Delete the affected tags locally and on origin. Per-package:
   `git tag -d linesmith/v{VERSION} && git push --delete origin linesmith/v{VERSION}`.
   Repeat for each bumped package's tag (`linesmith-core/v{VERSION}`,
   `linesmith-plugin/v{VERSION}` if those were created in the same release
   cycle).
2. If crates.io already published: `cargo yank --vers {VERSION} -p <crate>`
   per crate. Do not republish the same version — crates.io rejects it.
3. If Homebrew formula already pushed: revert the formula commit on
   `oakoss/homebrew-tap`.
4. Delete each GitHub Release Knope created (per-package release pages
   exist when more than one package bumped in a cycle).
5. Fix the issue, cut a new patch release (`linesmith/v{VERSION+1}` if
   crates.io published; re-use `linesmith/v{VERSION}` otherwise — fine if
   nothing made it out).

**Never force-push over a tag that made it to crates.io.** Users may have
`Cargo.lock`ed against that version.

See [`docs/specs/release-process.md`](../specs/release-process.md)
§Edge cases for the full recovery matrix.

## Knope migration tags (one-time, for the first Knope release)

The release-plz era used `<crate>-v<version>` per-package tags
(`linesmith-v0.1.3`, `linesmith-core-v0.2.0`, `linesmith-plugin-v0.1.3`)
plus the bare `v0.1.3` / `v0.2.0` workspace tags. Knope expects
`<crate>/v<version>` format. Before the first `knope prepare-release` run,
create equivalent per-package tags pointing at the existing release-plz tags
so Knope can compute "what's changed since the last release" per package:

```sh
# linesmith (binary) — the v0.2.0 manual unblock workspace tag is the latest
git tag linesmith/v0.2.0 v0.2.0

# linesmith-core — release-plz tagged this in the v0.2.0 unblock cycle
git tag linesmith-core/v0.2.0 linesmith-core-v0.2.0

# linesmith-plugin — last release-plz tag was v0.1.3
git tag linesmith-plugin/v0.1.3 linesmith-plugin-v0.1.3

git push origin linesmith/v0.2.0 linesmith-core/v0.2.0 linesmith-plugin/v0.1.3
```

These commands are safe to re-run (tag creation is idempotent if you delete
locally first). After the first Knope release, the legacy `<crate>-v*` and
bare `v*` tags can stay in place — `release.yml`'s tag filter still matches
the bare form for backward compatibility, and doctor's self-update probe
normalizes all four formats.

## First-release bootstrap (already done for v0.1.0)

Recorded here for the next new crate in the `oakoss` org, since the
chicken-and-egg story with crates.io trusted publishing isn't obvious.

1. Publish the crate once from local with a short-lived scoped token
   (24h expiration; scopes `publish-new` + `publish-update`, crate pattern
   matches the new crate name).
2. Go to `https://crates.io/crates/<name>/settings/new-trusted-publisher`
   and wire up `oakoss/<repo>` + `knope-release.yml` as the trusted
   publisher's workflow. Leave the environment field empty — this repo
   doesn't provision GitHub Environments, so the workflow ID alone
   identifies the dispatcher.
3. Delete the local token (`cargo logout`).
4. `knope-release.yml`'s `publish` job from that point forward publishes
   token-free via OIDC.

The tap repo also needs an initial commit on `main` before `cargo-dist`
can check it out for the first Homebrew formula push. Dropping a README
there is enough.
