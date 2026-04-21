//! `RhaiSegment` — the adapter that lets a compiled `.rhai` plugin
//! participate in the layout engine as a first-class
//! [`crate::segments::Segment`].
//!
//! Built from a [`CompiledPlugin`] + the shared `Arc<Engine>`:
//! `declared_deps` is promoted to `&'static` (per the `Segment` trait's
//! lifetime contract) via `Vec::leak` once at construction. Each
//! render builds a fresh [`build_ctx`] mirror, invokes the script's
//! `render(ctx)` function, and runs the returned value through
//! [`validate_return`] for shape enforcement.
//!
//! Runtime failures (rhai errors, resource-exceeded, malformed
//! return) surface as [`SegmentError`] so the layout engine logs once
//! and hides the segment for this invocation, matching the posture of
//! a built-in `render` that returns `Err`.

use std::sync::Arc;

use rhai::{Dynamic, Engine, Scope, AST};

use crate::data_context::{DataContext, DataDep};
use crate::segments::{RenderResult, Segment, SegmentError};

use super::ctx_mirror::build_ctx;
use super::output::validate_return;
use super::registry::CompiledPlugin;

/// A plugin-authored segment backed by a compiled rhai script.
pub struct RhaiSegment {
    id: String,
    ast: AST,
    engine: Arc<Engine>,
    config: Dynamic,
    declared_deps: &'static [DataDep],
}

impl RhaiSegment {
    /// Wrap a [`CompiledPlugin`] in the [`Segment`] trait.
    ///
    /// `config` is the plugin's `[segments.<id>]` TOML table, already
    /// converted to a rhai-compatible [`Dynamic`]. Pass [`Dynamic::UNIT`]
    /// when no table is configured.
    ///
    /// Consumes `plugin`: the AST and declared deps move into the
    /// segment, and `declared_deps` is promoted to `&'static` via
    /// [`Vec::leak`] — see [`Segment::data_deps`] for why.
    #[must_use]
    pub fn from_compiled(plugin: CompiledPlugin, engine: Arc<Engine>, config: Dynamic) -> Self {
        let declared_deps: &'static [DataDep] = Vec::leak(plugin.declared_deps);
        Self {
            id: plugin.id,
            ast: plugin.ast,
            engine,
            config,
            declared_deps,
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl Segment for RhaiSegment {
    fn render(&self, ctx: &DataContext) -> RenderResult {
        let mirror = build_ctx(ctx, self.declared_deps, self.config.clone());
        let mut scope = Scope::new();
        let returned: Dynamic = self
            .engine
            .call_fn(&mut scope, &self.ast, "render", (mirror,))
            .map_err(|e| SegmentError::new(format!("plugin `{}` render failed: {e}", self.id)))?;
        validate_return(returned, &self.id).map_err(|e| {
            SegmentError::new(format!(
                "plugin `{}` returned malformed shape: {e}",
                self.id
            ))
        })
    }

    fn data_deps(&self) -> &'static [DataDep] {
        self.declared_deps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
    use crate::plugins::build_engine;
    use crate::plugins::registry::PluginRegistry;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn minimal_status() -> StatusContext {
        StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "Sonnet".to_string(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            },
            context_window: None,
            cost: None,
            rate_limits: None,
            effort: None,
            raw: Arc::new(serde_json::json!({})),
        }
    }

    fn load_single(
        dir: &tempfile::TempDir,
        name: &str,
        src: &str,
    ) -> (CompiledPlugin, Arc<Engine>) {
        fs::write(dir.path().join(name), src).expect("write plugin");
        let engine = build_engine();
        let (registry, errors) =
            PluginRegistry::load_with_xdg(&[dir.path().to_path_buf()], None, &engine, &[]);
        assert!(errors.is_empty(), "unexpected load errors: {errors:?}");
        let plugin = registry
            .into_plugins()
            .into_iter()
            .next()
            .expect("plugin loaded");
        (plugin, engine)
    }

    #[test]
    fn plugin_returning_unit_hides_segment() {
        let tmp = TempDir::new().expect("tempdir");
        let (plugin, engine) = load_single(
            &tmp,
            "hide.rhai",
            r#"
            const ID = "hide";
            fn render(ctx) { () }
            "#,
        );
        let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT);
        let dc = DataContext::new(minimal_status());
        assert_eq!(seg.render(&dc).unwrap(), None);
    }

    #[test]
    fn plugin_returning_single_run_renders() {
        let tmp = TempDir::new().expect("tempdir");
        let (plugin, engine) = load_single(
            &tmp,
            "simple.rhai",
            r#"
            const ID = "simple";
            fn render(ctx) {
                #{ runs: [#{ text: "hello" }] }
            }
            "#,
        );
        let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT);
        let dc = DataContext::new(minimal_status());
        let rendered = seg.render(&dc).unwrap().expect("rendered");
        assert_eq!(rendered.text(), "hello");
    }

    #[test]
    fn plugin_sees_status_fields_via_ctx() {
        let tmp = TempDir::new().expect("tempdir");
        let (plugin, engine) = load_single(
            &tmp,
            "model_echo.rhai",
            r#"
            const ID = "model_echo";
            fn render(ctx) {
                #{ runs: [#{ text: ctx.status.model.display_name }] }
            }
            "#,
        );
        let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT);
        let dc = DataContext::new(minimal_status());
        let rendered = seg.render(&dc).unwrap().expect("rendered");
        assert_eq!(rendered.text(), "Sonnet");
    }

    #[test]
    fn plugin_receives_config_passed_in() {
        let tmp = TempDir::new().expect("tempdir");
        let (plugin, engine) = load_single(
            &tmp,
            "cfg.rhai",
            r#"
            const ID = "cfg";
            fn render(ctx) {
                #{ runs: [#{ text: ctx.config.label }] }
            }
            "#,
        );
        let mut config = rhai::Map::new();
        config.insert("label".into(), Dynamic::from("configured".to_string()));
        let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::from_map(config));
        let dc = DataContext::new(minimal_status());
        let rendered = seg.render(&dc).unwrap().expect("rendered");
        assert_eq!(rendered.text(), "configured");
    }

    #[test]
    fn plugin_can_read_ctx_env_from_rhai_side() {
        // The whitelist + OnceLock snapshot in `ctx_mirror` is only
        // useful if `ctx.env.<KEY>` is actually reachable from a
        // running plugin. A plugin that branches on `ctx.env.TERM ==
        // ()` (the unset case) covers both the snapshot and the
        // unit-or-string discriminator.
        let tmp = TempDir::new().expect("tempdir");
        let (plugin, engine) = load_single(
            &tmp,
            "env.rhai",
            r#"
            const ID = "env_probe";
            fn render(ctx) {
                let term = ctx.env.TERM;
                let label = if term == () { "unset" } else { "set" };
                #{ runs: [#{ text: label }] }
            }
            "#,
        );
        let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT);
        let dc = DataContext::new(minimal_status());
        let rendered = seg.render(&dc).unwrap().expect("rendered");
        // Don't pin to "set" or "unset" — env_snapshot() is
        // process-cached, so test order can decide whether `TERM`
        // was set when the OnceLock was populated. Either label
        // proves the env path round-trips through rhai.
        assert!(rendered.text() == "set" || rendered.text() == "unset");
    }

    #[test]
    fn declared_deps_surface_via_trait() {
        let tmp = TempDir::new().expect("tempdir");
        let (plugin, engine) = load_single(
            &tmp,
            "deps.rhai",
            r#"// @data_deps = ["usage"]
            const ID = "deps";
            fn render(ctx) { () }
            "#,
        );
        let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT);
        assert!(seg.data_deps().contains(&DataDep::Status));
        assert!(seg.data_deps().contains(&DataDep::Usage));
    }

    #[test]
    fn plugin_runtime_error_maps_to_segment_error() {
        // Division-by-zero at runtime surfaces as a rhai error. The
        // segment must map it to `SegmentError` (hide + log), not
        // panic.
        let tmp = TempDir::new().expect("tempdir");
        let (plugin, engine) = load_single(
            &tmp,
            "boom.rhai",
            r#"
            const ID = "boom";
            fn render(ctx) {
                let n = 1 / 0;
                #{ runs: [#{ text: `${n}` }] }
            }
            "#,
        );
        let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT);
        let dc = DataContext::new(minimal_status());
        let err = seg.render(&dc).unwrap_err();
        assert!(err.message.contains("boom"), "message: {}", err.message);
    }

    #[test]
    fn plugin_returning_malformed_shape_maps_to_segment_error() {
        let tmp = TempDir::new().expect("tempdir");
        let (plugin, engine) = load_single(
            &tmp,
            "bad.rhai",
            r#"
            const ID = "bad_shape";
            fn render(ctx) { 42 }
            "#,
        );
        let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT);
        let dc = DataContext::new(minimal_status());
        let err = seg.render(&dc).unwrap_err();
        assert!(
            err.message.contains("bad_shape"),
            "message: {}",
            err.message
        );
        assert!(
            err.message.to_lowercase().contains("malformed") || err.message.contains("must return"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn operation_limit_kills_infinite_loop_without_hang() {
        // Without the engine's `max_operations` ceiling this test
        // hangs CI instead of failing.
        let tmp = TempDir::new().expect("tempdir");
        let (plugin, engine) = load_single(
            &tmp,
            "loop.rhai",
            r#"
            const ID = "loop";
            fn render(ctx) {
                loop {}
            }
            "#,
        );
        let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT);
        let dc = DataContext::new(minimal_status());
        let err = seg.render(&dc).unwrap_err();
        assert!(
            err.message.to_lowercase().contains("operation") || err.message.contains("loop"),
            "message: {}",
            err.message
        );
    }
}
