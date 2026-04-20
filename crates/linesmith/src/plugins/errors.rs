//! [`PluginError`] covers every failure mode described in
//! `docs/specs/plugin-api.md` §Edge cases.
//!
//! Load-time errors (`Compile`, `UnknownDataDep`, `MalformedDataDeps`,
//! `IdCollision`) are collected in a `Vec<PluginError>` by the registry
//! and surfaced via `linesmith doctor`. Runtime errors (`Runtime`,
//! `ResourceExceeded`, `Timeout`, `MalformedReturn`) drop the plugin
//! segment for one render invocation and log once to stderr.

use std::path::PathBuf;

/// Which of the configured rhai resource ceilings tripped. One-to-one
/// with the `MAX_*` constants in [`crate::plugins::engine`]; a typed
/// enum here (rather than `&'static str`) keeps [`PluginError::ResourceExceeded`]
/// and `linesmith doctor` output typo-proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceLimit {
    MaxOperations,
    MaxCallLevels,
    MaxExprDepth,
    MaxStringSize,
    MaxArraySize,
    MaxMapSize,
}

impl ResourceLimit {
    /// Stable string form for logs + doctor output.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MaxOperations => "max_operations",
            Self::MaxCallLevels => "max_call_levels",
            Self::MaxExprDepth => "max_expr_depth",
            Self::MaxStringSize => "max_string_size",
            Self::MaxArraySize => "max_array_size",
            Self::MaxMapSize => "max_map_size",
        }
    }
}

impl std::fmt::Display for ResourceLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Every failure mode a plugin can hit at load time or render time.
/// Variants match `plugin-api.md` §Edge cases; error copy is aimed at
/// the person reading `linesmith doctor` output, not the plugin
/// author's script.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginError {
    /// Rhai script failed to parse or compile at load time.
    Compile { path: PathBuf, message: String },

    /// Script runtime error during `render(ctx)` — any thrown rhai
    /// error that wasn't a resource-limit hit. Drops the segment for
    /// this invocation; logged once.
    Runtime { id: String, message: String },

    /// Script exceeded a configured rhai resource limit per
    /// `plugin-api.md` §Resource ceilings. Drops the segment; logged.
    ResourceExceeded { id: String, limit: ResourceLimit },

    /// Host-side wallclock timer fired before the script returned
    /// (default 50ms per render). Distinct from `ResourceExceeded
    /// { limit: MaxOperations }`, which covers CPU-budget overruns
    /// surfaced by rhai itself. Drops the segment; logged.
    Timeout { id: String },

    /// `render(ctx)` returned a value that isn't `()` and isn't a map
    /// matching the `RenderedSegment` shape. Drops the segment.
    MalformedReturn { id: String, message: String },

    /// `@data_deps` declared a name that isn't in the plugin-accessible
    /// set. Per `plugin-api.md`, `credentials` and `jsonl` are reserved
    /// and surface here even though they're real `DataDep` variants.
    UnknownDataDep { id: String, name: String },

    /// `@data_deps = ...` header didn't parse as a JSON-style array of
    /// bare-string dep names.
    MalformedDataDeps { id: String, message: String },

    /// Two discovered plugins (or a plugin and a built-in) claim the
    /// same `id`. First-discovered wins per the precedence rules in
    /// `plugin-api.md` §Plugin file location; loser is rejected.
    IdCollision {
        id: String,
        winner_path: PathBuf,
        loser_path: PathBuf,
    },
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compile { path, message } => {
                write!(f, "compile error in {}: {message}", path.display())
            }
            Self::Runtime { id, message } => {
                write!(f, "plugin {id} runtime error: {message}")
            }
            Self::ResourceExceeded { id, limit } => {
                write!(f, "plugin {id} exceeded {limit}")
            }
            Self::Timeout { id } => write!(f, "plugin {id} timed out"),
            Self::MalformedReturn { id, message } => {
                write!(f, "plugin {id} returned malformed value: {message}")
            }
            Self::UnknownDataDep { id, name } => {
                write!(f, "plugin {id} declares unknown @data_deps entry `{name}`")
            }
            Self::MalformedDataDeps { id, message } => {
                write!(f, "plugin {id} has malformed @data_deps header: {message}")
            }
            Self::IdCollision {
                id,
                winner_path,
                loser_path,
            } => write!(
                f,
                "plugin id `{id}` collision: kept {}, rejected {}",
                winner_path.display(),
                loser_path.display()
            ),
        }
    }
}

impl std::error::Error for PluginError {}
