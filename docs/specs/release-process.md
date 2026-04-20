# Release Process

- Status: draft
- Version: 0.1
- Last updated: 2026-04-20
- Driving ADRs: [ADR-0001](../adrs/0001-use-rust-for-runtime.md), [ADR-0007](../adrs/0007-cargo-dist-distribution.md)

## Overview

This spec defines how linesmith versions are cut, built, packaged, signed (or deliberately not), and distributed. The release pipeline is `cargo-dist` (binaries, installers, Homebrew formula) + `release-plz` (version bumps, CHANGELOG, crates.io publish), running as two chained GitHub Actions workflows: `release-plz.yml` opens release PRs and tags+publishes on merge, and `release.yml` fires on the pushed tag to build binaries. A maintainer cuts a release by merging the release PR; everything after is automation.

> **Naming note.** Upstream renamed `cargo-dist` to `dist` (see [ADR-0007](../adrs/0007-cargo-dist-distribution.md)); this spec says `cargo-dist` throughout to match widely-referenced ecosystem documentation. They're the same tool. When the rename lands everywhere in third-party tutorials, the spec will revise.

Target-platform coverage, installer choices, and the single-binary story are decided in [ADR-0007](../adrs/0007-cargo-dist-distribution.md). This spec turns those decisions into concrete version semantics, a platform matrix, workflow step ordering, supply-chain signing posture, and a runbook.

Out of scope: user-facing install instructions (those belong in the README); mirror distribution (e.g., APT, DEB, Snap, Nix) — deferred until v0.2+ with a demand signal; auto-update in-binary (deferred indefinitely; `doctor --full` reports newer releases).

## Requirements

### Functional

- Version tags use semver: `vMAJOR.MINOR.PATCH` (e.g., `v0.1.0`, `v0.2.0-rc.1`)
- `release-plz` manages version bumps and CHANGELOG entries from Conventional Commits
- `cargo-dist` builds binaries for the platform matrix on tag-push
- `cargo-dist` generates a shell installer (`curl | sh`), a PowerShell installer (`irm | iex`), and a Homebrew formula
- `cargo install linesmith` remains automatically available for Rust-toolchain users (no extra configuration)
- Every tagged release gets a GitHub Release with artifacts, a CHANGELOG excerpt, and a source tarball
- Pre-1.0 releases treat minor bumps as breaking (per semver convention for `0.y.z`); no patches may introduce breaking changes
- A dry-run workflow lets maintainers validate the pipeline without publishing (invoked via `workflow_dispatch`)
- Every released binary is reproducible within a single `cargo-dist` version — same commit + same toolchain → byte-identical output

### Non-functional

- Full release pipeline (tag-push → all artifacts live) completes in <20 minutes on GitHub-hosted runners
- Maintainer effort per release: merge a release PR; no manual artifact uploads, formula edits, or `cargo publish` calls
- Supply chain: artifacts carry GitHub attestations (SLSA level 3 via `actions/attest-build-provenance`); `cargo-auditable` embeds dep list in the binary for post-release CVE correlation
- All GitHub Actions uses are pinned to commit SHAs (not tags) — no silent action-tag drift
- No long-lived API tokens in repository secrets — forbidden: `CARGO_REGISTRY_TOKEN`, `HOMEBREW_TOKEN`, PATs. Permitted: short-lived OIDC tokens (crates.io), GitHub App installation credentials (Homebrew tap push), and the workflow's auto-provisioned `GITHUB_TOKEN`. GitHub App private keys stored as Actions secrets are acceptable because they mint bounded-scope installation tokens, not long-lived registry credentials
- Cost: stay under the free tier for `oakoss/linesmith` (GitHub Free tier: 2000 Actions minutes/month)

## Interface / Contract

### Tag format and semver posture

```text
v{MAJOR}.{MINOR}.{PATCH}[-{PRERELEASE}]
```

Examples:

```text
v0.1.0         first public release
v0.1.1         bug-fix patch of v0.1.0
v0.2.0-rc.1    pre-release candidate
v0.2.0         cut from rc.1
v1.0.0         first stable public API promise
```

Semver posture:

| Stage       | Minor bump (`0.x.y` → `0.x+1.0`) | Patch bump (`0.x.y` → `0.x.y+1`) |
| ----------- | -------------------------------- | -------------------------------- |
| Pre-1.0     | Breaking changes allowed         | Bug fixes only; no breaking      |
| Post-1.0    | Additive features                | Bug fixes only                   |
| Pre-release | `-rc.N`, `-alpha.N`, `-beta.N`   | Not valid on patch versions      |

"Breaking change" for linesmith means any of:

- A public-facing binary CLI surface change that rejects previously-valid invocations
- A config-schema field removed or renamed without a compatibility shim
- A public Rust API surface change in the `linesmith` crate (if we publish a library crate later)
- A plugin-API schema change that requires rhai script edits (per plugin-api.md's v0.1/v0.2 line)

Adding a new command, a new segment, a new theme, or a new config field with a default value is additive and warrants a minor bump (pre-1.0) or a patch (post-1.0).

### Target platform matrix

Seven targets in the v0.1 release, shipped simultaneously from one tag. This is one target more than lsm-9sa's original scope (6); the extra `x86_64-unknown-linux-musl` build covers Alpine and distroless-container users, and the marginal cost is one additional cargo-dist matrix entry on the same Linux runner.

| Target triple               | Platform      | Install method priority |
| --------------------------- | ------------- | ----------------------- |
| `x86_64-apple-darwin`       | macOS Intel   | brew, shell             |
| `aarch64-apple-darwin`      | macOS ARM     | brew, shell             |
| `x86_64-unknown-linux-gnu`  | Linux glibc   | brew, shell             |
| `x86_64-unknown-linux-musl` | Linux musl    | shell                   |
| `aarch64-unknown-linux-gnu` | Linux ARM     | brew, shell             |
| `x86_64-pc-windows-msvc`    | Windows Intel | powershell              |
| `aarch64-pc-windows-msvc`   | Windows ARM   | powershell              |

Dropping a target from the matrix is a minor-version event (user-visible regression); adding a target is additive.

### Installer methods

Four install paths; `cargo-dist` owns the first three, `cargo install` is the free Rust-toolchain fallback.

**Shell installer** (macOS + Linux):

```sh
curl -LsSf https://github.com/oakoss/linesmith/releases/latest/download/linesmith-installer.sh | sh
```

Downloads the correct target binary for the host, places it in `~/.local/bin/` (or `$CARGO_HOME/bin`), and adds to `PATH` via a shell init snippet.

**PowerShell installer** (Windows):

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/oakoss/linesmith/releases/latest/download/linesmith-installer.ps1 | iex"
```

Same semantics, Windows-appropriate paths.

**Homebrew** (macOS + Linux):

```sh
brew install oakoss/tap/linesmith
```

Tap repo: `github.com/oakoss/homebrew-tap`. The formula is generated and pushed by `cargo-dist` on each release. Users run `brew tap oakoss/tap` once; subsequent `brew upgrade` pulls new versions automatically.

**Cargo**:

```sh
cargo install linesmith
```

Rust-toolchain required. Compiles from source; users pay the ~60-90s compile time. Automatic fallback — no workflow work required. Crate name: `linesmith`.

### CHANGELOG contract

- Format: [Keep a Changelog](https://keepachangelog.com/) structure, written by `git-cliff` from Conventional Commits
- Generation: `release-plz` invokes `git-cliff` on PR creation; the maintainer reviews and merges
- Commit types mapped:
  - `feat` → Added
  - `fix` → Fixed
  - `perf` → Performance
  - `refactor` → Changed (internal-only)
  - `docs`, `chore`, `test`, `ci` → omitted from user-facing CHANGELOG (visible in git log)
- Beads footer (bare `lsm-xyz`) is preserved in the CHANGELOG entry as a linked reference
- Breaking changes flagged via `!` in commit subject (e.g., `feat(api)!: rename render ctx`) are hoisted to a "⚠ BREAKING" subsection and force a minor bump

`cliff.toml` in the repo root defines the template.

### release-plz vs cargo-dist split

Two workflows are required because `release-plz` creates the tag (which `cargo-dist` then reacts to):

| Concern                        | Owner         | Workflow          | Fires on                               |
| ------------------------------ | ------------- | ----------------- | -------------------------------------- |
| Version bump in `Cargo.toml`   | `release-plz` | `release-plz.yml` | Push to `main` with releasable commits |
| CHANGELOG entry generation     | `release-plz` | `release-plz.yml` | Same                                   |
| Release PR open                | `release-plz` | `release-plz.yml` | Same                                   |
| Git tag on release PR merge    | `release-plz` | `release-plz.yml` | Release PR merge                       |
| `cargo publish` (crates.io)    | `release-plz` | `release-plz.yml` | Release PR merge (after tag push)      |
| Multi-platform binary builds   | `cargo-dist`  | `release.yml`     | Tag push (from the step above)         |
| GitHub Release + artifacts     | `cargo-dist`  | `release.yml`     | Tag push                               |
| Shell / PowerShell installers  | `cargo-dist`  | `release.yml`     | Tag push                               |
| Homebrew formula update + push | `cargo-dist`  | `release.yml`     | Tag push                               |
| Build attestations (SLSA)      | `cargo-dist`  | `release.yml`     | Tag push                               |

`release-plz` is the conductor, living in `release-plz.yml`; `cargo-dist` is the specialist, living in `release.yml`. The handoff is the git tag: `release-plz.yml` pushes it on release-PR merge, and that tag push triggers `release.yml`. If `release-plz publish` fails before tagging, `release.yml` never starts and no binaries ship.

### Release profile

Matches [ADR-0007](../adrs/0007-cargo-dist-distribution.md) §Build profile:

```toml
[profile.release]
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
opt-level = "z"      # optimize for size; revisit if startup latency regresses
```

`opt-level = "z"` is size-first; `cargo bloat --release` runs as a post-build metric check (not gate) and reports any >5% size regression against the previous release's binary.

### Supply-chain posture

| Concern                    | Mechanism                                                                                                                           |
| -------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| Dependency auditability    | `cargo-auditable build` embeds a parseable dep manifest in the ELF/PE section; tools like `cargo audit --binary` can read post-ship |
| Build provenance           | `actions/attest-build-provenance@<SHA>` generates SLSA attestations for every release artifact                                      |
| Reproducible CI            | All `uses:` in `.github/workflows/*.yml` pinned to commit SHAs; Renovate updates them via managed PRs                               |
| Crates.io publish          | OIDC trusted publishing configured in `crates.io` settings (no `CARGO_REGISTRY_TOKEN` secret in the repo)                           |
| Homebrew formula push      | GitHub App installation token via `oakoss`-scoped app; not a PAT                                                                    |
| Release artifact integrity | SHA256 checksums in the GitHub Release body; `cargo-dist` generates                                                                 |

A secret-less pipeline (no `CRATES_IO_TOKEN`, no `HOMEBREW_TOKEN` in GitHub Secrets) is a hard requirement. If any step needs a long-lived secret, the workflow change is rejected at review.

### Signing posture

**v0.1 ships unsigned.** macOS users hit Gatekeeper's "unknown developer" prompt; the install docs tell them to run:

```sh
xattr -d com.apple.quarantine /usr/local/bin/linesmith
```

Linux distributions with AppArmor / SELinux treat unsigned binaries normally; no user action. Windows SmartScreen warns for unrecognized publishers; users click "More info → Run anyway."

v0.2+ revisits codesigning if (a) users report enough friction to justify the ~$100/year Apple developer ID cost, or (b) a free signing identity becomes viable (SigStore's macOS story, or GitHub's emerging artifact signing).

Users who want to verify artifact provenance before install use:

```sh
gh attestation verify <binary-path> --owner oakoss
```

This validates the SLSA attestation attached during the release.

### Crates.io publishing

`linesmith` crate on crates.io — single crate published (not a workspace). Published via OIDC trusted publishing:

- One-time: configure `oakoss/linesmith` as a trusted publisher in the `linesmith` crates.io settings page (see <https://crates.io/docs/trusted-publishing>)
- Per-release: `release-plz` uses the workflow's OIDC token to request a short-lived crates.io token and runs `cargo publish`
- No `CARGO_REGISTRY_TOKEN` secret in the repo

The crates.io description, README snippet, and keywords are sourced from `Cargo.toml`'s `[package]` metadata. License: `MIT OR Apache-2.0` (standard Rust-ecosystem dual license).

### Homebrew tap

Tap repo: `github.com/oakoss/homebrew-tap`. Layout:

```text
oakoss/homebrew-tap/
├── Formula/
│   └── linesmith.rb
└── README.md
```

The formula is regenerated + pushed by `cargo-dist` on each release. First-time users run `brew tap oakoss/tap`; subsequent updates are automatic via `brew upgrade`.

The formula ships both `x86_64-apple-darwin` and `aarch64-apple-darwin` bottles; Homebrew auto-selects based on host architecture. Linux users on Homebrew get `x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu` bottles.

## Behavior

### Release runbook

Day-of-release steps for a maintainer:

1. **Pre-flight checks** (local):
   - `mise run check` — all tests + lints pass
   - `mise run bench` — no >10% regression against the previous release's benches
   - `cargo audit` — no new advisories
   - `linesmith doctor --plain` on a clean checkout — passes
2. **Review the `release-plz` PR** that's been open on `main`. Verify the CHANGELOG entry reads correctly and the version bump matches the user-visible change shape.
3. **Merge the `release-plz` PR.** This triggers `release-plz`'s second pass: it tags the commit `v{VERSION}` and pushes the tag.
4. **Watch the Actions workflow.** The tag-push fires `release.yml`, which:
   a. Runs `release-plz publish` → `cargo publish` (crates.io).
   b. Runs `cargo-dist` matrix across all seven target triples.
   c. Generates installers + Homebrew formula.
   d. Attests build provenance.
   e. Pushes the Homebrew formula to `oakoss/homebrew-tap`.
   f. Creates the GitHub Release with artifacts + CHANGELOG excerpt.
5. **Verify artifacts** (10-minute budget):
   - Pull the macOS `aarch64-apple-darwin` binary from the release, run `linesmith --version`, confirm the version string matches the tag
   - Run `linesmith doctor --plain` on the downloaded binary, confirm all-PASS
   - Verify `brew install oakoss/tap/linesmith` on a fresh macOS VM (or container)
   - Verify `cargo install linesmith` from a fresh Rust install
6. **Announce.** Draft a short post — release notes excerpt + install one-liner for each platform — and publish to the announcement channels (README "Release Log" section + any social channels).

If step 4 fails partway through, see §Edge cases §Rollback.

### Workflow structure

Two workflow files, chained via the git-tag event:

**`.github/workflows/release-plz.yml`** — runs on push to `main` and via `workflow_dispatch`. Creates release PRs, and on release-PR merge, tags + publishes to crates.io:

```text
on:
  push:
    branches: [main]
  workflow_dispatch:

jobs:
  release-plz:
    # Opens release PRs with version bump + CHANGELOG when commits accumulate.
    # On release-PR merge: creates the v{VERSION} tag, pushes it, then runs
    # `cargo publish` via OIDC trusted publishing. The tag push is what fires
    # release.yml below.
```

**`.github/workflows/release.yml`** — runs on tag push (from release-plz.yml) and via `workflow_dispatch` for dry-runs:

```text
on:
  push:
    tags: ['v*.*.*']
  workflow_dispatch:
    inputs:
      allow_dirty: { type: boolean, default: false }

jobs:
  cargo-dist-plan:
    # Runs `dist plan` to validate the release matrix before building.
  cargo-dist-build:
    needs: cargo-dist-plan
    strategy:
      matrix: <7 target triples>
      fail-fast: false   # lets successful legs finish and upload artifacts even on sibling failure
    # Cross-compiles per target. Each matrix leg uploads its binary as an artifact.
  cargo-dist-global-publish:
    needs: cargo-dist-build
    # Generates installers, Homebrew formula, GitHub Release with all artifacts.
    # A matrix-level failure still blocks this job (standard `needs:` semantics);
    # see §Edge cases for the recovery path when one target fails.
  attest:
    needs: cargo-dist-build
    # Runs actions/attest-build-provenance on every artifact successfully built.
```

The tag-push handoff from `release-plz.yml` to `release.yml` is the coupling point. If `release-plz publish` fails (e.g., crates.io rejects the upload), no tag is pushed, and `release.yml` never starts — so binaries can't get ahead of the published crate.

### Pre-release workflow

Pre-release tags (`vX.Y.Z-rc.N`, `-alpha.N`, `-beta.N`) skip Homebrew formula pushes and don't publish to crates.io by default. They do produce GitHub Release artifacts with all seven target binaries, so testers can install via:

```sh
curl -LsSf https://github.com/oakoss/linesmith/releases/download/v0.2.0-rc.1/linesmith-installer.sh | sh
```

`cargo install linesmith --git https://github.com/oakoss/linesmith --tag v0.2.0-rc.1` also works for Rust-toolchain pre-release testers.

### Dry-run workflow

`release.yml` supports `workflow_dispatch` with an `allow_dirty: true` input. Dispatched from a branch, it runs the full pipeline but:

- Targets an ephemeral tag (`v0.0.0-dry-run-<commit>`), deleted after the run
- Replaces `cargo publish` with `cargo publish --dry-run` (validates package contents and metadata without pushing)
- Skips the Homebrew formula push (no write to `oakoss/homebrew-tap`)
- Uploads binary artifacts as GitHub Actions workflow artifacts (7-day retention) rather than creating a GitHub Release
- Still generates build attestations so the attestation flow itself is exercised

Used for validating pipeline changes before cutting a real release. Dry-run runs do not consume crates.io version slots.

## Edge cases

| Case                                                          | Handling                                                                                                                                                                                                                                                                                                                                                                          |
| ------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `release-plz` fails to publish to crates.io (transient 5xx)   | Workflow re-run-able; `cargo-dist` gated on successful crates.io step, so no orphaned artifacts                                                                                                                                                                                                                                                                                   |
| `cargo-dist` fails on one target (e.g., ARM Linux OOM)        | `cargo-dist-global-publish` blocks because it `needs: cargo-dist-build` (whole matrix). With `fail-fast: false`, successful legs finish and upload their binaries as workflow artifacts. Re-run the failed leg from the Actions UI, which resumes the chain and lets `global-publish` finish. No GitHub Release, installers, or formula update ships until every target completes |
| Crates.io publish succeeds but `cargo-dist` fails entirely    | Partial release: `cargo install linesmith` works but curl/brew/powershell installers have no artifacts. Rerun failed jobs; do NOT yank. Pin a tracker issue noting the asymmetry until binaries catch up                                                                                                                                                                          |
| Homebrew formula push fails (auth, rate-limit, repo conflict) | Binary + shell-installer releases succeed; brew users see "formula not found"; formula re-pushed via manual re-run                                                                                                                                                                                                                                                                |
| Tag pushed to a commit that's not on main                     | `release-plz` rejects with "tag not on main"; workflow exits 1; manual cleanup: `git push --delete origin v{VERSION}` + retag                                                                                                                                                                                                                                                     |
| Tag pushed without a release-plz PR                           | `release-plz publish` detects missing CHANGELOG entry and fails; workflow aborts before crates.io publish                                                                                                                                                                                                                                                                         |
| Need to yank a release from crates.io                         | `cargo yank --vers X.Y.Z` locally; no workflow involved; GitHub Release stays but add a "⚠ Yanked" note in the release body                                                                                                                                                                                                                                                       |
| Critical CVE mid-release-cycle                                | Cut a new patch release (`v0.1.N+1`) with the fix; don't try to edit the existing release's artifacts                                                                                                                                                                                                                                                                             |
| Secret leak (e.g., OIDC config)                               | Rotate immediately via crates.io + GitHub settings; new releases use rotated identity; old tags stay as-is                                                                                                                                                                                                                                                                        |
| Contributor pushes a tag without permission                   | Branch protection + `release.yml` require `secrets.GITHUB_TOKEN` permissions that only repo admins hold                                                                                                                                                                                                                                                                           |
| Accidental `v1.0.0` tag before we're ready                    | Delete the tag both locally and on origin; force-push is not needed (tag isn't a branch); `release.yml` is re-idempotent on retag                                                                                                                                                                                                                                                 |
| GitHub Actions outage                                         | Wait; `release-plz` PR stays open; re-merge when Actions is healthy                                                                                                                                                                                                                                                                                                               |
| `oakoss/homebrew-tap` repo deleted or renamed                 | cargo-dist push fails with 404; recreate repo with the same name; re-run the failed workflow step                                                                                                                                                                                                                                                                                 |
| Binary size regresses >5% vs previous release                 | `cargo bloat` step annotates the workflow run (not a gate). Maintainer reviews; regression may be intentional                                                                                                                                                                                                                                                                     |
| CI runner architecture unavailable (ARM Linux runner outage)  | That platform's binary is missing from the release body; post-outage re-run fills it in                                                                                                                                                                                                                                                                                           |
| First-time tap setup when oakoss/homebrew-tap is empty        | First `cargo-dist` run creates `Formula/linesmith.rb` and pushes; no manual bootstrap needed                                                                                                                                                                                                                                                                                      |

### Rollback

A failed release is rolled back by:

1. Delete the tag locally (`git tag -d v{VERSION}`) and on origin (`git push --delete origin v{VERSION}`).
2. If crates.io was already published, run `cargo yank --vers {VERSION}`.
3. If a Homebrew formula was pushed, revert the commit on `oakoss/homebrew-tap`.
4. If a GitHub Release was created, delete it from the releases page.
5. Fix the underlying issue, push a new tag (`v{VERSION+1}` if crates.io published; otherwise re-use `v{VERSION}`).

Rule: never force-push over an existing tag that made it to crates.io. Users may have `Cargo.lock`ed against it.

## Testing strategy

Follows `AGENTS.md`: workflow changes are tested via dry-run dispatches; pipeline regressions are caught by the dry-run before tag-push.

### Workflow tests

- Dry-run via `workflow_dispatch` on every PR that touches `.github/workflows/*.yml`, `Cargo.toml` (version fields), `release-plz.toml`, or `dist-workspace.toml`
- Matrix-level tests: the cross-compile step runs on every PR (covered by the existing `check` workflow; release.yml reuses the same build matrix)
- Attestation verification: post-release, a `verify.yml` workflow downloads the released binary, runs `gh attestation verify`, asserts PASS

### Manual test plan (first release only)

Before cutting `v0.1.0`, the maintainer should verify:

- All seven target triples build cleanly via the dry-run workflow
- Shell installer installs to `~/.local/bin/` on macOS + Linux
- PowerShell installer installs to `$env:USERPROFILE\.local\bin` on Windows
- Homebrew tap repo receives a formula on dry-run (simulated by pointing to a fork)
- `cargo install linesmith --locked` succeeds from a fresh clone
- `gh attestation verify` returns PASS for the dry-run binary

### Bench regression thresholds

Criterion benchmarks run pre-release (`mise run bench`). A >10% regression against the previous release fails the pre-flight manual check (§Release runbook step 1). The bench suite tracks:

- Full-line render cold-start (target: <20ms)
- Full-line render warm (target: <5ms)
- Per-segment render latency (target: <1ms per segment on a cached DataContext)

## Open questions

- **SigStore adoption.** SigStore's macOS codesigning story is still evolving. If it reaches "linesmith can sign binaries for free without a paid Apple dev ID" before v0.2, switch to signed builds at that point.
- **APT / DEB / Snap / Nix.** Community demand signals; none planned for v0.1. A tap-style approach (third-party repo we publish to) is cheaper than first-party `.deb` maintenance.
- **Windows MSI / Chocolatey.** PowerShell installer covers casual Windows users; MSI installers and Chocolatey packaging are nice-to-haves if demand emerges.
- **Reproducible builds across runners.** Same commit + same `cargo-dist` version + same runner image → byte-identical binaries. Different runner OS versions (Ubuntu 22.04 vs 24.04) may drift. Pin runner versions to reduce this drift.
- **Automated release notes curation.** git-cliff produces a CHANGELOG; the "what's new for humans" blurb at the top of the release body is still manual. A future `release-notes.md` template + GPT-generated draft could speed this up; v0.1 stays manual.
- **Crates.io alternative registries.** For users behind corporate firewalls, a public alternative (like cargo's `--registry` flag) may matter. Defer — users with that constraint tend to know how to work around it.

## Change log

- 2026-04-20: initial draft (v0.1). Defines semver posture (pre-1.0 minor-for-breaking), the 7-target platform matrix (macOS/Linux/Windows × x86_64/aarch64 + linux-musl), four installer methods (shell, PowerShell, Homebrew, cargo install), release-plz/cargo-dist responsibility split, release profile settings (matches ADR-0007), supply-chain posture (OIDC trusted publishing, SLSA attestations, pinned action SHAs, `cargo-auditable`), unsigned-binary posture for v0.1, day-of-release runbook, rollback steps, and edge cases. Closes lsm-9sa under epic lsm-c2i.
