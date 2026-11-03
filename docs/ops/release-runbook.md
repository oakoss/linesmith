# Release Runbook

Step-by-step playbook for cutting a linesmith release. Assumes the prereqs
in [`docs/specs/release-process.md`](../specs/release-process.md) are already
set up (GitHub App on `oakoss/linesmith` + `oakoss/homebrew-tap`, org secrets
`BOT_CLIENT_ID` / `BOT_PRIVATE_KEY`, crates.io trusted publishing configured
for each of the three crates with `oakum.yml` as the workflow and
no environment requirement).

See the spec for the full contract (target matrix, supply-chain posture,
semver rules, edge cases). This file is the "what to type, in order."

## Who cuts releases

Any maintainer with push access to `main` and the ability to merge PRs into
`oakoss/linesmith`. No crates.io credentials needed: the release pipeline
uses OIDC trusted publishing. No Homebrew credentials needed: the workflow
mints a GitHub App installation token on the fly.

## Normal release flow

Every release lands by merging the version pull request. `oakum.yml`
(`version` job) opens / force-updates it automatically on each push to
`main` that leaves a bump file pending under `.changeset/`. Pushes with
nothing pending land as green no-op runs.

### 1. Pre-flight (local)

Before merging the version pull request, verify main is healthy:

```sh
mise run check      # fmt + lint + cargo check + clippy + schema:check
mise run test       # full test suite
mise run bench      # benchmark; >10% regression vs prev release = investigate
```

A `cargo audit` step for open advisories against `Cargo.lock` is intended
here; wiring it into mise is tracked under `lsm-d4z` so the runbook
doesn't reference an uninstalled tool.

Optionally preview the next release:

```sh
oakum status
```

It prints the release plan from the pending bump files and writes nothing.

If any check fails, fix on `main` first. The version pull request should be
mergeable against a healthy branch.

### 2. Review the version pull request

`oakum.yml` (`version` job) opens it against `main` (branch:
`oakum/version-packages`, author: the `oakoss` App). Click in and verify:

- **Per-package bumps** match the bump files. Pre-1.0 (`zero-major`): a
  `minor` or `major` bump file moves `0.x` to `0.x+1.0`, `patch` stays a
  patch. A library bump carries `linesmith` along at least at patch
  (`cascade` in the plan); if users see the change in the binary, the bump
  file should name `linesmith` at that level too.
- **Internal dep-pin updates** are in lockstep with each bump. A
  `linesmith-core` 0.4.1 → 0.5.0 bump rewrites the pin in
  `crates/linesmith/Cargo.toml` to `linesmith-core = { version = "^0.5.0", path = ... }`.
- **Per-crate CHANGELOG entries** read cleanly:
  `crates/linesmith/CHANGELOG.md`, `crates/linesmith-core/CHANGELOG.md`,
  and `crates/linesmith-plugin/CHANGELOG.md` each get their own entry, the
  bump files' bodies under `### Added` / `### Changed` / `### Fixed` by level.
- **Bump files** under `.changeset/` are deleted (oakum consumed them).
- **No surprise files** in the diff. Should be `Cargo.lock`, per-crate
  `Cargo.toml`s, per-crate `CHANGELOG.md`s, and the deleted
  `.changeset/*.md` files. Anything else is a red flag.

If the version, dep-pin, or CHANGELOG is wrong, open a fix PR with a
corrected or additional bump file (`oakum add`); when that PR merges to
`main`, the `version` job updates the version pull request.

### 3. Merge

Merge the PR with the squash strategy (the repo's only enabled merge
method). The push to main fires `oakum.yml`:

1. **`release` job** — runs `oakum release`. Tags each bumped
   package as `<crate>/v<version>` (e.g. `linesmith/v0.5.0`,
   `linesmith-core/v0.5.0`). Creates a GitHub Release per tag whose body is
   that version's CHANGELOG section.

Each of those tag pushes then fires its own `oakum.yml` run:

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
`release` job, so the irreversible step can be cancelled on its own. A
multi-package release pushes several tags and
so starts several publish runs; the dedicated `oakum-publish`
concurrency group holds one running plus one pending and evicts anything
queued in between, which is safe because each run publishes the whole
workspace.

**Expect one run per bumped crate's tag, plus the main-push run** — so two
for a single-package cycle, four for a full three-crate release. Of
those three tag runs, expect one **cancelled** (evicted from the
concurrency queue, not a failure) and the rest green — the first
publishes everything and the others no-op, because `cargo-release` skips
already-published members.

A no-op run is green rather than red only because the publish step
tolerates that one case explicitly: when every member is skipped,
`cargo release` exits 2 with `no packages selected`, and the step
converts that to success — but only after counting one
`disabled due to previous publish` warning for **every** crates.io-publishable
member, using the same `cargo metadata` filter `verify-published` uses.
The count matters: a crate can leave the selection silently (cargo-release
logs several such routes at `debug`/`trace`), and merely checking that the
phrase appeared somewhere would read one crate's already-published warning
as covering a sibling that was never published at all. A short count fails
red and names the shortfall. **So a red publish run means a real failure,
with one bounded exception the log names outright.** Before this
tolerance existed, the 0.4.0 release ended with a red run on a release
that had fully succeeded (`lsm-kma0`).

The exception is a sparse-index hiccup. `cargo-release` treats an index
lookup error as "not published" — deliberately, so an outage publishes
rather than skips — and that member therefore stays in the selection, gets
a real publish attempt, and is rejected by crates.io as already uploaded.
The run goes red on a release that was already complete. Recognise it by
the log: a `failed to read metadata for <crate>` warning followed by an
already-uploaded rejection, and no `no packages selected` line at all.
Confirm the version on the index and re-run the tag; do not treat it as a
stranded release.

A **cancelled** publish run is not automatically harmless, despite the
queue-eviction note above: cancelling a tag run is also the only way to
stop an in-flight crates.io push. Distinguish them by the log — an evicted
run never reaches the `Install cargo-release` step, whereas a run killed
mid-publish streams its progress up to the point it stopped (the step uses
`tee`, so the log survives cancellation). If a cancelled run got as far as
publishing, treat it as a partial release and check the registry before
re-running.

If you only see the main-push run, publish never fired; recover with
`gh workflow run oakum.yml -f tag=<crate>/v<version>`. The `tag` input is
required — recovery publishes that tag's commit, never `main`, which after
any later merge would hold something the release never contained.

If the main-push run's `release` job failed, rerun that run's failed jobs:
`oakum release` tags whatever main declares that has no tag yet, and resumes
a tag that already exists rather than recreating it.

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
   GitHub Release (`gh release upload`), puts oakum's CHANGELOG section
   above cargo-dist's install notes, and marks it `--draft=false --latest`.
5. `publish-homebrew-formula` — pushes `Formula/linesmith.rb` to
   `oakoss/homebrew-tap` (skipped automatically on prereleases).
6. `announce` — completion signal.

Total runtime: typically 10-15 min.

### 5. Verify

A green **`verify-published` job** means crates.io serves every version in
the manifests at the pushed commit, unyanked. It asserts the manifest
versions are live, not that this particular run shipped something — on a
push that released nothing, the manifests still hold already-published
versions and the job goes green. Read it as "the registry matches what
main says it should be."

The checks below cover what nothing automated does: installers, the tap,
provenance, and the doctor probe.

Run these from a machine that doesn't have the repo cloned:

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
body for the `linesmith/v<version>` tag is auto-populated by oakum
(the CHANGELOG section) + cargo-dist (install snippets + download table);
narrative context can live in a separate social post or pinned issue.

## Documenting a change (contributor flow)

A pull request that changes what a user gets carries a bump file. CI's
`oakum check --strict` fails a pull request whose changed crate nothing
covers, naming the commands that cover it. A branch whose commits to a
crate all release nothing (`refactor:`, `test:`, `chore:`, `docs:`) needs
none.

```sh
oakum add --packages "linesmith-core:minor,linesmith:minor" --message "…"
oakum add --interactive              # guided prompts
oakum generate --from main           # from the branch's conventional commits
oakum add --none --packages "linesmith:none" --message "…"  # covered, no release
```

The result is a markdown file under `.changeset/` like:

```markdown
---
linesmith-core: minor
linesmith: minor
---

Rename Segment trait's render() method to compose(). Existing plugin
scripts continue to work via a deprecation shim that emits a
linesmith-warn on load; the shim is removed in linesmith-core 0.6.
```

Name every crate the change should release, at the level it should take:
a library bump releases the binary that ships it at patch on its own. The
body is the CHANGELOG entry, written for someone deciding whether to
upgrade. Commit the bump file alongside your code change; the next version
pull request consumes it and deletes it.

## Pre-release (RC) flow

Pre-release tags (`linesmith/vX.Y.Z-rc.N`, `-alpha.N`, `-beta.N`) are cut
manually, bypassing the version pull request flow:

```sh
git tag -a linesmith/v0.3.0-rc.1 -m "v0.3.0-rc.1"
git push origin linesmith/v0.3.0-rc.1
```

Cutting one requires bypass on the org tag ruleset — see §Tag protection.

`release.yml` fires on the tag, cargo-dist auto-detects the prerelease
suffix and:

- Creates a GitHub pre-release (the `host` job's fallback `gh release create`
  path covers this — no oakum-created release exists).
- Uploads all 6 target binaries.
- **Skips** the Homebrew formula push.
- **Skips** the `--latest` flag on the release edit.
- Does NOT publish to crates.io (`oakum.yml`'s publish trigger matches
  exact `<crate>/vX.Y.Z` tags only).

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

To preview the next version pull request's plan, run `oakum status`
locally, or read the Release plan comment CI posts on every pull request.

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
4. Delete each GitHub Release oakum created (per-package release pages
   exist when more than one package bumped in a cycle).
5. Fix the issue, cut a new patch release (`linesmith/v{VERSION+1}` if
   crates.io published; re-use `linesmith/v{VERSION}` otherwise — fine if
   nothing made it out).

**Never force-push over a tag that made it to crates.io.** Users may have
`Cargo.lock`ed against that version.

Step 1's tag deletion needs bypass on the org tag ruleset — see
§Tag protection.

See [`docs/specs/release-process.md`](../specs/release-process.md)
§Edge cases for the full recovery matrix.

## Tag protection

A `<pkg>/vX.Y.Z` tag push mints a crates.io publishing token, so tag
creation is a privileged operation. The ruleset deliberately covers a
wider glob than that publish trigger, so prerelease and legacy tags are
protected too. The org ruleset `Release Tags - Org` (id 14384235,
target `tag`, applies to every oakoss repo) enforces `creation`,
`deletion`, `non_fast_forward`, and `update` over:

```text
refs/tags/v*        refs/tags/*/v*
refs/tags/@*        refs/tags/*-v*
```

`*/v*` covers the per-package format oakum tags; `*-v*` covers the legacy
release-plz tags so history stays protected.

Only two actors bypass it: the `oakoss` App (`Integration:3174795`),
which is what the release workflow tags as, and `OrganizationAdmin`.
Both ids are re-derivable if the ruleset is ever recreated:
`gh api orgs/oakoss/rulesets --jq '.[] | select(.target=="tag") | "\(.id) \(.name)"'`
and `gh api apps/oakoss --jq .id`.
Everyone else is rejected — including any workflow using the default
`GITHUB_TOKEN`, which is `github-actions[bot]` and holds no bypass.

That makes every manual tag operation in this runbook admin-only:
cutting an RC tag, deleting tags during rollback, and the one-time
migration-tag push. A rejected push is not automatically this ruleset,
though — an under-scoped credential or a wrong-repository push looks
much the same from the client. The query below is what tells them
apart: a row for your ref means the ruleset acted; no row means look
at the credential.

To confirm the ruleset evaluated a push (bypasses are logged, so this
works even when you bypass it):

```sh
gh api --paginate repos/oakoss/linesmith/rulesets/rule-suites \
  --jq '.[] | select(.ref | startswith("refs/tags/"))
        | "\(.result) \(.ref) actor=\(.actor_name)"'
```

`--paginate` matters: without it you get only the first page, which
branch pushes dominate. Narrow to a single tag by appending
`| grep linesmith/v0.3.0-rc.1`.

## Package tags for the bare v0.1.x tags (one-time, for oakum)

The bare `v0.1.0` and `v0.1.1` tags released the `linesmith` crate alone,
before per-package tagging. With several packages tagged, oakum cannot tell
which package a bare tag released, and every oakum command exits 2 until a
package tag names the same commit. Done 2026-11-02:

```sh
gh workflow disable release.yml
git tag -s linesmith/v0.1.0 1ab79e1 -m "linesmith 0.1.0"
git tag -s linesmith/v0.1.1 c219174 -m "linesmith 0.1.1"
git push origin linesmith/v0.1.0 linesmith/v0.1.1
gh workflow enable release.yml
```

`release.yml` is disabled around the push because a tag push runs the
workflow file at the tagged commit: at `1ab79e1` and `c219174` that is the
v0.1 `release.yml`, whose `**[0-9]+.[0-9]+.[0-9]+*` trigger matches these
tags and would create 0.1.x GitHub Releases and push a 0.1.x Homebrew
formula. The bare tags and their GitHub Releases stay. Creating these needs
bypass on the org tag ruleset; see §Tag protection.

## Knope migration tags (one-time, done for the first Knope release)

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

These commands are spent — the migration is done. Re-running them now
needs bypass on the org tag ruleset (both the delete and the re-push);
see §Tag protection. After the first Knope release, the legacy `<crate>-v*` and
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
   and wire up `oakoss/<repo>` + `oakum.yml` as the trusted
   publisher's workflow. Leave the environment field empty — this repo
   doesn't provision GitHub Environments, so the workflow ID alone
   identifies the dispatcher.
3. Delete the local token (`cargo logout`).
4. `oakum.yml`'s `publish` job from that point forward publishes
   token-free via OIDC.

The tap repo also needs an initial commit on `main` before `cargo-dist`
can check it out for the first Homebrew formula push. Dropping a README
there is enough.
