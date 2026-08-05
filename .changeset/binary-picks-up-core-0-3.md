---
linesmith: major
---

**Not a breaking change — the binary picks up seven features already shipped in `linesmith-core` 0.3.0.**

The CLI surface, the config schema, and the `linesmith` library API are all unchanged. The heading above reflects the version position, not the content: pre-1.0, `0.x+1.0` is where additive releases go per [release-process.md](docs/specs/release-process.md) §Semver posture, and Knope files that position under "Breaking Changes" with no way to distinguish the two cases.

`linesmith` 0.2.1 was published against `linesmith-core ^0.2.0`, so everything merged as `feat(core)` since then has been live on crates.io as a library while remaining unreachable to anyone running `cargo install linesmith`. Those scopes bump `linesmith-core` alone and only refresh the binary's dependency pin, so even the binary-crate source those PRs touched went unreleased: the commit scope, not the files touched, decides which package bumps. The version number stayed 0.2.1 while its dependency graph changed underneath it.

Now reaching users: per-segment `icon` property and icons mode (#21), multi-span segments and the inline `context_bar` percentage, brackets, and dim trough (#23), the unified progress-bar renderer with rate-limit threshold coloring (#24), `git_branch` dirty counts with per-category colors (#25), `Role::Timer` (#27), the `group` color-grouping flag (#29), and the group-lead coloring render layer (#30).

The only binary-crate production change in this release is #21's `config.icons_mode` doctor check ("Icon font guidance"); everything else arrives through the refreshed `linesmith-core` pin.
