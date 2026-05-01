//! linesmith: Rust status line for Claude Code and other AI coding CLIs.
//!
//! This crate hosts the CLI driver, doctor subcommand, and binary
//! entry point. The render engine, segment system, themes, layout,
//! plugin host, and runtime predicates live in [`linesmith_core`]
//! and [`linesmith_plugin`]; consumers reach those types from the
//! originating crates directly per ADR-0020.
//!
//! End-user binary: `cargo install linesmith`. Programmatic access
//! to the render pipeline is via [`linesmith_core::run`] and the
//! `run_with_*` family — depend on `linesmith-core` (and
//! `linesmith-plugin` if you need plugin-host types) directly.
//! This crate's published library surface is intentionally narrow:
//! `cli`, `doctor`, `cli_main`, and `CliEnv` are the only items
//! the binary itself surfaces beyond the entry point.

pub mod cli;
pub mod doctor;
pub(crate) mod driver;

// Crate-internal re-exports of the linesmith-core surface so
// driver / doctor / cli modules can keep the short
// `crate::config::*`, `crate::data_context::*`, etc. paths inside
// this crate. `pub(crate)` keeps these out of the published lib
// surface — downstream consumers reach `linesmith_core::*` directly
// per consume-from-origin (ADR-0020). Only the modules actually
// used internally are aliased; new internal callers add the
// short alias here as needed.
pub(crate) use linesmith_core::{
    config, data_context, detect_terminal_width, logging, presets, run_lines_with_context, runtime,
    segments, theme, RunContext,
};

pub use driver::{cli_main, CliEnv};
