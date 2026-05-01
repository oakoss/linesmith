//! Runtime plugin discovery + load. Wraps
//! `PluginRegistry::load_with_xdg` plus the cold-start fast path.

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::Config;
use crate::data_context::xdg::{resolve_subdir, XdgEnv, XdgScope};
use crate::plugins::{build_engine, PluginRegistry};
use crate::segments::BUILT_IN_SEGMENT_IDS;

/// `$XDG_CONFIG_HOME/linesmith/segments/` (with `$HOME` fallback)
/// per the cascade in [`crate::data_context::xdg::resolve_subdir`].
/// `None` when neither env var is populated.
#[must_use]
pub(crate) fn xdg_segments_dir(env: &XdgEnv) -> Option<PathBuf> {
    resolve_subdir(env, XdgScope::Config, "segments")
}

/// Discover + compile plugins from `cfg.plugin_dirs` plus the XDG
/// segments dir. Returns `None` when neither `cfg.plugin_dirs` is
/// configured nor an XDG segments directory exists on disk —
/// cold-start fast path that skips `build_engine` entirely.
/// (Configured-but-missing entries in `cfg.plugin_dirs` take the
/// `Some` path and surface as load errors on the registry.)
/// Callers handle `registry.load_errors()` however they want: the
/// driver writes them to stderr. Doctor currently discards them;
/// folding them into the Plugins-category snapshot is tracked as
/// follow-up work alongside the equivalent gap for themes.
#[must_use]
pub(crate) fn load_plugins(
    cfg: Option<&Config>,
    xdg_env: &XdgEnv,
) -> Option<(PluginRegistry, Arc<rhai::Engine>)> {
    let config_dirs: &[PathBuf] = cfg.map_or(&[], |c| c.plugin_dirs.as_slice());
    let xdg_dir = xdg_segments_dir(xdg_env);

    // Cold-start fast path: no plugin source means no engine cost.
    let xdg_present = xdg_dir.as_deref().is_some_and(|p| p.is_dir());
    if config_dirs.is_empty() && !xdg_present {
        return None;
    }

    let engine = build_engine();
    let registry = PluginRegistry::load_with_xdg(
        config_dirs,
        xdg_dir.as_deref(),
        &engine,
        BUILT_IN_SEGMENT_IDS,
    );
    Some((registry, engine))
}
