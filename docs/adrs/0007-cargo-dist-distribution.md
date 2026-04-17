# Use cargo-dist for multi-platform release distribution

- Status: accepted
- Date: 2026-04-17
- Deciders: Jace

## Context and Problem Statement

Part of linesmith's value proposition ([ADR-0001](0001-use-rust-for-runtime.md)) is a single static binary that kills the `npx -y @latest` supply-chain concern. To deliver on that, we need a reliable way to ship pre-compiled binaries across macOS (x86_64 + aarch64), Linux (x86_64 + aarch64, glibc + musl), and Windows (x86_64 + aarch64), with installers that work on every platform. How should we structure the release pipeline?

## Decision Drivers

- Every target platform must have a one-line install command
- Homebrew formula for macOS/Linux users
- Shell installer script (`curl | sh` or equivalent) for general Unix users
- PowerShell installer for Windows
- No manual release steps — releases should be a `git tag` away
- Low maintenance overhead — we're not a release engineering team
- Reproducible builds where possible

## Considered Options

- **`dist` (formerly cargo-dist)** — maintained tool that generates GitHub Actions workflows, builds for all targets, creates releases with installers and formulas
- **Manual GitHub Actions + `cross`** — hand-write build matrices, package releases ourselves
- **Homebrew-only** — ship only via brew formula; ignore non-Homebrew users
- **Cargo install only** — `cargo install linesmith` requires Rust toolchain; loses the single-binary story
- **Hybrid: cargo-dist + cargo install** — cargo-dist for pre-built binaries, `cargo install` as a fallback for Rust users

## Decision Outcome

Chosen option: **`dist` (cargo-dist)**, because it's the industry standard for Rust CLI shipping in 2026, covers every platform we need, generates installer scripts and Homebrew formulas automatically, and integrates cleanly with GitHub Releases. The alternative (hand-written CI) costs us hours per month to maintain with no meaningful differentiation. `cargo install` remains available automatically as a fallback for users with a Rust toolchain.

Release pipeline:

1. Bump version in `Cargo.toml`
2. `git tag vX.Y.Z && git push --tags`
3. `dist` GitHub Actions workflow fires, builds all targets, publishes release
4. Homebrew tap updated automatically (after one-time setup)
5. Users install via `curl | sh`, `brew install oakoss/linesmith/linesmith`, `powershell -c "irm ... | iex"`, or `cargo install linesmith`

Build profile for size (in `Cargo.toml`):

```toml
[profile.release]
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
```

### Consequences

- Good, because every target platform ships from one command
- Good, because Homebrew formula is generated automatically — no separate `homebrew-linesmith` repo to maintain manually
- Good, because release workflow is declarative; we don't write CI
- Good, because `cargo install linesmith` works for users with a Rust toolchain — no extra work
- Good, because release profile settings shave 30-50% off binary size (~3-5MB target)
- Bad, because we're locked into `dist`'s release conventions; deviating means opting out of automation
- Bad, because `dist` version upgrades occasionally require regenerating the GH Actions workflow and reviewing diffs
- Neutral, because GitHub Actions CI minutes are free at our projected release cadence

### Confirmation

Revisit if:

- `dist` becomes unmaintained or its GH Actions workflow pattern breaks
- Binary sizes exceed 10MB despite feature gating and LTO
- A platform emerges (FreeBSD? Redox?) that `dist` doesn't support

## Pros and Cons of the Options

### `dist` (cargo-dist)

- Good: maintained, industry standard, multi-platform out of the box
- Good: generates installers (shell, PowerShell) and Homebrew formulas
- Good: integrates with GitHub Releases and tags
- Bad: opinionated — deviation from defaults loses automation benefits
- Bad: occasional upgrades require manual intervention

### Manual GitHub Actions + `cross`

- Good: full control over build matrix, caching, artifact layout
- Bad: weeks of release engineering to match `dist` baseline
- Bad: ongoing maintenance burden — cross-compilation edge cases change
- Bad: no generated Homebrew formula; must hand-maintain

### Homebrew-only

- Good: one-command install for macOS/Linux
- Bad: abandons Windows users entirely
- Bad: loses the `curl | sh` install path that many users prefer

### Cargo install only

- Good: simplest to ship — no CI work
- Bad: requires Rust toolchain on user machines; defeats the single-binary promise
- Bad: excludes users who don't have Rust installed
- Bad: compile time on user machines (30s-3min) is a poor first impression

### Hybrid: cargo-dist + cargo install

- Good: cargo install works automatically alongside cargo-dist — no conflict
- This is effectively what we're doing — `dist` is primary, `cargo install` is automatic fallback

## More Information

- Driven by: `research/rust-crate-survey.md` (dist recommendation), `research/user-demand.md` (native binary trust as competitive moat)
- Related ADRs: [ADR-0001](0001-use-rust-for-runtime.md) (single-binary promise)
- dist docs: <https://opensource.axo.dev/cargo-dist/>
- Will drive: `specs/release-process.md` (versioning, changelog, tag conventions)
