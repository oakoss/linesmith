# Release Runbook

Step-by-step playbook for cutting a linesmith release. Assumes the prereqs
in [`docs/specs/release-process.md`](../specs/release-process.md) are already
set up (GitHub App on `oakoss/linesmith` + `oakoss/homebrew-tap`, org secrets
`BOT_CLIENT_ID`/`BOT_PRIVATE_KEY`, crates.io trusted publishing configured).

See the spec for the full contract (target matrix, supply-chain posture,
semver rules, edge cases). This file is the "what to type, in order."

## Who cuts releases

Any maintainer with push access to `main` and the ability to merge PRs
into `oakoss/linesmith`. No crates.io credentials needed: the release
pipeline uses OIDC trusted publishing. No Homebrew credentials needed:
the workflow mints a GitHub App installation token on the fly.

## Normal release flow

Every release lands by merging a `release-plz` PR. `release-plz` opens
the PR automatically when main accumulates commits matching
`^(feat|fix|perf|refactor)`. A typical cycle:

### 1. Pre-flight (local)

Before merging the release PR, verify main is healthy:

```sh
mise run check      # fmt + lint + cargo check + clippy
mise run test       # full test suite
mise run bench      # benchmark; >10% regression vs prev release = investigate
```

A `cargo audit` step for open advisories against `Cargo.lock` is intended
here; wiring it into mise is tracked under `lsm-d4z` so the runbook
doesn't reference an uninstalled tool.

If any of these fail, fix on `main` first. The release PR should be
mergeable against a healthy branch.

### 2. Review the release PR

`release-plz` opens it against `main` (label: `release`). Click in and
verify:

- **Version bump** matches the user-visible change shape. Pre-1.0
  rules: `feat` commits → minor (`0.x.y` → `0.x+1.0`, treated as
  breaking per semver posture); `fix` / `perf` / `refactor` → patch.
- **CHANGELOG entry** reads cleanly. Commit subjects are grouped under
  Features / Bug Fixes / Performance / Refactoring. `docs`, `chore`,
  `test`, `ci` commits are correctly omitted.
- **No surprise files** in the diff. Should be `Cargo.toml`,
  `Cargo.lock`, `CHANGELOG.md`. Anything else is a red flag.

If the version or CHANGELOG is wrong, push a fix commit to `main`;
`release-plz` will update the PR on the next push.

### 3. Merge

Merge the PR with the default merge-commit or squash strategy. On
merge, `release-plz.yml` fires a second time and:

1. Pushes the `v{VERSION}` tag to `main` at the merge commit.
2. Publishes to crates.io via OIDC trusted publishing (no token
   involved).

The tag push is what triggers `release.yml` (cargo-dist).

### 4. Watch the release workflow

```sh
gh run watch --exit-status
# or open Actions tab: gh workflow view release.yml --web
```

`release.yml` jobs, in order:

1. `plan` — reads `dist-workspace.toml`, computes the 7-target matrix.
2. `build-local-artifacts` (matrix) — 7 parallel legs; `fail-fast:
false` so successful legs still upload artifacts if a sibling fails.
   Each leg emits a SLSA attestation via
   `actions/attest-build-provenance` in-step (there is no separate
   `attest` job).
3. `build-global-artifacts` — generates shell/PowerShell installer
   scripts + aggregate `sha256.sum`.
4. `host` — creates the GitHub Release, attaches all artifacts.
5. `publish-homebrew-formula` — pushes `Formula/linesmith.rb` to
   `oakoss/homebrew-tap` (skipped automatically on prereleases).
6. `announce` — completion signal.

Total runtime: typically 10-15 min.

### 5. Verify (10-min budget)

Spot-check from a machine that doesn't have the repo cloned:

```sh
# crates.io
cargo install linesmith --locked
linesmith --version       # should match the tag

# Shell installer (macOS / Linux)
curl -LsSf https://github.com/oakoss/linesmith/releases/latest/download/linesmith-installer.sh | sh

# Homebrew (macOS / Linux)
brew install oakoss/tap/linesmith

# Provenance
gh attestation verify "$(command -v linesmith)" --owner oakoss
```

### 6. Announce (optional)

Draft a short post summarizing the user-visible changes. The GitHub
Release body is auto-populated by cargo-dist with install snippets +
a download table; narrative context can live in a separate social
post or pinned issue.

## Pre-release (RC) flow

Pre-release tags (`vX.Y.Z-rc.N`, `-alpha.N`, `-beta.N`) are cut
manually, bypassing `release-plz`:

```sh
git tag -a v0.2.0-rc.1 -m "v0.2.0-rc.1"
git push origin v0.2.0-rc.1
```

`release.yml` fires on the tag, cargo-dist auto-detects the prerelease
suffix and:

- Creates a GitHub pre-release (marked as such in the UI)
- Uploads all 7 target binaries
- **Skips** the Homebrew formula push
- **Skips** crates.io publish (release-plz isn't involved)

Testers install via the shell installer or `cargo install linesmith --git <url> --tag v0.2.0-rc.1`.

## Dry-run

`release.yml` also accepts `workflow_dispatch` triggers. The full
ephemeral-tag + `cargo publish --dry-run` plumbing is tracked under
`lsm-9ns` (run `bd show lsm-9ns` for the contract); wire it up before
relying on this trigger for PR validation.

## Rollback

If a release ships broken artifacts:

1. Delete the tag locally and on origin:

   ```sh
   git tag -d v{VERSION}
   git push --delete origin v{VERSION}
   ```

2. If crates.io already published: `cargo yank --vers {VERSION}`. Do
   not republish the same version — crates.io rejects it.
3. If Homebrew formula already pushed: revert the formula commit on
   `oakoss/homebrew-tap`.
4. Delete the GitHub Release if created.
5. Fix the issue, cut a new patch release (`v{VERSION+1}` if crates.io
   published; otherwise re-use `v{VERSION}` is fine if nothing made it
   out).

**Never force-push over a tag that made it to crates.io.** Users may
have `Cargo.lock`ed against that version.

See [`docs/specs/release-process.md`](../specs/release-process.md)
§Edge cases for the full recovery matrix.

## First-release bootstrap (already done for v0.1.0)

Recorded here for the next new crate in the `oakoss` org, since the
chicken-and-egg story with crates.io trusted publishing isn't obvious.

1. Publish the crate once from local with a short-lived scoped token
   (24h expiration; scopes `publish-new` + `publish-update`, crate
   pattern matches the new crate name).
2. Go to `https://crates.io/crates/<name>/settings/new-trusted-publisher`
   and wire up `oakoss/<repo>` + `release-plz.yml` as the trusted
   publisher.
3. Delete the local token (`cargo logout`).
4. `release-plz.yml` from that point forward publishes token-free via
   OIDC.

The tap repo also needs an initial commit on `main` before
`cargo-dist` can check it out for the first Homebrew formula push.
Dropping a README there is enough.
