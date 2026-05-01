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
use std::time::{Duration, Instant};

use rhai::{Dynamic, Engine, EvalAltResult, Scope, AST};

use crate::data_context::{DataContext, DataDep};
use crate::segments::{RenderContext, RenderResult, Segment, SegmentError};

use super::ctx_mirror::build_ctx;
use super::engine::{
    set_current_plugin_id, set_render_deadline, DeadlineAbortMarker, DEFAULT_RENDER_DEADLINE_MS,
};
use super::output::validate_return;
use super::registry::CompiledPlugin;

/// RAII guard for the engine's per-render thread-local state
/// (`RENDER_DEADLINE` + `CURRENT_PLUGIN_ID`). Drop restores both to
/// `None` even if the render panics or short-circuits, so a leaky
/// thread-local can't poison subsequent renders on the same thread.
struct RenderState;

impl RenderState {
    fn install(plugin_id: &str, deadline: Instant) -> Self {
        // Catches a leaked thread-local — would mean a prior render
        // panicked between install and Drop without unwinding through
        // a Drop handler (e.g. caught by `catch_unwind`). Production
        // wouldn't notice; dev / test surfaces it loudly.
        debug_assert!(
            super::engine::render_deadline_snapshot().is_none(),
            "RENDER_DEADLINE leaked from a prior render"
        );
        debug_assert!(
            super::engine::current_plugin_id_snapshot().is_none(),
            "CURRENT_PLUGIN_ID leaked from a prior render"
        );
        set_render_deadline(Some(deadline));
        set_current_plugin_id(Some(plugin_id));
        Self
    }
}

impl Drop for RenderState {
    fn drop(&mut self) {
        set_render_deadline(None);
        set_current_plugin_id(None);
    }
}

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

    /// Map a rhai eval error into a [`SegmentError`]. Deadline aborts
    /// get a wallclock-specific message that names the host's default
    /// budget; every other failure carries rhai's own `Display`
    /// output through unchanged.
    ///
    /// Deadline classification matches against [`DeadlineAbortMarker`]
    /// — a host-only Rust type plugins can't construct from rhai.
    /// Plugin `throw` also surfaces as `ErrorTerminated`, but with a
    /// script-supplied payload that can't impersonate the marker.
    fn classify_render_error(&self, err: Box<EvalAltResult>) -> SegmentError {
        if let EvalAltResult::ErrorTerminated(token, _) = err.as_ref() {
            if token.is::<DeadlineAbortMarker>() {
                return SegmentError::new(format!(
                    "plugin `{}` exceeded the {}ms render deadline",
                    self.id, DEFAULT_RENDER_DEADLINE_MS
                ));
            }
        }
        SegmentError::new(format!("plugin `{}` render failed: {err}", self.id))
    }
}

impl Segment for RhaiSegment {
    fn render(&self, ctx: &DataContext, rc: &RenderContext) -> RenderResult {
        let mirror = build_ctx(ctx, rc, self.declared_deps, self.config.clone());
        let deadline = Instant::now() + Duration::from_millis(DEFAULT_RENDER_DEADLINE_MS);
        let _state = RenderState::install(&self.id, deadline);
        let mut scope = Scope::new();
        let returned: Dynamic = self
            .engine
            .call_fn(&mut scope, &self.ast, "render", (mirror,))
            .map_err(|e| self.classify_render_error(e))?;
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
            model: Some(ModelInfo {
                display_name: "Sonnet".to_string(),
            }),
            workspace: Some(WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            }),
            context_window: None,
            cost: None,
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            version: None,
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
        let registry =
            PluginRegistry::load_with_xdg(&[dir.path().to_path_buf()], None, &engine, &[]);
        assert!(
            registry.load_errors().is_empty(),
            "unexpected load errors: {:?}",
            registry.load_errors()
        );
        let plugin = registry
            .into_plugins()
            .into_iter()
            .next()
            .expect("plugin loaded");
        (plugin, engine)
    }

    #[test]
    fn plugin_can_read_terminal_width_from_ctx_render() {
        // Single test that exercises the full RenderContext threading
        // path: layout engine → RhaiSegment::render → build_ctx →
        // build_render → rhai property read. A regression in any link
        // (missing key, misnamed field, host-side mirror dropped)
        // surfaces here. The number `137` is arbitrary; the test pins
        // that whatever the host passes is what the script sees.
        let tmp = TempDir::new().expect("tempdir");
        let (plugin, engine) = load_single(
            &tmp,
            "tw.rhai",
            r#"
            const ID = "tw";
            fn render(ctx) {
                #{ runs: [#{ text: `${ctx.render.terminal_width}` }] }
            }
            "#,
        );
        let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT);
        let dc = DataContext::new(minimal_status());
        let rc = RenderContext::new(137);
        let rendered = seg.render(&dc, &rc).unwrap().expect("rendered");
        assert_eq!(rendered.text(), "137");
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
        let rc = RenderContext::new(80);
        assert_eq!(seg.render(&dc, &rc).unwrap(), None);
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
        let rc = RenderContext::new(80);
        let rendered = seg.render(&dc, &rc).unwrap().expect("rendered");
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
        let rc = RenderContext::new(80);
        let rendered = seg.render(&dc, &rc).unwrap().expect("rendered");
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
        let rc = RenderContext::new(80);
        let rendered = seg.render(&dc, &rc).unwrap().expect("rendered");
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
        let rc = RenderContext::new(80);
        let rendered = seg.render(&dc, &rc).unwrap().expect("rendered");
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
        // panic. Also confirms the *generic* classifier branch:
        // non-deadline failures must NOT be relabeled as a deadline
        // abort by an over-eager classifier match.
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
        let rc = RenderContext::new(80);
        let err = seg.render(&dc, &rc).unwrap_err();
        assert!(err.message.contains("boom"), "message: {}", err.message);
        assert!(
            err.message.contains("render failed"),
            "non-deadline failures must use the generic branch: {}",
            err.message
        );
        assert!(
            !err.message.contains("deadline"),
            "non-deadline failures must NOT be relabeled as a timeout: {}",
            err.message
        );
    }

    #[test]
    fn plugin_throw_cannot_impersonate_deadline_abort() {
        // Codex flagged: a plugin that `throw`s a string identical to
        // a former host sentinel could be misclassified as a deadline
        // timeout. With the marker-type sentinel, even a plugin that
        // throws the most-suspicious-looking string must fall through
        // to the generic "render failed" branch.
        let tmp = TempDir::new().expect("tempdir");
        let (plugin, engine) = load_single(
            &tmp,
            "fake.rhai",
            r##"
            const ID = "fake_deadline";
            fn render(ctx) {
                throw "linesmith:render-deadline-exceeded";
            }
            "##,
        );
        let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT);
        let dc = DataContext::new(minimal_status());
        let rc = RenderContext::new(80);
        let err = seg.render(&dc, &rc).unwrap_err();
        assert!(
            err.message.contains("render failed"),
            "throw must use the generic branch: {}",
            err.message
        );
        // Host-specific wording is "exceeded the {N}ms render deadline";
        // the thrown payload's "deadline" substring is allowed because
        // it's the script's own message, surfaced verbatim by rhai.
        assert!(
            !err.message.contains("exceeded the"),
            "thrown payload must not impersonate the host deadline message: {}",
            err.message
        );
    }

    #[test]
    fn render_state_drop_clears_thread_locals() {
        // Pin the load-bearing safety property of `RenderState`: Drop
        // restores both thread-locals to None even after a clean
        // scope exit. A regression that removed either set_*(None)
        // call from Drop would silently leak a stale deadline or
        // plugin id into subsequent renders on this thread.
        use crate::plugins::engine::{current_plugin_id_snapshot, render_deadline_snapshot};
        {
            let _state =
                RenderState::install("guard_test", Instant::now() + Duration::from_secs(60));
            assert!(render_deadline_snapshot().is_some());
            assert_eq!(current_plugin_id_snapshot().as_deref(), Some("guard_test"));
        }
        assert!(render_deadline_snapshot().is_none(), "deadline leaked");
        assert!(current_plugin_id_snapshot().is_none(), "plugin id leaked");
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
        let rc = RenderContext::new(80);
        let err = seg.render(&dc, &rc).unwrap_err();
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
    fn deadline_abort_surfaces_clear_segment_error() {
        // RhaiSegment::render installs a fresh 50ms deadline via its
        // RAII guard, overwriting any prior thread-local. To exercise
        // the classifier path the segment uses on a real abort, drive
        // the engine directly with a past deadline, then feed the
        // resulting EvalAltResult through `classify_render_error`.
        use crate::plugins::engine::set_render_deadline;
        let tmp = TempDir::new().expect("tempdir");
        let (plugin, engine) =
            load_single(&tmp, "x.rhai", r#"const ID = "x"; fn render(ctx) { () }"#);
        set_render_deadline(Some(Instant::now()));
        let err = engine.eval::<()>("loop {}").unwrap_err();
        set_render_deadline(None);
        let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT);
        let segment_err = seg.classify_render_error(err);
        assert!(
            segment_err.message.contains("deadline"),
            "deadline aborts should name the timeout: {}",
            segment_err.message
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
        let rc = RenderContext::new(80);
        let err = seg.render(&dc, &rc).unwrap_err();
        assert!(
            err.message.to_lowercase().contains("operation") || err.message.contains("loop"),
            "message: {}",
            err.message
        );
    }
}
