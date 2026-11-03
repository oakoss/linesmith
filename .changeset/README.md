# Bump files

This directory is used by [oakum](https://github.com/oakoss/oakum) to manage versions and changelogs.

A **bump file** is a small Markdown file recording one change: which packages it affects, how far each should bump, and what to tell users about it. Files accumulate here as work lands, and are consumed when a release is cut.

## How it works

1. You make a change and commit a bump file alongside it, usually one per pull request.
2. Bump files accumulate on the main branch.
3. At release time oakum merges them into a plan, works out which other packages need releasing, updates versions and changelogs, and deletes the files it consumed.

## Creating one

`oakum add` writes one file. Flags:

| Flag                | Effect                                                                                                                                  |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `--packages <list>` | Comma-separated `name:level` pairs (`core:minor,utils:patch`)                                                                           |
| `--message <text>`  | Changelog note body                                                                                                                     |
| `--name <slug>`     | Filename stem, slugified                                                                                                                |
| `--interactive`     | Guided prompts (needs a terminal)                                                                                                       |
| `--empty`           | Empty frontmatter (intentionally releaseless)                                                                                           |
| `--none`            | `name: none` coverage. Requires `--packages` with `name:none` pairs                                                                     |
| `--section <name>`  | Keep a Changelog section for the note (`added`, `changed`, `deprecated`, `removed`, `fixed`, `security`); the level picks one otherwise |

A flagless `oakum add` exits non-zero and names `--packages`, `--empty`, `--none`, and `--interactive`. `--interactive` without a terminal tells you to use `--packages` instead.

```bash
oakum add --packages "my-package:minor" --message "What changed and what you do differently."
```

```bash
oakum add --empty --message "docs only"
oakum add --none --packages "my-package:none" --message "covered without a release"
```

Or write the file yourself; hand-written files work the same:

```markdown
---
my-package: minor
---

Bump files can be written by hand.
```

Bare package names are **unquoted**, and oakum refuses a quoted one, naming the file. knope treats a quoted name as a package that does not exist and skips the file without reporting anything, so the unquoted form is the one every reader agrees on. `@changesets/cli` writes the quoted form; `oakum migrate` rewrites those as it adopts them, so a cutover needs no hand-editing.

Scoped npm names are the exception and must be quoted, because `@` starts a reserved token in YAML:

```markdown
---
'@scope/my-package': minor
---
```

## What you do not write

**You never write a bump file for a package that merely depends on what you changed.** Oakum works that out from the dependency graph, including whether the dependent actually needs releasing at all. A library whose published range still covers the new version usually does not; a binary that bakes its dependencies in at build time always does.

Dependents that will release appear in `oakum status`. If you find yourself adding a bump file because something downstream also needs releasing, that is a bug in oakum. Paste the status table and report it.

## Choosing a level

- **patch** — a fix; behavior that was wrong is now right
- **minor** — something new that does not break existing usage
- **major** — a change that breaks existing usage

Only these three, plus `none` for coverage without a direct bump. For an application rather than a library there is no compatibility contract behind them, but the choice still selects which SemVer component advances, and thus the version and tag.

## Changes that ship no release

Under `oakum check --strict` (CI's mode), a pull request that changes a package must carry a bump file even when nothing releases, unless every commit on the branch that reaches the package releases nothing (`refactor:`, `test:`, `chore:`, `docs:`, …); without `--strict`, the gap is reported and the run still passes.

When you need a file, for example to cover packages under a strict coverage gate without releasing them, use the same shapes as bumpy and changesets:

- Empty frontmatter (`---` then `---` with no package lines) for an intentionally releaseless change
- `package: none` for a package that takes no direct bump but still accepts a cascade

`oakum add` writes these with `--empty` / `--none`.

## Keeping them current

A bump file is part of the change, like a test. If a pull request grows from a fix into a feature, update the level and the summary to match before merging. Otherwise the release understates what shipped, and the changelog misleads the person deciding whether to upgrade.

Reviewers and agents should read the bump file as part of reviewing the diff.

## Writing the summary

The summary becomes the changelog entry, read by someone deciding whether to upgrade. Say what changed and what they do differently. "Fixed a bug" tells them nothing; "A bump file naming an unknown package now reports the file path instead of aborting the run" tells them whether it affects them.

Markdown works. A sentence or two suits most changes.

The level picks the changelog section (`patch` under Fixed, `minor` under Added, `major` under Changed). When that is wrong, say so: a summary whose first line is one of Keep a Changelog's headings (`### Added`, `### Changed`, `### Deprecated`, `### Removed`, `### Fixed`, `### Security`) goes under that heading, and the line itself is dropped. `oakum add --section` writes it.

## Files in this directory

- `_config.toml` — oakum's configuration
- `_schema.json` — generated; validates the config in your editor
- `*.md` — pending bump files

Everything ending in `.md` here is treated as a bump file, so notes to yourself and scratch files belong somewhere else. Anything without a `.md` extension is ignored.

Four names are skipped: this `README.md` (any case), and `AGENTS.md`, `CLAUDE.md`, and `GEMINI.md` (exact names). A lowercase `agents.md` is parsed as a bump file. If an agent writes notes in this directory, give them any other name.

## Checking your work

```bash
oakum check
```

Reports tag drift, packages that changed with no covering bump file, a `CHANGELOG.md` that `oakum version` would refuse to append to (one still titled with the package name from changesets), and a `.<name>.oakum-write.*` oakum staging file left in this directory by a write that did not finish. It writes nothing. Until an install pin exists in `.github/workflows`, `.github/actions`, `package.json`, `.mise.toml`, `mise.toml`, or a Cargo workspace member named `oakum`, it reports `unverified` instead. `oakum init` prints a workflow; it does not write the pin.

On a pinned repository whose tags match the manifests, whose bump files parse, and whose changed packages are covered, it prints nothing and exits 0.

A malformed bump file fails it, named with its parse error. `--strict` also fails when a changed package has no covering intent; the printed workflow runs it that way.

The next-release table is `oakum status`.

Full documentation: <https://github.com/oakoss/oakum/tree/main/docs>
