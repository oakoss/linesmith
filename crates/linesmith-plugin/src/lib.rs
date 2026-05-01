//! linesmith-plugin: rhai plugin host for the linesmith status line.
//!
//! Construction (`build_engine`) and load-time machinery (discovery,
//! script compilation, `@data_deps` header parsing, id-collision
//! detection) live here. The bridge that adapts a [`CompiledPlugin`]
//! to linesmith-core's `Segment` trait — and the `DataContext` →
//! `rhai::Map` mirror — lives in `linesmith-core::plugins` so this
//! crate's public surface stays free of linesmith-domain types.
//!
//! Surface contract per ADR-0018: declared deps cross this boundary
//! as `Vec<String>` (raw header tokens, validated against the
//! plugin-accessible name set defined by `header::KNOWN_DEPS` per
//! `docs/specs/plugin-api.md`); the consumer maps strings back to
//! its own dep enum. `pub use rhai;` lets consumers reach
//! `rhai::Map` / `Engine` / `Dynamic` via `linesmith_plugin::rhai::*`
//! without adding rhai as a direct dep — workspace alignment is
//! still the source of truth for which rhai version compiles together.
//!
//! Workspace-internal in v0.1; public surface is free to refactor
//! without SemVer cost until a publish decision lands.

pub mod discovery;
pub mod engine;
pub mod errors;
pub mod header;
pub mod registry;

pub use discovery::scan_plugin_dirs;
pub use engine::build_engine;
pub use errors::{CollisionWinner, PluginError, ResourceLimit};
pub use header::{parse_data_deps_header, HeaderError};
pub use registry::{CompiledPlugin, CompiledPluginParts, PluginRegistry};

/// Re-export rhai so consumers can name `rhai::Map` / `Engine` /
/// `Dynamic` via `linesmith_plugin::rhai::*` without adding rhai as
/// a direct dep. Workspace alignment, not this re-export, is the
/// version pin.
pub use rhai;
