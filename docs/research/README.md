# Research

Surveys, deep dives, and competitive analysis that inform linesmith's [ADRs](../adrs/) and [specs](../specs/). Research drives decisions here; ADRs cite the research they stand on.

See [`docs/README.md`](../README.md) for the full docs pipeline.

## Index

Sorted newest first. When a research session produces findings that shape a decision, the referenced ADR links back here.

| Date       | Doc                                                               | Summary                                                                                                       |
| ---------- | ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| 2026-05-13 | [layout-decision-observability](layout-decision-observability.md) | Per-segment layout-decision channel: typed `LayoutObservers` callback + `LineItem.id` (vs tracing / trait id) |
| 2026-04-18 | [json-parsing-stack](json-parsing-stack.md)                       | Parser + cache serialization due-diligence: serde_json wins; partial structs > parser swap; cache stays JSON  |
| 2026-04-18 | [data-fetching-strategy](data-fetching-strategy.md)               | Per-source cost matrix, mtime caching, JSONL incremental tail, OAuth cache stack, segment-driven lazy load    |
| 2026-04-18 | [claude-data-files](claude-data-files.md)                         | Complete map of CC's persistent state: settings cascade, ~/.claude.json, sessions/, Keychain, oauthAccount    |
| 2026-04-18 | [ccometixline-rust-patterns](ccometixline-rust-patterns.md)       | Rust-peer cross-check: CCometixLine uses same OAuth endpoint; patterns to adopt (ureq, cache) + avoid (npm)   |
| 2026-04-18 | [ccstatusline-widget-internals](ccstatusline-widget-internals.md) | ccstatusline's rate-limit widgets, OAuth endpoint (`/api/oauth/usage`), cache strategy, effort detection      |
| 2026-04-18 | [cc-info-commands](cc-info-commands.md)                           | Claude Code built-in info slash commands (`/usage`, `/stats`, `/config`, ...) and where each sources data     |
| 2026-04-18 | [jsonl-data-source](jsonl-data-source.md)                         | Claude Code JSONL transcript schema, 5h block aggregation, ccstatusline widget catalog (superseded in part)   |
| 2026-04-17 | [rust-crate-survey](rust-crate-survey.md)                         | Stack picks for each crate category: JSON, ANSI, git, config, plugins, HTTP, CLI args, release tooling        |
| 2026-04-17 | [cross-tool-statusline-support](cross-tool-statusline-support.md) | Which AI coding CLIs expose a `statusLine` API today (Claude, Qwen) and which are coming (Codex, Copilot)     |
| 2026-04-17 | [user-demand](user-demand.md)                                     | Ranked feature requests and top complaints from ccstatusline / claude-code issues and community posts         |
| 2026-04-17 | [competitor-landscape](competitor-landscape.md)                   | Survey of existing Claude Code statusline tools (14+) plus adjacent shell-prompt tools                        |
| 2026-04-17 | [claude-code-statusline-api](claude-code-statusline-api.md)       | Claude Code's statusline JSON contract: invocation model, schema, performance constraints                     |

## Conventions

- **Filenames** are descriptive, lowercase-kebab-case (no date prefix). Git and the `Date:` front-matter field carry chronology.
- **Every research doc** starts from [`0000-template.md`](0000-template.md).
- **Adding a research doc** means adding a row to the table above. Keep the table sorted by date (descending) and the summary to one line.
- **Research outcomes** typically promote to an ADR. When that happens, link the ADR from the research doc's "Implications / actions" section.
