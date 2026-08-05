# Release Process

- Status: draft
- Version: 0.2
- Last updated: 2026-08-05
- Driving ADRs: [ADR-0001](../adrs/0001-use-rust-for-runtime.md), [ADR-0007](../adrs/0007-cargo-dist-distribution.md), [ADR-0019](../adrs/0019-publish-linesmith-core-as-scaffolding-from-v0-1.md), [ADR-0027](../adrs/0027-knope-for-release-automation.md)

## Overview

This spec defines how linesmith versions are cut, built, packaged, signed (or deliberately not), and distributed. The release pipeline is `cargo-dist` (binaries, installers, Homebrew formula) + `Knope` (per-package version bumps, per-crate CHANGELOG, internal dep-pin updates, GitHub Release creation) + `cargo release publish` (crates.io publish, leaf-first ordering, skips already-published versions). It runs as two workflow files across a chain of runs: `knope-release.yml` hosts four jobs — `prepare` opens the release PR when a feature/fix PR merges to main, `release` tags bumped packages when the release PR merges, `publish` pushes to crates.io from a tag-triggered run (one per `<crate>/v<version>` tag, so a multi-crate release starts more than one; it cannot ride the merge run, because crates.io rejects `pull_request_target` — see §Crates.io publishing), and `verify-published` asserts from the merge run that the registry actually caught up. `release.yml` (cargo-dist) fires separately on the `linesmith/v*` tag push to build binaries and upload them to the existing release. A maintainer cuts a release by merging the release PR; everything after is automation.

> **Naming note.** Upstream renamed `cargo-dist` to `dist` (see [ADR-0007](../adrs/0007-cargo-dist-distribution.md)); this spec says `cargo-dist` throughout to match widely-referenced ecosystem documentation. They're the same tool. When the rename lands everywhere in third-party tutorials, the spec will revise.

Target-platform coverage, installer choices, and the single-binary story are decided in [ADR-0007](../adrs/0007-cargo-dist-distribution.md). This spec turns those decisions into concrete version semantics, a platform matrix, workflow step ordering, supply-chain signing posture, and a runbook.

Out of scope: user-facing install instructions (those belong in the README); mirror distribution (e.g., APT, DEB, Snap, Nix) — deferred until v0.2+ with a demand signal; auto-update in-binary (deferred indefinitely; `linesmith doctor` reports newer releases as part of its standard run).

## Requirements

### Functional

- Version tags use semver, per-package format: `<crate>/vMAJOR.MINOR.PATCH` (e.g., `linesmith/v0.2.0`, `linesmith-core/v0.2.0`, `linesmith-plugin/v0.1.3`, `linesmith/v0.2.0-rc.1`). Bare `vMAJOR.MINOR.PATCH` tags (`v0.2.0`) are accepted by `release.yml` only for backward compatibility with the manual v0.2.0 unblock; Knope produces only the per-package form going forward.
- `Knope` manages per-package version bumps + per-crate CHANGELOG entries from Conventional Commits and changeset files
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

Per-package format produced by Knope (ADR-0027):

```text
{crate}/v{MAJOR}.{MINOR}.{PATCH}[-{PRERELEASE}]
```

Examples:

```text
linesmith/v0.1.0           first public release of the binary
linesmith/v0.1.1           bug-fix patch
linesmith-core/v0.2.0      breaking change in the core scaffolding crate
linesmith-plugin/v0.1.3    additive change in the plugin host crate
linesmith/v0.2.0-rc.1      pre-release candidate
linesmith/v1.0.0           first stable public API promise
```

Legacy formats still recognized by `release.yml`'s tag filter for backward compatibility (Knope emits neither):

```text
v0.2.0                     bare workspace tag from the manual v0.2.0 unblock
linesmith-v0.1.3           release-plz's per-package format (pre-Knope)
```

The `doctor` self-update probe normalizes all four forms (`{crate}/v*`, `{crate}-v*`, bare `v*`, plain `*`) when it fetches `/releases/latest`. See `crates/linesmith/src/doctor/snapshot.rs::strip_package_prefix`.

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

Six targets in the v0.1 release, shipped simultaneously from one tag. lsm-9sa scoped six targets; v0.1 added `x86_64-unknown-linux-musl` (Alpine and distroless-container coverage) and dropped `aarch64-pc-windows-msvc` during the v0.1.1 release recovery on a cargo-xwin/cc-rs/ring cross-compile blocker. Net: still six. Windows ARM users install from source via `cargo install linesmith`; lsm-duv8 tracks restoring the prebuilt path.

| Target triple               | Platform      | Install method priority |
| --------------------------- | ------------- | ----------------------- |
| `x86_64-apple-darwin`       | macOS Intel   | brew, shell             |
| `aarch64-apple-darwin`      | macOS ARM     | brew, shell             |
| `x86_64-unknown-linux-gnu`  | Linux glibc   | brew, shell             |
| `x86_64-unknown-linux-musl` | Linux musl    | shell                   |
| `aarch64-unknown-linux-gnu` | Linux ARM     | brew, shell             |
| `x86_64-pc-windows-msvc`    | Windows Intel | powershell              |

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

- Format: Knope's `PrepareRelease` default — `## <version> (<date>)` headers, `### <Section>` subheadings (Features / Fixes / Refactoring / etc.), one bullet per commit with the conventional-commit scope **bolded** and a SHA link. Follows Keep-a-Changelog structure except headers use bare semver (not bracketed `[X.Y.Z]`) so Knope's `parse_title` can find prior sections and insert with proper Markdown spacing. Driven by Conventional Commits + changeset files.
- Generation: `knope-release.yml`'s `prepare` job runs `knope prepare-release` on each non-release PR merge to `main` (filtered by `head.ref != 'release'` on the `pull_request_target: closed` event, plus `workflow_dispatch` with `mode=prepare` as an operator escape hatch). Knope walks every conventional commit since each package's last `<pkg>/v*` tag plus every changeset file under `.changeset/`, computes the appropriate bump per package, writes the new version into each `versioned_files` entry, prepends an entry to the package's CHANGELOG, and deletes consumed changeset files — all into the working tree, uncommitted. `peter-evans/create-pull-request` then commits that through the GitHub API (`sign-commits`, so the commit is signed as the bot and satisfies `required_signatures` on main) and opens or updates the `release` PR. The maintainer reviews and merges.
- Per-package CHANGELOGs:
  - `CHANGELOG.md` (root) — `linesmith` binary
  - `crates/linesmith-core/CHANGELOG.md` — `linesmith-core` scaffolding crate
  - `crates/linesmith-plugin/CHANGELOG.md` — `linesmith-plugin` scaffolding crate
- Commit types Knope treats as bump triggers per default: `feat`, `fix`, `perf`. `docs`/`chore`/`test`/`ci` are visible in git log and PR diffs but don't bump a version and don't appear in the user-facing CHANGELOG. (Knope's `extra_changelog_sections` knob could surface `docs` per-crate if desired; currently unconfigured.)
- Beads footer (bare `lsm-xyz`) lands in the squash-merge commit and is reachable via `git log`. It is NOT preserved in the CHANGELOG (Knope's PrepareRelease consumes only the commit subject + breaking-change flag).
- Scope routing per `knope.toml`'s `[packages.<name>] scopes` field:
  - `feat(core): X` / `fix(core): X` → bumps `linesmith-core` only
  - `feat(plugins): X` → bumps `linesmith-plugin` only
  - `feat(cli|tui|segments|themes|config|doctor): X` → bumps `linesmith` only
  - `feat: X` (no scope) → bumps every package per Knope's "no scope = all packages" semantics
  - `feat(repo|ci|adr|spec|docs|readme|ideas|beads): X` → routed nowhere (intentional: those scopes pair with non-bumping commit types in normal use)
- Breaking changes flagged via `!` in commit subject (e.g., `feat(core)!: rename Segment trait`) hoist to a `### Breaking changes` subsection in the relevant package's CHANGELOG and force a minor bump pre-1.0 / major post-1.0.

Changeset files (`.changeset/*.md`) supplement conventional commits when a single PR's commit subject can't fully express its release impact. The format:

```markdown
---
linesmith-core: minor
---

Rename Segment trait's render() method to compose(). Existing plugin
scripts continue to work via a deprecation shim that emits a
linesmith-warn on load; the shim is removed in linesmith-core 0.3.
```

Run `knope document-change` to scaffold the file interactively. Knope deletes consumed changeset files on the next `PrepareRelease` run.

### Knope + cargo-dist split

Two GitHub Actions workflows are required because Knope creates the release PR + tags + GitHub Releases, then cargo-dist reacts to the binary's tag:

| Concern                             | Owner         | Workflow                            | Fires on                         |
| ----------------------------------- | ------------- | ----------------------------------- | -------------------------------- |
| Per-package version bumps           | Knope         | `knope-release.yml` (`prepare` job) | Non-`release` PR merge to `main` |
| Internal dep-pin updates by name    | Knope         | `knope-release.yml` (`prepare` job) | Same                             |
| Per-crate CHANGELOG entries         | Knope         | `knope-release.yml` (`prepare` job) | Same                             |
| Changeset file consumption          | Knope         | `knope-release.yml` (`prepare` job) | Same                             |
| Release PR open + force-update      | Knope         | `knope-release.yml` (`prepare` job) | Same                             |
| Git tags `<crate>/v<version>`       | Knope         | `knope-release.yml`                 | Release PR merge                 |
| Per-package GitHub Releases         | Knope         | `knope-release.yml`                 | Release PR merge                 |
| `cargo release publish --workspace` | cargo-release | `knope-release.yml`                 | `<crate>/v*` tag push            |
| crates.io publication verified      | sparse index  | `knope-release.yml`                 | Release PR merge                 |
| Multi-platform binary builds        | cargo-dist    | `release.yml`                       | `linesmith/v*` tag push          |
| Upload binaries to existing release | cargo-dist    | `release.yml`                       | Same                             |
| Shell / PowerShell installers       | cargo-dist    | `release.yml`                       | Same                             |
| Homebrew formula update + push      | cargo-dist    | `release.yml`                       | Same                             |
| Build attestations (SLSA)           | cargo-dist    | `release.yml`                       | Same                             |

Knope is the conductor (lives in `knope-release.yml` — four jobs: `prepare`, `release`, `publish`, `verify-published`); cargo-dist is the binary specialist (lives in `release.yml`). The handoff is the `linesmith/v<ver>` git tag: `knope-release.yml` pushes it via `knope release`, that tag push triggers `release.yml`, and the cargo-dist `host` job uploads binaries to the GitHub Release Knope already created (`gh release upload` + `gh release edit --draft=false --latest`).

If `knope release` fails before tagging, `release.yml` never starts and no binaries ship. If `cargo release publish` fails (transient registry error, dependency-ordering issue), it can be re-run via `knope-release.yml`'s `workflow_dispatch` trigger with `mode=publish` (the default `mode=release` would attempt to re-tag) — the tagged release on GitHub stays as-is; `cargo-release` skips already-published versions on retry.

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

Three crates published per ADR-0019: `linesmith` (binary), `linesmith-core` (scaffolding), `linesmith-plugin` (scaffolding). Published via OIDC trusted publishing in leaf-first order:

- One-time: configure `oakoss/linesmith` as a trusted publisher in each crate's settings page on crates.io. The trusted-publisher entries name `knope-release.yml` as the workflow and leave the environment field empty — no GitHub Environment is provisioned for this repo, so the workflow ID alone is the identity crates.io accepts.
- Per-release: `knope-release.yml`'s `publish` job mints a short-lived crates.io token via `rust-lang/crates-io-auth-action` (consumes the workflow's OIDC token) and runs `cargo release publish --workspace --execute --no-confirm`. `cargo-release` checks each member against crates.io and skips crates already at their target version — load-bearing for single-package release cycles where only `linesmith-core` or `linesmith-plugin` bumped (bare `cargo publish --workspace` errors on the unchanged siblings since cargo has no `--skip-existing` flag).
- No `CARGO_REGISTRY_TOKEN` secret in the repo.

The `publish` job runs on the `push` of the `<crate>/v<ver>` tags the `release` job creates, **not** in the `pull_request_target` run that merged the release PR. GitHub still mints an OIDC JWT on every trigger; what changed is that crates.io refuses to **exchange** it for a publish token on `pull_request_target` and `workflow_run`, returning HTTP 400 ([rust-lang/crates.io#12219](https://github.com/rust-lang/crates.io/pull/12219), 2025-10-29). Both triggers run in the target repository's security context while being triggerable from forks, which is a privilege-escalation vector. Those two are the entire denylist — every other event name passes; the `push` / `release` / `workflow_dispatch` trio quoted in crates.io's error message is a suggestion, not an allowlist. When this breaks, the 400 comes from crates.io, so read the auth step's log rather than hunting a GitHub OIDC misconfiguration.

The trusted-publisher entry is unaffected by the split. crates.io matches on repository, repository owner ID, workflow filename, and environment; the ref is not part of the match at all, and the trigger is enforced by the separate rejection above rather than by the entry. So a tag-triggered run of `knope-release.yml` satisfies the same entry.

Because a tag trigger is weaker than the old "merged release PR" gate, the job re-establishes that property itself: `Verify tag is an ancestor of main` refuses to publish a tag pointing at a commit that never landed on `main`.

A separate `verify-published` job asserts the registry matches what `main` says it should hold. Two scoping notes: it asserts the manifest versions on `main` are live and unyanked, not that this particular run shipped anything — a run where Knope found nothing releasable goes green because the manifests already match. And it does not cover a hand-pushed `<crate>/v*` tag, which triggers `publish` without a merge run to verify it. In detail: it reads every publishable member's version from the manifests on `main` — not from the tag names, so a tag disagreeing with what was built is caught — and polls the crates.io sparse index for up to 20 minutes, failing with a recovery command if any version is still absent. It is a sibling of `publish`, not a step inside it, and rides the `pull_request_target` merge run instead of the tag run. That placement is the whole point: every way the tag-triggered publish run can fail to start at all would take a step inside it down too, and those are precisely the failures that produced a green pipeline over a stale registry for two months.

`publish` sits in its own `knope-publish` concurrency group rather than the `knope-release` group shared by `prepare` and `release`. A concurrency group retains only one pending run: a newly queued run cancels whatever was already pending. On a shared group, a `prepare` run from an unrelated merge could therefore evict a pending publish, and the release would never reach crates.io. Within the dedicated group, eviction is harmless when it happens at all: a multi-crate release starts one publish run per tag, and only at three or more tags does the surplus get cancelled — with two, one runs and one pends. Because Knope bumps every package in a single commit, each run checks out the same full set of versions and publishes the whole workspace, so whichever survives does the work. `docs/ops/release-runbook.md` gives the per-crate-count expectations.

Knope creates each tag through the GitHub Releases API (one `POST /releases` per package) rather than pushing them with git, so tags arrive as independent single-ref events. Two consequences follow. API-authored events created with an App installation token do start workflow runs, whereas a `GITHUB_TOKEN` push deliberately does not. And GitHub's rule that no tag push events are created when more than three tags land _at once_ can never be triggered, at any crate count. If the release path ever moves to local `git tag` + `git push --tags`, that ceiling becomes live and caps the workspace at three publishable crates.

Each crate's crates.io description, README snippet, and keywords are sourced from its `Cargo.toml`'s `[package]` metadata (the library crates ship their own descriptions disclaiming SemVer stability, per ADR-0019). License: `MIT`.

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

### Repo metadata

One-time GitHub metadata for topic search visibility and landing page copy. Run once per repo (e.g. when forking for a sibling project):

```sh
gh repo edit oakoss/linesmith \
  --description "Status line for Claude Code and other AI coding CLIs — Rust, plugin API, role-based themes." \
  --homepage "https://docs.rs/linesmith" \
  --add-topic claude-code,statusline,rust,ai-coding,claude,cli,anthropic,terminal,prompt-tool
```

Topic conventions: hyphen-separated lowercase, ≤50 chars, max 20 per repo (GitHub limit). Topics above cover: tool family (`claude-code`, `claude`, `anthropic`), category (`statusline`, `cli`, `terminal`, `prompt-tool`), language (`rust`), and broader domain (`ai-coding`). Homepage points at `docs.rs/linesmith`, which resolves after the first `cargo publish`; forks should omit `--homepage` until then.

## Behavior

### Release runbook

Day-of-release steps for a maintainer:

1. **Pre-flight checks** (local):
   - `mise run check` — all tests + lints pass
   - `mise run bench` — no >10% regression against the previous release's benches
   - `cargo audit` — no new advisories
   - `linesmith doctor --plain` on a clean checkout — passes
2. **Review the Knope release PR** that's open against `main` (branch: `release`, label: usually none — Knope doesn't set one by default). Verify each package's bump shape is correct (a `feat(core)` commit should bump only `linesmith-core`; an unscoped `feat:` bumps all three) and each per-package CHANGELOG entry reads correctly.
3. **Merge the release PR** with the squash strategy (the repo's only enabled merge method). On merge, `knope-release.yml` fires and:
   1. Runs `knope release` — tags each bumped package as `<crate>/v<version>` and creates a per-package GitHub Release with CHANGELOG-derived notes.
   2. Runs `cargo release publish --workspace --execute --no-confirm --allow-branch 'main,HEAD'` via OIDC trusted publishing — publishes bumped crates in leaf-first order, skipping any whose version is already on crates.io. Both allowlist entries are load-bearing: the `push` path checks out `refs/tags/<crate>/v<version>`, so `actions/checkout` detaches HEAD and cargo-release sees the branch as `HEAD`; the `workflow_dispatch` path passes the literal `'main'` ref name, so the checkout lands on the `main` branch.
4. **Watch the cargo-dist workflow.** The `linesmith/v<version>` tag push fires `release.yml`, which:
   1. Runs the `plan` job to compute the 6-target matrix.
   2. Cross-compiles the binary on each target with `fail-fast: false`.
   3. Generates shell/PowerShell installers and aggregate `sha256.sum`.
   4. Uploads binaries + installers to the existing `linesmith/v<version>` GitHub Release Knope created (`gh release upload`), then marks it `--draft=false --latest` (`gh release edit`).
   5. Pushes the Homebrew formula to `oakoss/homebrew-tap` (skipped on prereleases).
   6. Generates SLSA attestations for every artifact via `actions/attest-build-provenance`.
5. **Verify artifacts** (10-minute budget):
   - Pull the macOS `aarch64-apple-darwin` binary from the `linesmith/v<version>` release, run `linesmith --version`, confirm the version string matches the tag.
   - Run `linesmith doctor --plain` on the downloaded binary, confirm all-PASS.
   - Verify `brew install oakoss/tap/linesmith` on a fresh macOS VM (or container).
   - Verify `cargo install linesmith` from a fresh Rust install.
6. **Announce.** The GitHub Release body is auto-populated by Knope (CHANGELOG-derived) + cargo-dist (install snippets + download table). Optionally add a narrative post to the README "Release Log" section.

If step 3 or 4 fails partway through, see §Edge cases §Rollback. `knope-release.yml`'s `workflow_dispatch` trigger lets you re-run the pipeline via the `mode` input — `mode=release` (default) reruns tag + crates.io publish; `mode=publish` skips tagging and reruns crates.io only (recovery when tags already exist); `mode=prepare` reruns the release-PR open/update step. `verify-published` runs alongside the `release` and `publish` modes rather than having a mode of its own.

### Workflow structure

Two workflow files, chained via release-PR merge + git-tag events:

**`.github/workflows/knope-release.yml`** — single combined workflow. `prepare` and `release` are mutually exclusive, gated on the merged PR's head ref under `pull_request_target: closed`; `publish` runs off the tags `release` creates, under `push: tags` (see §Crates.io publishing for why it cannot ride the merge run); `verify-published` rides the merge run and asserts the registry caught up. `workflow_dispatch` remains the operator escape hatch for all three modes. Combined into one file in 2026-05 to eliminate a race where two split-file workflows could both fire on the release-PR squash, with the prepare path reading tag state before the release path had created the new tag (see Change log entry):

Triggers: `pull_request_target: closed` on `main`, `push` on `*/v<X.Y.Z>` tags, and `workflow_dispatch` with a `mode` of `prepare` | `release` | `publish` (default `release`).

| Job                | Runs when                                                            | Does                                                                                                                                                                                                                                                                                                     |
| ------------------ | -------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `prepare`          | any merged PR whose head **isn't** our own `release`; `mode=prepare` | Mints the App token, runs `knope prepare-release` (stages bumps, dep-pin rewrites, CHANGELOG entries, deletes consumed changesets), then `peter-evans/create-pull-request` with `sign-commits` opens or updates the release PR. A follow-up step fails the run if the resulting commits aren't verified. |
| `release`          | merged PR whose head **is** our own `release`; `mode=release`        | `knope release` tags each bumped package `<crate>/v<version>` and creates per-package GitHub Releases from the CHANGELOG.                                                                                                                                                                                |
| `publish`          | `*/v<X.Y.Z>` tag push; `mode=release` or `mode=publish`              | Mints an OIDC crates.io token, then `cargo release publish --workspace` (skips already-published members, leaf-first).                                                                                                                                                                                   |
| `verify-published` | same as `release`, plus `mode=publish`                               | Polls the crates.io sparse index until every publishable member's manifest version is live and unyanked, or its deadline passes.                                                                                                                                                                         |

Job conditions, `needs` edges, concurrency groups, and permissions live in the workflow file rather than being mirrored here — this section previously carried verbatim copies and they went stale twice in one day, each time needing a reviewer to catch it. The reasoning behind the non-obvious ones is in the workflow's own comments, and the properties that matter to the contract are below.

The `head.repo.full_name == github.repository` guard on the release job is load-bearing under `pull_request_target`: a fork can name its source branch `release`, so the `head.ref == 'release'` check alone would match a coincidentally-named fork branch and mint the bot's token to tag commits the maintainer didn't intend to release. (`publish` no longer needs the guard — it runs off tags, and `Verify tag is an ancestor of main` enforces the equivalent property.) The prepare job intentionally omits this guard so fork PRs CAN drive release prep (the bot still has to merge the resulting release PR — human review remains in the loop).

`prepare` and `release` share the `knope-release` concurrency group with `cancel-in-progress: false` so a fix/feat PR merging while a release PR is mid-tagging queues its prepare run behind the in-flight release-job, preventing prepare from reading stale tag state. `publish` deliberately sits outside that group — see §Crates.io publishing. `verify-published` has no group at all: it only reads, and parking it in `knope-release` would block prepare runs for the length of its poll.

**`.github/workflows/release.yml`** — runs on `linesmith/v*` tag push (from `knope-release.yml`), via `workflow_dispatch` for manual dry-runs, and via path-filtered `pull_request:` for auto-validation of release-infra changes (per [ADR-0017](../adrs/0017-release-workflow-pr-validation.md)):

```text
on:
  push:
    tags:
      - 'linesmith/v[0-9]+.[0-9]+.[0-9]+*'
      - 'v[0-9]+.[0-9]+.[0-9]+*'           # legacy v* tags from the v0.2.0 unblock
  workflow_dispatch:
  pull_request:
    paths:
      - '.github/workflows/release.yml'
      - 'dist-workspace.toml'
      - 'Cargo.toml'
      - 'crates/*/Cargo.toml'
      - 'Cargo.lock'

jobs:
  plan:
    # Runs `dist plan` to validate the release matrix before building.
  build-local-artifacts:
    needs: plan
    strategy:
      matrix: <6 target triples per dist-workspace.toml>
      fail-fast: false   # lets successful legs finish even on sibling failure
    # Cross-compiles per target + emits SLSA attestation in-step.
  build-global-artifacts:
    needs: build-local-artifacts
    # Generates shell/PowerShell installers + aggregate sha256.sum.
  host:
    needs: [plan, build-local-artifacts, build-global-artifacts]
    # Upload to existing release (Knope created it on release-PR merge):
    #   gh release upload "$TAG" artifacts/*
    #   gh release edit "$TAG" --draft=false $PRERELEASE_FLAG $LATEST_FLAG
    # Falls back to `gh release create` for bare `v*` tags (legacy path).
  publish-homebrew-formula:
    needs: [plan, host]
    # Pushes Formula/linesmith.rb to oakoss/homebrew-tap (skipped on prereleases).
```

The `linesmith/v<version>` tag push from `knope-release.yml` is the coupling point with cargo-dist. If `knope release` fails before tagging, `release.yml` never starts and no binaries ship. If `cargo release publish` (in `knope-release.yml`'s `publish` job) fails, binaries can still ship for the linesmith tag — the partial state is acceptable because `cargo install linesmith` from crates.io is a separate distribution channel from the cargo-dist GitHub Release artifacts (`curl | sh`, brew, powershell). Re-run via `workflow_dispatch`; `cargo-release` skips crates already published.

### Pre-release workflow

Pre-release tags (`linesmith/vX.Y.Z-rc.N`, `-alpha.N`, `-beta.N`) skip Homebrew formula pushes and skip the `--latest` flag on the GitHub Release edit. They do **not** publish to crates.io: `knope-release.yml`'s publish trigger matches exact `<pkg>/vX.Y.Z` tags only, deliberately excluding prerelease suffixes because a crates.io publish is irreversible. Cargo would resolve them if published (`linesmith = "0.2.0-rc.1"` matches; bare `linesmith = "0.2"` skips it), but RC validation here is binaries-only. Publishing an RC requires a manual `cargo publish`.

Cutting a pre-release without going through the normal Knope flow: tag the commit manually as `git tag -a linesmith/v0.2.0-rc.1 -m "v0.2.0-rc.1" && git push origin linesmith/v0.2.0-rc.1`. cargo-dist's `release.yml` picks up the tag, builds binaries, and creates a GitHub Release (since Knope didn't create one). The `host` job's fallback path (`gh release create` when `gh release view` 404s) handles this case.

Testers install via:

```sh
curl -LsSf https://github.com/oakoss/linesmith/releases/download/linesmith/v0.2.0-rc.1/linesmith-installer.sh | sh
```

`cargo install linesmith --git https://github.com/oakoss/linesmith --tag linesmith/v0.2.0-rc.1` also works for Rust-toolchain pre-release testers.

### Dry-run workflow

`release.yml` exposes two dry-run paths beyond tag-push (per [ADR-0017](../adrs/0017-release-workflow-pr-validation.md)):

1. **`workflow_dispatch`** — maintainer-triggered from any branch. Use for ad-hoc validation when you want the full matrix before cutting a tag.
2. **`pull_request` (paths-filtered)** — auto-fires when a PR touches release-infra files (`release.yml`, `dist-workspace.toml`, `Cargo.toml`, `crates/*/Cargo.toml`, `Cargo.lock`). Catches cross-compile breakage from cargo-update / Knope dep-pin updates / dependabot before merge. SHA is the PR head, not the merge SHA. Filter excludes `knope.toml` and `knope-release.yml` (false coverage — release.yml's matrix doesn't run Knope's code paths). Concurrency cancellation: superseded PR runs cancel automatically.

Either path:

- Synthesizes an ephemeral tag string (`v0.0.0-dry-run-<short-sha>`) and threads it to `dist build` for artifact naming. Nothing is pushed to git.
- Skips the GitHub Release creation (the `host` job is gated on `publishing == 'true'`, set by the `plan` job to `github.event_name == 'push'`).
- Skips the Homebrew formula push (`publish-homebrew-formula` is gated on the same `publishing` flag for defense-in-depth).
- Uploads binary artifacts as GitHub Actions workflow artifacts with 7-day retention; real releases keep the 90-day default so maintainers can re-download post-release without re-building.
- Still generates SLSA build attestations on each target leg, except on fork PRs where `id-token: write` is clamped — Attest skips gracefully via `if: github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository`.

**Validation surface caveats.** Triggering on `dist-workspace.toml` exercises only the keys cargo-dist reads at runtime (target list, build profile, attestations toggle). Keys that affect generated CI shape — `[dist.github-action-commits]`, runner customizations, the trigger surface itself — only take effect after `dist init` regen rewrites `release.yml`, and under `allow-dirty = ["ci"]` regen skips the file entirely. CI-shape changes require regenerating `release.yml` in the same PR to actually exercise.

Dry-run runs touch only GitHub Actions artifact storage — no crates.io version slot consumed, no GitHub Release created, no Homebrew formula pushed.

cargo-dist 0.31's `--allow-dirty` flag is boolean and covers only "CI scripts out of date" — already permanently allowed via `dist-workspace.toml`'s `allow-dirty = ["ci"]`, so no per-dispatch toggle is needed. Source-tree-dirty dispatch isn't a concept in 0.31; CI checkouts are always clean. `knope-release.yml`'s `cargo release publish` step has no dry-run path either, so pipeline-validating crates.io changes still require a local `cargo release publish --dry-run` or a real `linesmith/v0.0.x` patch release. Knope itself exposes `knope prepare-release --dry-run` (locally) which writes the version bumps + CHANGELOG entries to the working tree without committing — useful for previewing what the next release PR would contain.

## Edge cases

| Case                                                          | Handling                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| ------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `cargo release publish` fails (transient 5xx)                 | Re-run `knope-release.yml` via `workflow_dispatch`. `cargo-release` skips already-published versions; failed crate retries on the same OIDC token. Note: cargo-dist already ran (binaries are live for the linesmith tag) — accept the brief asymmetry between crates.io and the GitHub Release until publish succeeds                                                                                                                                                                                                                                                                                                                                                                                       |
| `cargo-dist` fails on one target (e.g., ARM Linux OOM)        | `host` blocks because it `needs: build-local-artifacts` (whole matrix). With `fail-fast: false`, successful legs finish and upload their binaries as workflow artifacts. Re-run the failed leg from the Actions UI, which resumes the chain and lets `host` finish. The release on GitHub stays in draft state until every target completes                                                                                                                                                                                                                                                                                                                                                                  |
| Crates.io publish succeeds but `cargo-dist` fails entirely    | Partial release: `cargo install linesmith` works but curl/brew/powershell installers have no artifacts. Re-run `release.yml` via `workflow_dispatch` against the `linesmith/v<ver>` tag; do NOT yank crates.io. Pin a tracker issue noting the asymmetry until binaries catch up                                                                                                                                                                                                                                                                                                                                                                                                                             |
| Homebrew formula push fails (auth, rate-limit, repo conflict) | Binary + shell-installer releases succeed; brew users see "formula not found"; formula re-pushed via manual re-run                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `knope release` fails on tag push (e.g., tag already exists)  | Workflow exits 1; manual cleanup: delete the partially-created tag on origin (`git push --delete origin <pkg>/v<ver>`; needs org-ruleset bypass — see [`docs/ops/release-runbook.md`](../ops/release-runbook.md) §Tag protection), fix the underlying issue, re-run `knope-release.yml` via `workflow_dispatch`                                                                                                                                                                                                                                                                                                                                                                                              |
| Tag pushed without a release PR                               | Possible if a maintainer pushes `linesmith/v*` by hand (pre-release path). cargo-dist's `host` job's fallback (`gh release view` → `gh release create`) covers this — Knope didn't create a release, so cargo-dist creates one with auto-extracted CHANGELOG body                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| Need to yank a release from crates.io                         | `cargo yank --vers X.Y.Z -p <crate>` locally; no workflow involved; GitHub Release stays but add a "⚠ Yanked" note in the release body                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| Critical CVE mid-release-cycle                                | Cut a new patch release (`linesmith/v0.1.N+1`) with the fix; don't try to edit the existing release's artifacts                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| Secret leak (e.g., OIDC config)                               | Rotate immediately via crates.io + GitHub settings; new releases use rotated identity; old tags stay as-is                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| Contributor pushes a tag without permission                   | Requires repo write access. `Verify tag is an ancestor of main` confines published content to reviewed `main` history, and a tag on a pre-change commit is inert (that commit's workflow file has no `push:` trigger, so no run is created). Residual gap is timing, not content: a write-access actor can force an unscheduled publish of versions already on `main` but not yet on crates.io. Closed 2026-08-05 (`lsm-3rui`): the `Release Tags - Org` ruleset now enforces `creation` over the per-package tag globs, so only the `oakoss` App and org admins can create a release tag. See [`docs/ops/release-runbook.md`](../ops/release-runbook.md) §Tag protection for the patterns and bypass actors |
| Accidental `linesmith/v1.0.0` tag before we're ready          | Delete the tag both locally and on origin; force-push is not needed (tag isn't a branch); `release.yml` is re-idempotent on retag (host job's `gh release view` check returns the existing release)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| GitHub Actions outage                                         | Wait; Knope release PR stays open; re-merge when Actions is healthy                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `oakoss/homebrew-tap` repo deleted or renamed                 | cargo-dist push fails with 404; recreate repo with the same name; re-run the failed workflow step                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| Binary size regresses >5% vs previous release                 | `cargo bloat` step annotates the workflow run (not a gate). Maintainer reviews; regression may be intentional                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| CI runner architecture unavailable (ARM Linux runner outage)  | That platform's binary is missing from the release body; post-outage re-run fills it in                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| First-time tap setup when oakoss/homebrew-tap is empty        | First `cargo-dist` run creates `Formula/linesmith.rb` and pushes; no manual bootstrap needed                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |

### Rollback

A failed release is rolled back by:

1. Delete the affected tags locally and on origin. Per-package tags require per-package deletion: `git tag -d linesmith/v{VERSION} && git push --delete origin linesmith/v{VERSION}` (repeat for each bumped package's tag if Knope created multiple).
2. If crates.io was already published, run `cargo yank --vers {VERSION} -p <crate>` per affected crate.
3. If a Homebrew formula was pushed, revert the commit on `oakoss/homebrew-tap`.
4. If GitHub Releases were created, delete each from the releases page (per-package release pages exist when Knope creates more than one in the same cycle).
5. Fix the underlying issue, then either re-run `knope-release.yml` via `workflow_dispatch` (preferred, idempotent against existing crates.io versions) or cut a fresh patch release (`linesmith/v{VERSION+1}` if crates.io published; re-use `linesmith/v{VERSION}` otherwise).

Rule: never force-push over an existing tag that made it to crates.io. Users may have `Cargo.lock`ed against it.

## Testing strategy

Follows `AGENTS.md`: workflow changes are tested via dry-run dispatches; pipeline regressions are caught by the dry-run before tag-push.

### Workflow tests

- Auto-triggered dry-run on every PR that touches `release.yml`, `dist-workspace.toml`, `Cargo.toml`, `crates/*/Cargo.toml`, or `Cargo.lock` (paths-filtered `pull_request:` trigger per ADR-0017)
- Manual `workflow_dispatch` dry-run for PRs that touch other release-adjacent files not in the auto-trigger (`knope.toml`, `knope-release.yml`, `ci.yml`, `audit.yml`, `codeql.yml`, etc.) — Knope's config files are intentionally excluded from auto-trigger because `release.yml`'s matrix doesn't run Knope's code paths (false coverage)
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
- **Automated release notes curation.** Knope produces per-package CHANGELOGs from conventional commits + changeset files; the "what's new for humans" narrative blurb at the top of the release body is still manual. A future `release-notes.md` template + GPT-generated draft could speed this up; v0.2 stays manual.
- **Crates.io alternative registries.** For users behind corporate firewalls, a public alternative (like cargo's `--registry` flag) may matter. Defer — users with that constraint tend to know how to work around it.

## Change log

- 2026-04-19: initial draft (v0.1). Defines semver posture (pre-1.0 minor-for-breaking), the 7-target platform matrix (macOS/Linux/Windows × x86_64/aarch64 + linux-musl), four installer methods (shell, PowerShell, Homebrew, cargo install), release-plz/cargo-dist responsibility split, release profile settings (matches ADR-0007), supply-chain posture (OIDC trusted publishing, SLSA attestations, pinned action SHAs, `cargo-auditable`), unsigned-binary posture for v0.1, day-of-release runbook, rollback steps, and edge cases. Closes lsm-9sa under epic lsm-c2i.
- 2026-04-19: license changed from `MIT OR Apache-2.0` to `MIT` to match the repo `LICENSE` swap (MPL-2.0 → MIT). Closes lsm-c2c.
- 2026-08-05: Close the unprivileged-tag-creation gap (`lsm-3rui`). Since the crates.io publish moved onto a tag trigger, creating a release tag became a privileged operation, but the `Release Tags - Org` ruleset scoped only to `refs/tags/v*` and `refs/tags/@*` and carried no `creation` rule — so any actor with `contents: write` could create `linesmith/v9.9.9` and force an unscheduled publish of versions sitting on `main` but not yet on crates.io. The ruleset now includes `refs/tags/*/v*` (Knope's per-package format) and `refs/tags/*-v*` (the legacy release-plz format, previously unprotected against deletion too) and enforces `creation`, with bypass limited to the `oakoss` App and org admins. Verified by pushing `zz-ruleset-test/vzzz` — matching `refs/tags/*/v*` but neither workflow's trigger — and reading the rule-suite evaluation, which recorded `result=bypass`; the tag was then deleted from local and origin, and no workflow ran. That establishes the ruleset evaluates the ref and that `creation` would reject a non-bypassed actor. It does NOT establish that the `oakoss` App's bypass works in the release flow, which needs a real release to demonstrate (`lsm-d2a9.1`). The trade-off is that the runbook's manual tag operations — RC cutting, rollback deletion, migration re-push — are now admin-only; see §Tag protection in the runbook. No ADR change.
- 2026-08-04: Add the `verify-published` job (`lsm-q0iu`). Nothing in the pipeline asserted that crates.io received a release, so every failure mode looked identical from the outside: green checks, tags created, GitHub Release marked "Latest", registry unchanged. That is how `lsm-9wrv` hid from 2026-06-06 to 2026-08-04. The job reads each publishable member's version from the manifests on `main` and polls the crates.io sparse index for up to 20 minutes, failing with the `mode=publish` recovery command if any is missing. A version present but yanked is reported separately and fails immediately rather than polling: the index lists yanked versions forever, no amount of waiting reverses a yank, and `mode=publish` cannot recover one because crates.io rejects a version it already holds — the fix is `cargo yank --undo` or a new release. Sourcing versions from manifests rather than tag names also catches a tag that disagrees with what was built. It is a sibling of `publish` on the `pull_request_target` merge run rather than a step inside `publish`, because publish executes in a separate tag-triggered run and every way that run can fail to start would take an inner step down with it. No ADR change — ADR-0027's decision (use Knope) stands; only the Actions wiring is refined.
- 2026-08-04: Replace Knope's `publish-release-pr` workflow with `peter-evans/create-pull-request` using `sign-commits` (`lsm-g5xz`). Knope's only mechanism for the release commit was shell `git commit` + `git push --force`, and commits pushed over git are unsigned whatever the token, so `required_signatures` on main left the release PR mergeable only by bypassing the rule — PR #22 sat unmergeable from 2026-06-06 to 2026-08-04 for exactly this reason. The action commits through the GitHub API, which signs as the token's App identity (`oakoss[bot]`); its REST `createCommit` call deliberately sends no `author`/`committer`, since GitHub only signs for a bot when the request carries no custom identity. That is also why the `Configure Git` step is gone: it set a fabricated `linesmith-bot[bot]` identity matching no real account. The action builds blobs and tree objects rather than using the `createCommitOnBranch` mutation, so file modes and deletions survive. It is idempotent and silently no-ops when nothing is releasable, which retires the `git diff --quiet` gate; a follow-up step asserts `pull-request-commits-verified`, scoped to runs that created or updated the branch because an untouched branch reports `false` from a local `%G?` check that signed commits can never satisfy. Supersedes [ADR-0027](../adrs/0027-knope-for-release-automation.md)'s description of the `publish-release-pr` workflow and its rationale for bare `--force` over `--force-with-lease`. No ADR change — ADR-0027's decision (use Knope) stands; only the Actions wiring is refined.
- 2026-08-04: Move the `publish` job off the `pull_request_target` run onto a `push: tags` trigger (`lsm-9wrv`). crates.io began rejecting OIDC token exchanges from `pull_request_target` and `workflow_run` on 2025-10-29 ([rust-lang/crates.io#12219](https://github.com/rust-lang/crates.io/pull/12219)) — both are triggerable from forks while running in the target repo's security context. This workflow was written 2026-05-24, after that block, so crates.io publishing never once succeeded through a release-PR merge; the failure was invisible because the `release` job still tagged and created a GitHub Release marked "Latest". Discovered when `linesmith-core` 0.3.0 shipped to GitHub but not to the registry, and recovered with a `mode=publish` dispatch. Publish also moved to its own `knope-publish` concurrency group: a group holds one running plus one pending, so on the shared group an unrelated `prepare` run could evict a pending publish and silently drop the release. Added `Verify tag is an ancestor of main` to replace the provenance the merged-PR gate used to provide, and tightened the tag glob to exact versions so hand-cut RC tags cannot trigger an irreversible publish. No ADR change — ADR-0027's decision (use Knope) stands; only the Actions wiring is refined, consistent with the 2026-05-26 entry's framing.
- 2026-05-26: Combine `knope-prepare.yml` and `knope-release.yml` into a single `knope-release.yml` with three mutually-exclusive jobs (`prepare`, `release`, `publish`) gated on the merged PR's head ref (`lsm-aqdk`). The 2026-05-23 split caused a race when a release PR merged: the squash-merge emitted both a `push: main` event (which fired `knope-prepare.yml`) and a `pull_request: closed` event (which fired `knope-release.yml`), and `knope-prepare` won the queue ahead of `knope-release` creating the new `<pkg>/v*` tag, so prepare's tag-walk saw the previous release as the latest tag and re-claimed the just-released commits into a phantom follow-on release PR (hit during the 0.2.1 ship). Combined shape eliminates the race architecturally — one workflow file, `pull_request_target: closed` trigger, jobs differentiated by `head.ref == 'release'` vs `head.ref != 'release'`. `pull_request_target` (not plain `pull_request`) so the workflow can mint the bot's GitHub App token on merged fork PRs — `pull_request` strips secrets when the PR head is on a fork, which the old `push: main` trigger sidestepped because `push` events run post-merge with secrets available. Safe to use `pull_request_target` here because we only react to `closed + merged == true` (maintainer-approved merge) and check out the base branch (main), never the PR head — no untrusted-contributor code runs with elevated perms. The `workflow_dispatch` input changed from boolean `skip_release` to a `mode` choice (`prepare` | `release` | `publish`); operator escape hatches for all three jobs explicitly accessible. crates.io Trusted Publisher allowlist unaffected (filename `knope-release.yml` retained). No ADR change — ADR-0027's decision (use Knope) stands; only the GitHub Actions wiring is refined.
- 2026-05-23: v0.2 — Knope migration per ADR-0027. Replaces release-plz with `knope-prepare.yml` (release PR creation) + `knope-release.yml` (tag, GitHub Release, `cargo release publish --workspace`). Adds changeset workflow (`knope document-change`). Per-package tag format `<crate>/v<version>` replaces the bare `v<version>` workspace tag and the release-plz `<crate>-v<version>` per-package form. `release.yml` hand-edits: tag filter widened to include both formats during the transition; host job's GitHub Release creation replaced with `gh release upload --clobber` + `gh release edit --draft=false --latest` to attach to the Knope-created release. `cliff.toml` retired (Knope generates per-crate CHANGELOGs internally). Doctor self-update parse fix lands alongside (handles `<pkg>/v*` and `<pkg>-v*` prefix forms). Closes lsm-llij.
