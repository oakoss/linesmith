# Cascade library bumps to the binary by sharing commit scopes

- Status: accepted
- Date: 2026-08-17
- Deciders: Jace
- Amends: [ADR-0027](0027-knope-for-release-automation.md) — the §Implementation clause "`scopes` restrict commit routing: `core` → `linesmith-core`, `plugins` → `linesmith-plugin`". Those scopes now route to the binary as well. ADR-0027's choice of Knope, its workflow layout, and the rest of its configuration stand unchanged.

## Context and Problem Statement

`knope.toml` maps each commit scope to exactly one package: `core` bumps `linesmith-core`, `plugins` bumps `linesmith-plugin`, and `cli` / `tui` / `segments` / `themes` / `config` / `doctor` bump `linesmith`. A library bump rewrites the binary's dependency pin but never bumps the binary itself.

linesmith's users install a binary. Nothing on crates.io reaches them — releases arrive through Homebrew, the shell and PowerShell installers, and the GitHub release, all of which are built by cargo-dist from the `linesmith/v*` tag. No binary bump means no tag, no cargo-dist run, and no delivery.

So a user-facing fix scoped to `core` publishes to the registry and reaches nobody. This is not hypothetical: it happened across seven merges (`lsm-w5xm`), and it recurred with the 403 classification fix. Dry-running the release that contains that fix shows the binary's changelog in full:

```text
## 0.4.1 (2026-08-18)

### Fixes

- drop unexpanded lefthook placeholders before calling beads (#46)
```

The 403 fix changes what users see in their status line — `[Forbidden]` where it used to say `[Network error]` — and it appears nowhere in the release notes of the artifact they install. That release ships a binary at all only because an unrelated `fix(config)` happened to bump it.

Nothing detects this. `main` and crates.io agree on the binary's version number while carrying different dependency graphs, so `verify-published` passes by its own contract.

How should a library bump reach the binary?

## Decision Drivers

- A user-visible fix must reach the artifact users install
- The binary's changelog should describe what changed in the binary
- Failure should not depend on maintainer vigilance at release time
- Knope is the release authority; a solution it can express beats one bolted beside it

## Considered Options

- **Cascade via shared scopes** — list a library's scope on every crate that pins it, not just on the library itself
- **Detect the drift** — extend `verify-published` to fail when `main`'s dep pin differs from what the published binary was built against
- **Re-scope by convention** — document that user-facing work is scoped to the binary even when the code lands in a library

## Decision Outcome

Chosen option: **cascade via shared scopes**, because the costs are asymmetric. Over-releasing spends CI minutes and a patch number. Under-releasing ships a fix that reaches nobody while every signal reports success — which has already happened eight times and is invisible by construction.

Knope has no native cascade: its `dependency` field rewrites a pin and nothing more, and `[packages.*]` exposes no key for inter-package relationships. But a scope may appear in more than one package, and knope then assigns the commit to both. Verified by dry run: adding `core` and `plugins` to the binary's `scopes` puts the 403 fix in **both** changelogs and bumps both crates.

The rule this follows is "every crate whose manifest a bump rewrites must itself bump", not "libraries bump the binary". `linesmith-plugin` has two consumers: `linesmith-core` hosts the plugin bridge and re-exports its types per [ADR-0020](0020-keep-cli-as-linesmith-bridge-in-core.md), and `linesmith` also pins it directly. So `plugins` joins the scopes of both. Omitting the core edge would be worse than the status quo rather than merely incomplete: today a plugin bump rewrites both pins and neither crate bumps, so nothing publishes and the released graph stays self-consistent. Bumping only the binary would publish a binary depending on plugin `0.2.x` alongside a still-published core depending on `^0.1.3`, which do not unify across a pre-1.0 minor. Cargo would resolve two copies, and `PluginRegistry` — which crosses from the plugin crate into `driver.rs` — would be two different types.

Detection was rejected as the primary fix because it converts a silent failure into a loud one without delivering the release — the maintainer still has to force a bump by hand, and the check itself becomes something to maintain. It remains worth adding later as a backstop.

Re-scoping by convention was rejected as the weakest guarantee: it asks every future commit to be labelled against where its effect is felt rather than where its code lives, which is both unnatural and unenforceable. `fix(core)` is the honest scope for a fix in `linesmith-core`.

### Consequences

- Good, because a user-visible library fix now ships in the artifact users install
- Good, because the binary's changelog lists the changes that are actually in the binary
- Good, because it is three entries in existing configuration, not new machinery to maintain
- Bad, because an internal-only `fix(core)` now cuts a binary release nobody needed — cargo-dist builds every configured target and pushes a Homebrew formula for it
- Bad, because severity travels with the commit, not just its presence: a `fix(core)!` breaking the library's Rust API also breaks the binary's version, even when the CLI is unchanged (confirmed, see §Confirmation). A changeset naming the binary's level overrides this, and `AGENTS.md` §Releases already prescribes changesets for exactly the case where the commit subject misstates per-package impact
- Neutral, because the commit appears in both changelogs; each artifact's notes are complete on their own, which is what a reader of either one wants

### Confirmation

Confirmed when a release whose only user-facing change is scoped to a library produces a `linesmith/v*` tag and a Homebrew formula update.

Three dry-run results this rests on. A shared scope puts the 403 fix in both changelogs and bumps both crates. A changeset naming `linesmith-core: major` takes it to 0.5.0 while leaving the binary at 0.4.1 — the override the severity consequence depends on, and evidence that changesets bypass scope routing rather than exercising it.

Severity propagation was confirmed in a throwaway two-crate workspace, since it needs a real `!` commit. With the library's scope listed on both packages, a `fix(lib)!` takes both from 0.4.0 to 0.5.0; with the scope on the library alone, only the library moves and the binary is untouched. So a breaking change to a library's Rust API does break the binary's version even when nothing user-facing changed.

Revisit if internal-only library churn makes binary releases noisy enough to matter — the fallback is this ADR's detection option, which trades delivery guarantees for quieter version numbers.

## More Information

- Raised by `lsm-w5xm`, decided under `lsm-8dae`
- Supersedes `knope.toml`'s own comment, which stated that `core` and `plugins` "route to their own crates and only refresh the dep-pin here", and the scope-to-package table in `AGENTS.md` §Commit Style
- Release mechanics this depends on: [release-process.md](../specs/release-process.md) §Distribution channels — Homebrew, shell, and PowerShell installers all derive from the `linesmith/v*` tag
