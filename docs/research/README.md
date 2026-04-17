# Research

Surveys, deep dives, and competitive analysis that inform linesmith's [ADRs](../adrs/) and [specs](../specs/). Research drives decisions here; ADRs cite the research they stand on.

See [`docs/README.md`](../README.md) for the full docs pipeline.

## Index

Sorted newest first. When a research session produces findings that shape a decision, the referenced ADR links back here.

| Date       | Doc                                                               | Summary                                                                                                   |
| ---------- | ----------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| 2026-04-17 | [rust-crate-survey](rust-crate-survey.md)                         | Stack picks for each crate category: JSON, ANSI, git, config, plugins, HTTP, CLI args, release tooling    |
| 2026-04-17 | [cross-tool-statusline-support](cross-tool-statusline-support.md) | Which AI coding CLIs expose a `statusLine` API today (Claude, Qwen) and which are coming (Codex, Copilot) |
| 2026-04-17 | [user-demand](user-demand.md)                                     | Ranked feature requests and top complaints from ccstatusline / claude-code issues and community posts     |
| 2026-04-17 | [competitor-landscape](competitor-landscape.md)                   | Survey of existing Claude Code statusline tools (14+) plus adjacent shell-prompt tools                    |
| 2026-04-17 | [claude-code-statusline-api](claude-code-statusline-api.md)       | Claude Code's statusline JSON contract: invocation model, schema, performance constraints                 |

## Conventions

- **Filenames** are descriptive, lowercase-kebab-case (no date prefix). Git and the `Date:` front-matter field carry chronology.
- **Every research doc** starts from [`0000-template.md`](0000-template.md).
- **Adding a research doc** means adding a row to the table above. Keep the table sorted by date (descending) and the summary to one line.
- **Research outcomes** typically promote to an ADR. When that happens, link the ADR from the research doc's "Implications / actions" section.
