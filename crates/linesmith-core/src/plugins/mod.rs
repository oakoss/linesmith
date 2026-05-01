//! Plugin runtime bridge layer.
//!
//! The rhai-pure plugin host (engine construction, discovery,
//! registry, `@data_deps` header parsing, error types) lives in
//! [`linesmith_plugin`] per ADR-0018 / ADR-0020 — consumers reach
//! those types from that crate directly, not through this module.
//! What lives here is the consumer-side bridge: the
//! [`RhaiSegment`] adapter that lets compiled plugins implement the
//! `Segment` trait, the [`build_ctx`] `DataContext` → `rhai::Map`
//! mirror, [`validate_return`] which decodes a plugin's `rhai::Map`
//! into a [`crate::segments::RenderedSegment`], and the
//! [`build_engine`] wrapper that installs a `LINESMITH_LOG`-respecting
//! warn emitter before delegating to [`linesmith_plugin::build_engine`].

pub mod ctx_mirror;
pub mod output;
pub mod segment;

pub use ctx_mirror::build_ctx;
pub use output::validate_return;
pub use segment::RhaiSegment;

/// Build the rhai plugin engine with linesmith-core's logger wired
/// as the host's warn emitter, so plugin `log()` output respects
/// `LINESMITH_LOG`. Wraps [`linesmith_plugin::build_engine`].
///
/// Every entry point that builds a plugin engine via linesmith-core
/// (CLI driver, library `run` / `run_with_*` family, doctor,
/// `runtime::plugins::load_plugins`) installs the emitter before
/// the first render. Direct consumers of
/// [`linesmith_plugin::build_engine`] skip this bridge by design —
/// that's the documented entry point for embedders who don't want
/// linesmith-core's logger.
#[must_use]
pub fn build_engine() -> std::sync::Arc<linesmith_plugin::rhai::Engine> {
    install_plugin_warn_emitter();
    // The wrapper IS the documented bypass-target; calling the bare
    // constructor here is the one legitimate use of the disallowed
    // method (every other consumer should reach for this wrapper).
    #[allow(clippy::disallowed_methods)]
    linesmith_plugin::build_engine()
}

/// One-shot install that bridges plugin `log()` output through
/// `crate::logging::emit` so `LINESMITH_LOG` gates plugin
/// diagnostics. Called from [`build_engine`] so every entry point
/// that builds an engine via this crate picks up the bridge before
/// the first render.
fn install_plugin_warn_emitter() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        linesmith_plugin::engine::install_warn_emitter(Box::new(|msg| {
            crate::logging::emit(crate::logging::Level::Warn, msg);
        }));
    });
}
