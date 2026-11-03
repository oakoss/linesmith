# Use oakum for release automation, replacing Knope

- Status: accepted
- Date: 2026-11-02
- Deciders: Jace Babin
- Supersedes: [ADR-0027](0027-knope-for-release-automation.md) (Knope) and [ADR-0032](0032-core-bumps-cascade-to-the-binary.md) (cascading a library bump by sharing commit scopes). ADR-0032's requirement stands: a library bump must reach the binary. Its mechanism does not.

## Context and Problem Statement

Knope ([ADR-0027](0027-knope-for-release-automation.md)) routes a change to a package by commit scope. A scope list is a hand-kept copy of the dependency graph, and it rotted: `feat(core)` releases bumped `linesmith-core` and only refreshed the binary's pin, so the binary stayed at 0.2.1 while its dependency graph changed underneath it (the 0.3.0 changelog note). [ADR-0032](0032-core-bumps-cascade-to-the-binary.md) patched that by repeating library scopes on the binary package, which is the same copy kept in one more place.

[oakum](https://github.com/oakoss/oakum), built by the same maintainer, reads the dependency graph instead: a library bump releases every package that ships it. It also gates pull requests on coverage (`oakum check --strict` fails a changed crate nothing covers), which Knope does not.

Should linesmith move its release automation to oakum?

## Decision Drivers

- A library bump reaches the binary users install without a hand-kept scope map (ADR-0032's requirement)
- A pull request that changes a crate cannot silently skip a release
- Per-package versioning, per-crate CHANGELOGs and dependency-pin rewrites stay (ADR-0019, ADR-0027's drivers)
- The supply-chain posture stays: OIDC trusted publishing, pinned action SHAs, GitHub App writes, signed commits on main
- cargo-dist keeps binary builds, Homebrew and attestations (ADR-0007)

## Considered Options

- **Stay on Knope** with ADR-0032's scope map
- **oakum with change files and conventional commits both on** (chosen)
- **oakum with conventional commits only**

## Decision Outcome

Chosen option: **oakum with change files and conventional commits both on**, because it derives the binary cascade from the graph and gates coverage on every pull request, while the levels come from bump files an author writes, which is how linesmith's 0.3.0, 0.4.0 and the `linesmith-core` 0.3.0 releases were driven.

Measured before the switch by replaying linesmith's five Knope releases with oakum 0.9.0 (`oakum status` at each pre-release commit, against what Knope shipped):

| Release | Knope shipped         | oakum, both modes                       | oakum, commits only                     |
| ------- | --------------------- | --------------------------------------- | --------------------------------------- |
| #15     | binary 0.2.1          | nothing (no bump files)                 | binary and core 0.3.0 (`ci(release)!:`) |
| #22     | core 0.3.0            | core 0.3.0, **binary 0.2.2 by cascade** | core and binary 0.3.0                   |
| #41     | binary 0.3.0          | same                                    | same                                    |
| #44     | core and binary 0.4.0 | same                                    | binary 0.3.1 by cascade                 |
| #47     | core and binary 0.4.1 | nothing (no bump files)                 | same                                    |

Release #22 is the one Knope got wrong and #41 repaired by hand; oakum ships the binary at once. #15 and #47 were commit-driven, so under the chosen mode each would have carried a bump file, which `oakum generate` writes from the branch's commits.

### Implementation

- `.changeset/_config.toml`: `change-files = true`, `conventional-commits = true`, `versioning = "zero-major"`, `tag-format = "{{ package }}/v{{ version }}"` (the shape Knope tagged, which `release.yml` and `linesmith doctor` read).
- `.github/workflows/oakum.yml` replaces `knope-release.yml`. A push to main runs `oakum release` (tag each untagged manifest version, create its GitHub Release), then `oakum ci version-pr` (open or refresh `oakum/version-packages`), then `verify-published`. A `<crate>/vX.Y.Z` tag push runs `publish`; a dispatch naming a tag recovers one. `publish` and `verify-published` keep Knope's pipeline logic; their triggers change, the `mode` dispatch input is gone (`release` and `version` recover by rerun), and `verify-published` reads the pushed commit rather than `main`.
- `ci.yml` runs `oakum check --strict` in Static Analysis and `oakum ci pr-status` in a non-gating Release plan job.
- `release.yml`'s `host` job keeps oakum's changelog body above cargo-dist's install notes.
- The binary's CHANGELOG moves to `crates/linesmith/CHANGELOG.md`, beside its manifest, since oakum writes each package's changelog in its own directory.
- `linesmith/v0.1.0` and `linesmith/v0.1.1` name the commits the bare `v0.1.x` tags released, so oakum can tell which package they belong to; the bare tags stay.

### Consequences

- Good, because a library bump releases the binary that ships it without any configuration.
- Good, because CI refuses a pull request whose changed crate nothing covers, before merge rather than at release time.
- Good, because `oakum status`, `oakum check` and the pull-request comment show the plan locally and on every pull request.
- Bad, because a cascaded binary takes a patch. A library `feat` users see in the binary needs `linesmith: minor` named in the bump file too.
- Bad, because every release-worthy pull request carries a bump file, where Knope read a commit-only change. `oakum generate` writes one.
- Neutral, because the version pull request lives on `oakum/version-packages` rather than `release`, and its commit is GitHub's signed squash merge.
- Neutral, because the crates.io trusted-publisher entries name `oakum.yml` instead of `knope-release.yml`.

### Confirmation

Confirmed when the first release after the switch tags, publishes to crates.io and ships cargo-dist's binaries with `verify-published` green. Revisit if the cascade level for the binary becomes a recurring correction in bump files.

## More Information

- Shadow-run notes: oakoss/oakum `okm-pvm`.
- [ADR-0019](0019-publish-linesmith-core-as-scaffolding-from-v0-1.md): per-package versioning.
- [ADR-0007](0007-cargo-dist-distribution.md): cargo-dist owns binary distribution.
