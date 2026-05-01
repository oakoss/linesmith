//! Rhai plugin runtime for user-authored segments.
//!
//! Plugins are `.rhai` files discovered at startup (per
//! `docs/specs/plugin-api.md`), compiled once, wrapped in the
//! [`RhaiSegment`] adapter, and registered alongside built-ins.
//!
//! The rhai-pure host (engine construction, discovery, registry,
//! `@data_deps` header parsing, error types) lives in the
//! [`linesmith_plugin`] crate per ADR-0018; this module re-exports
//! that surface and adds the bridge layer that adapts plugin output
//! to the in-crate `Segment` trait — `RhaiSegment` itself, the
//! [`build_ctx`] `DataContext` → `rhai::Map` mirror, and
//! [`validate_return`] which decodes a plugin's `rhai::Map` into a
//! [`crate::segments::RenderedSegment`].

pub mod ctx_mirror;
pub mod output;
pub mod segment;

pub use ctx_mirror::build_ctx;
pub use linesmith_plugin::{
    parse_data_deps_header, scan_plugin_dirs, CollisionWinner, CompiledPlugin, CompiledPluginParts,
    HeaderError, PluginError, PluginRegistry, ResourceLimit,
};
pub use output::validate_return;
pub use segment::RhaiSegment;

/// Re-export the plugin-host submodules other than `engine`. Modules
/// now live in `linesmith-plugin`; this re-export keeps the
/// `crate::plugins::{errors,registry,header,discovery}::*` paths
/// used by `doctor` and the bridge layer resolving without churn at
/// every call site.
pub use linesmith_plugin::{discovery, errors, header, registry};

/// Selective re-export of `linesmith_plugin::engine`'s items —
/// deliberately omits `build_engine` so the `crate::plugins::engine`
/// path can't bypass [`crate::plugins::build_engine`]'s warn-emitter
/// install. Callers wanting the bare host constructor must reach
/// `linesmith_plugin::build_engine` directly, making the bypass
/// explicit. Any future engine-module item the bridge needs gets
/// added here.
pub mod engine {
    pub use linesmith_plugin::engine::{
        current_plugin_id_snapshot, install_warn_emitter, is_deadline_abort,
        render_deadline_snapshot, set_current_plugin_id, set_render_deadline,
        DEFAULT_RENDER_DEADLINE_MS, MAX_ARRAY_SIZE, MAX_EXPR_DEPTH, MAX_MAP_SIZE, MAX_STRING_SIZE,
    };
}

/// Build the rhai plugin engine, wiring linesmith-core's logger as
/// the host's warn emitter so plugin `log()` output respects
/// `LINESMITH_LOG`. Wraps [`linesmith_plugin::build_engine`] —
/// every entry point that goes through `linesmith-core` (CLI,
/// library `run`/`run_with_*`, doctor, `runtime::plugins::load_plugins`,
/// or direct API consumers using `crate::plugins::*`) installs the
/// emitter before the first render.
///
/// Direct consumers of `linesmith_plugin::build_engine` skip this
/// bridge by design — that's the documented entry point for
/// embedders who don't want linesmith-core's logger.
#[must_use]
pub fn build_engine() -> std::sync::Arc<linesmith_plugin::rhai::Engine> {
    install_plugin_warn_emitter();
    linesmith_plugin::build_engine()
}

/// One-shot install that bridges plugin `log()` output through
/// `crate::logging::emit` so `LINESMITH_LOG` gates plugin
/// diagnostics. Called from [`build_engine`] (the linesmith-core
/// plugin facade) so every entry point that builds an engine via
/// this crate picks up the bridge before the first render.
fn install_plugin_warn_emitter() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        engine::install_warn_emitter(Box::new(|msg| {
            crate::logging::emit(crate::logging::Level::Warn, msg);
        }));
    });
}
