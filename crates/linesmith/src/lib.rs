//! linesmith: Rust status line for Claude Code and other AI coding CLIs.
//!
//! This crate hosts the CLI driver, doctor subcommand, and binary
//! entry point. The render engine, segment system, themes, layout,
//! plugin host, and runtime predicates live in [`linesmith_core`];
//! this crate re-exports them so existing call sites that reach
//! `linesmith::config::*` etc. keep working through the v0.1
//! workspace split (per ADR-0018).
//!
//! End-user binary: `cargo install linesmith`. Programmatic access
//! to the render pipeline is via [`linesmith_core::run`] and the
//! `run_with_*` family.

pub mod cli;
pub mod doctor;
pub(crate) mod driver;

// Re-export everything from core so existing internal call sites
// (`crate::config::*`, `crate::data_context::*`, etc.) and external
// embedders (the `gen-config-schema` bin, future docs.rs viewers,
// downstream consumers of `linesmith` the published crate) keep
// resolving without churn.
pub use linesmith_core::{
    build_default_segments, build_lines, build_segments, config, data_context,
    detect_terminal_width, input, layout, logging, lsm_debug, lsm_error, lsm_warn, plugins,
    presets, run, run_lines_with_context, run_with_context, run_with_segments_and_width,
    run_with_width, runtime, segments, theme, RunContext,
};

pub use driver::{cli_main, CliEnv};
