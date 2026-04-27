//! Plugin runtime benchmarks. Pins the ADR-0004 budgets:
//!
//! - `plugin_engine_init`: `build_engine()` cost in isolation (<1 ms)
//! - `plugin_compile`: rhai AST compile for the smallest valid plugin (<3 ms)
//! - `plugin_load`: end-to-end discovery + compile via [`PluginRegistry`]
//! - `plugin_render_cold`: first `RhaiSegment::render` with a fresh
//!   engine (<2 ms)
//! - `plugin_render_warm`: subsequent render on the same segment (<1 ms)
//!
//! `plugin_render_warm` reuses one [`DataContext`] across iterations,
//! so its lazy `OnceCell` sources are cached after the first render.
//! That's intentional steady-state measurement, not the per-prompt
//! cold-ctx path; a future bench can add a fresh-ctx variant.
//!
//! Numbers are wall-clock measurements on the developer machine; CI
//! doesn't run benches in the gate. Use `mise run bench` to refresh
//! the locally-recorded baseline before changing anything that
//! touches the engine setup or the render hot path.

use std::sync::Arc;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use rhai::{Dynamic, Engine};
use tempfile::TempDir;

use linesmith::data_context::DataContext;
use linesmith::plugins::{build_engine, CompiledPlugin, PluginRegistry, RhaiSegment};
use linesmith::segments::{RenderContext, Segment, BUILT_IN_SEGMENT_IDS};

const MINIMAL_PLUGIN: &str = include_str!("../tests/fixtures/plugins/minimal.rhai");
const MINIMAL_PAYLOAD: &[u8] = include_bytes!("../tests/fixtures/claude_minimal.json");

fn fresh_registry(src: &str) -> (PluginRegistry, Arc<Engine>, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("minimal.rhai"), src).expect("write fixture");
    let engine = build_engine();
    let registry = PluginRegistry::load_with_xdg(
        &[tmp.path().to_path_buf()],
        None,
        &engine,
        BUILT_IN_SEGMENT_IDS,
    );
    assert!(
        registry.load_errors().is_empty(),
        "bench fixture must compile cleanly: {:?}",
        registry.load_errors()
    );
    (registry, engine, tmp)
}

fn build_segment(registry: PluginRegistry, engine: Arc<Engine>) -> RhaiSegment {
    let plugin: CompiledPlugin = registry
        .into_plugins()
        .into_iter()
        .next()
        .expect("compiled plugin");
    RhaiSegment::from_compiled(plugin, engine, Dynamic::UNIT)
}

fn rc() -> RenderContext {
    RenderContext::new(80)
}

fn data_context() -> DataContext {
    DataContext::new(linesmith::input::parse(MINIMAL_PAYLOAD).expect("payload parses"))
}

fn bench_engine_init(c: &mut Criterion) {
    // `build_engine` registers StandardPackage, host fns, resource
    // limits, and the `on_progress` callback. ADR-0004 budgets the
    // whole thing under 1 ms.
    c.bench_function("plugin_engine_init", |b| {
        b.iter(|| {
            let engine = build_engine();
            criterion::black_box(engine);
        });
    });
}

fn bench_compile(c: &mut Criterion) {
    // Pure rhai AST compile — no filesystem, no registry, no segment
    // wrap. Fresh engine per iteration so the AST cache and string
    // interning are paid for each measurement, isolating the compile
    // cost from process-wide warmup.
    c.bench_function("plugin_compile", |b| {
        b.iter_batched(
            build_engine,
            |engine| {
                let ast = engine.compile(MINIMAL_PLUGIN).expect("compile ok");
                criterion::black_box(ast);
            },
            BatchSize::PerIteration,
        );
    });
}

fn bench_load(c: &mut Criterion) {
    // End-to-end load: filesystem discovery + per-file compile +
    // header parse + collision check. The `plugin_compile` bench
    // isolates parse cost from this; the gap between them is what
    // discovery + bookkeeping pay.
    c.bench_function("plugin_load", |b| {
        b.iter_batched(
            || {
                let tmp = TempDir::new().expect("tempdir");
                std::fs::write(tmp.path().join("minimal.rhai"), MINIMAL_PLUGIN)
                    .expect("write fixture");
                tmp
            },
            |tmp| {
                let engine = build_engine();
                let registry = PluginRegistry::load_with_xdg(
                    &[tmp.path().to_path_buf()],
                    None,
                    &engine,
                    BUILT_IN_SEGMENT_IDS,
                );
                assert!(registry.load_errors().is_empty());
                criterion::black_box(registry);
            },
            BatchSize::PerIteration,
        );
    });
}

fn bench_render_cold(c: &mut Criterion) {
    // Cold = fresh engine + fresh segment per iteration. Pays the
    // CompiledPlugin → RhaiSegment wrap and the first call_fn per
    // measurement; the only thing held constant is the source text.
    c.bench_function("plugin_render_cold", |b| {
        b.iter_batched(
            || {
                let (registry, engine, tmp) = fresh_registry(MINIMAL_PLUGIN);
                (build_segment(registry, engine), data_context(), tmp)
            },
            |(seg, ctx, _tmp)| {
                let rendered = seg
                    .render(&ctx, &rc())
                    .expect("render ok")
                    .expect("visible rendered segment");
                criterion::black_box(rendered.text());
            },
            BatchSize::PerIteration,
        );
    });
}

fn bench_render_warm(c: &mut Criterion) {
    // Warm = reuse the same segment + DataContext across iterations.
    // Measures steady-state render cost once the engine, AST, and
    // any lazy ctx sources are resident.
    let (registry, engine, _tmp) = fresh_registry(MINIMAL_PLUGIN);
    let seg = build_segment(registry, engine);
    let ctx = data_context();
    c.bench_function("plugin_render_warm", |b| {
        b.iter(|| {
            let rendered = seg
                .render(&ctx, &rc())
                .expect("render ok")
                .expect("visible rendered segment");
            criterion::black_box(rendered.text());
        });
    });
}

criterion_group!(
    plugins,
    bench_engine_init,
    bench_compile,
    bench_load,
    bench_render_cold,
    bench_render_warm,
);
criterion_main!(plugins);
