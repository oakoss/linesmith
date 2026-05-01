//! Rhai plugin runtime for user-authored segments.
//!
//! Plugins are `.rhai` files discovered at startup (per
//! `docs/specs/plugin-api.md`), compiled once, wrapped in a
//! `RhaiSegment` adapter, and registered alongside built-ins. This
//! module root re-exports [`build_engine`](engine::build_engine) —
//! the constructor for the shared `Arc<Engine>` every plugin invokes
//! `call_fn` on — and [`PluginError`](errors::PluginError), the
//! load-time + render-time failure surface.

pub mod ctx_mirror;
pub mod discovery;
pub mod engine;
pub mod errors;
pub mod header;
pub mod output;
pub mod registry;
pub mod segment;

pub use ctx_mirror::build_ctx;
pub use discovery::scan_plugin_dirs;
pub use engine::build_engine;
pub use errors::{CollisionWinner, PluginError, ResourceLimit};
pub use header::{parse_data_deps_header, HeaderError};
pub use output::validate_return;
pub use registry::{CompiledPlugin, PluginRegistry};
pub use segment::RhaiSegment;
