//! Integration tests that drive every on-disk plugin fixture through
//! the discovery → compile → render pipeline. Each fixture embeds via
//! `include_str!` so the test binary is self-contained; per-test
//! tempdirs keep registries isolated (e.g. so the syntax-error
//! fixture doesn't pollute load_errors counts in the timeout test).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rhai::{Dynamic, Engine};
use tempfile::TempDir;

use linesmith::data_context::DataContext;
use linesmith::input::StatusContext;
use linesmith::plugins::{build_engine, CompiledPlugin, PluginError, PluginRegistry, RhaiSegment};
use linesmith::segments::{Segment, BUILT_IN_SEGMENT_IDS};

const MINIMAL: &str = include_str!("fixtures/plugins/minimal.rhai");
const USES_CTX_CONFIG: &str = include_str!("fixtures/plugins/uses_ctx_config.rhai");
const VISIBILITY: &str = include_str!("fixtures/plugins/visibility_via_render.rhai");
const DECLARES_USAGE: &str = include_str!("fixtures/plugins/declares_usage.rhai");
const SYNTAX_ERROR: &str = include_str!("fixtures/plugins/syntax_error.rhai");
const RUNTIME_ERROR: &str = include_str!("fixtures/plugins/runtime_error.rhai");
const INFINITE_LOOP: &str = include_str!("fixtures/plugins/infinite_loop.rhai");
const COLLISION_BUILT_IN: &str = include_str!("fixtures/plugins/collision_built_in.rhai");
const UNKNOWN_DEP: &str = include_str!("fixtures/plugins/unknown_dep.rhai");

fn write_fixture(dir: &TempDir, name: &str, src: &str) -> PathBuf {
    let p = dir.path().join(name);
    fs::write(&p, src).expect("write fixture");
    p
}

fn load_isolated(name: &str, src: &str) -> (PluginRegistry, Arc<Engine>, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    write_fixture(&tmp, name, src);
    let engine = build_engine();
    let registry = PluginRegistry::load_with_xdg(
        &[tmp.path().to_path_buf()],
        None,
        &engine,
        BUILT_IN_SEGMENT_IDS,
    );
    (registry, engine, tmp)
}

fn first_segment(registry: PluginRegistry, engine: Arc<Engine>) -> RhaiSegment {
    // Surface load errors in the panic payload — a fixture that
    // silently fails to register would otherwise panic with an
    // opaque "no plugin compiled" instead of the actual cause.
    let errors: Vec<String> = registry
        .load_errors()
        .iter()
        .map(|e| format!("{e:?}"))
        .collect();
    let plugin: CompiledPlugin = registry
        .into_plugins()
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            panic!("expected one compiled plugin; load errors: {errors:?}");
        });
    RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT)
}

const MINIMAL_PAYLOAD: &[u8] = include_bytes!("fixtures/claude_minimal.json");
const WORKTREE_PAYLOAD: &[u8] = include_bytes!("fixtures/claude_worktree.json");

fn minimal_status() -> StatusContext {
    linesmith::input::parse(MINIMAL_PAYLOAD).expect("minimal fixture parses")
}

fn worktree_status() -> StatusContext {
    // Carries `rate_limits` so the visibility fixture can render its
    // `Some(_)` branch end-to-end.
    linesmith::input::parse(WORKTREE_PAYLOAD).expect("worktree fixture parses")
}

#[test]
fn minimal_fixture_compiles_and_renders_literal_text() {
    let (registry, engine, _tmp) = load_isolated("minimal.rhai", MINIMAL);
    assert!(
        registry.load_errors().is_empty(),
        "unexpected load errors: {:?}",
        registry.load_errors()
    );
    let seg = first_segment(registry, engine);
    let dc = DataContext::new(minimal_status());
    let rendered = seg.render(&dc).unwrap().expect("visible");
    assert_eq!(rendered.text(), "minimal");
}

#[test]
fn uses_ctx_config_fixture_round_trips_label_from_toml_extras() {
    let (registry, engine, _tmp) = load_isolated("uses_ctx_config.rhai", USES_CTX_CONFIG);
    let plugin = registry.into_plugins().into_iter().next().unwrap();
    let mut config = rhai::Map::new();
    config.insert("label".into(), Dynamic::from("from-fixture".to_string()));
    let seg = RhaiSegment::from_compiled(plugin, engine, Dynamic::from_map(config));
    let dc = DataContext::new(minimal_status());
    let rendered = seg.render(&dc).unwrap().expect("visible");
    assert_eq!(rendered.text(), "from-fixture");
}

#[test]
fn visibility_fixture_hides_when_rate_limits_unset() {
    let (registry, engine, _tmp) = load_isolated("visibility_via_render.rhai", VISIBILITY);
    let seg = first_segment(registry, engine);
    let dc = DataContext::new(minimal_status());
    assert_eq!(seg.render(&dc).unwrap(), None);
}

#[test]
fn visibility_fixture_renders_when_rate_limits_present() {
    let (registry, engine, _tmp) = load_isolated("visibility_via_render.rhai", VISIBILITY);
    let seg = first_segment(registry, engine);
    let dc = DataContext::new(worktree_status());
    let rendered = seg.render(&dc).unwrap().expect("visible");
    assert_eq!(rendered.text(), "rate-limit-aware");
}

#[test]
fn declares_usage_fixture_sees_stub_not_implemented() {
    // The lazy `usage` source is a stub returning NotImplemented;
    // the fixture renders the error code so a regression that dropped
    // declared-dep gating (or changed the error variant) trips here
    // before it hits production.
    let (registry, engine, _tmp) = load_isolated("declares_usage.rhai", DECLARES_USAGE);
    let seg = first_segment(registry, engine);
    let dc = DataContext::new(minimal_status());
    let rendered = seg.render(&dc).unwrap().expect("visible");
    assert_eq!(rendered.text(), "NotImplemented");
}

#[test]
fn syntax_error_fixture_rejected_at_load_time() {
    let (registry, _engine, _tmp) = load_isolated("syntax_error.rhai", SYNTAX_ERROR);
    assert!(registry.is_empty(), "fixture must not register");
    let errors = registry.load_errors();
    assert_eq!(errors.len(), 1);
    assert!(matches!(errors[0], PluginError::Compile { .. }));
}

#[test]
fn collision_built_in_fixture_rejected_with_id_collision() {
    let (registry, _engine, _tmp) = load_isolated("collision_built_in.rhai", COLLISION_BUILT_IN);
    assert!(registry.is_empty());
    let errors = registry.load_errors();
    assert_eq!(errors.len(), 1);
    let PluginError::IdCollision { id, .. } = &errors[0] else {
        panic!("expected IdCollision, got {:?}", errors[0]);
    };
    assert_eq!(id, "model");
}

#[test]
fn unknown_dep_fixture_rejected_with_unknown_data_dep() {
    let (registry, _engine, _tmp) = load_isolated("unknown_dep.rhai", UNKNOWN_DEP);
    assert!(registry.is_empty());
    let errors = registry.load_errors();
    assert_eq!(errors.len(), 1);
    let PluginError::UnknownDataDep { name, .. } = &errors[0] else {
        panic!("expected UnknownDataDep, got {:?}", errors[0]);
    };
    assert_eq!(name, "credentials");
}

#[test]
fn runtime_error_fixture_maps_to_segment_error() {
    let (registry, engine, _tmp) = load_isolated("runtime_error.rhai", RUNTIME_ERROR);
    let seg = first_segment(registry, engine);
    let dc = DataContext::new(minimal_status());
    let err = seg.render(&dc).unwrap_err();
    assert!(
        err.message.contains("runtime_error"),
        "message must name the plugin: {}",
        err.message
    );
    assert!(err.message.contains("render failed"));
}

#[test]
fn infinite_loop_fixture_aborts_via_op_limit_within_bounded_time() {
    // A tight `loop {}` trips MAX_OPERATIONS in microseconds, so this
    // test pins the op-limit branch (not the wallclock deadline; that
    // path is unit-tested under src/plugins/engine.rs and segment.rs).
    // The bounded-time assertion is the CI-hang regression guard.
    let (registry, engine, _tmp) = load_isolated("infinite_loop.rhai", INFINITE_LOOP);
    let seg = first_segment(registry, engine);
    let dc = DataContext::new(minimal_status());

    let started = Instant::now();
    let err = seg.render(&dc).unwrap_err();
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(500),
        "infinite_loop fixture must abort within 500ms, took {elapsed:?}"
    );
    assert!(
        err.message.contains("render failed"),
        "expected the generic op-limit classifier branch, got: {}",
        err.message
    );
    // Pin the rhai-side wording so the deadline-branch path can never
    // satisfy this test by accident.
    assert!(
        err.message.to_lowercase().contains("operations"),
        "expected the op-limit message to surface rhai's `operations` literal, got: {}",
        err.message
    );
}
