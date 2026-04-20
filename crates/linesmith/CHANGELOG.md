# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/oakoss/linesmith/releases/tag/v0.1.0) - 2026-04-20

### Added

- *(core)* linesmith presets list + presets apply
- *(themes)* style-string parser + per-segment style overrides
- *(themes)* user theme TOML loading + themes list subcommand
- *(themes)* Catppuccin Latte/Frappé/Macchiato/Mocha flavors
- *(config)* warn on unknown keys in config.toml
- *(config)* color policy precedence + claude_padding width factor
- *(themes)* role-based theme engine with default + minimal built-ins
- *(core)* load config.toml + CLI flags + per-segment overrides
- *(core)* add layout engine with priority truncation + width hints
- *(core)* add rate_limit / cost / effort segments + chrono dep
- *(core)* expand StatusContext to v0.2 + add model and context_window segments
- *(core)* scaffold Rust workspace with workspace-segment slice

### Fixed

- *(segments)* strip control chars from RenderedSegment text

### Other

- *(repo)* switch license to MIT and add crates.io metadata
- *(core)* drop beads-ID from driver test-section comment
- *(core)* extract cli driver + segment builder from lib.rs
- *(core)* extract cli_main so main.rs orchestration is testable
- *(segments)* return RenderResult so render errors propagate distinctly
- *(segments)* encapsulate RenderedSegment + add SegmentDefaults builders
- de-slopify prose across ADRs, research, and top-level docs
- *(repo)* bootstrap project foundation
