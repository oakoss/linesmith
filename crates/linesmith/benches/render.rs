//! End-to-end render benchmarks for local regression detection. Run
//! via `mise run bench` (or `cargo bench --bench render`) before and
//! after changes to the layout, segment, or theme hot paths to spot
//! algorithmic regressions inside the post-spawn render loop.
//!
//! These benches do NOT defend the README's `<20ms cold start` claim
//! — process spawn, ELF load, and crate-level static init are outside
//! Criterion's measurement window. The `mise run bench:cold-start`
//! task uses hyperfine against a release build for the user-facing
//! wall-clock signal.
//!
//! Per `docs/research/perf-benchmarking-survey.md`, CI gating with
//! Criterion + GitHub-hosted runners is low-signal: variance overwhelms
//! the regression delta worth catching, so this file is local-only.
//! CodSpeed on bare-metal CI runners is the v0.2+ candidate (tracked
//! as `lsm-ue14`).
//!
//! Three configurations cover the realistic surface:
//!
//! - `render_minimal_*`: `["model", "workspace"]` over the smallest
//!   valid Claude payload. Floor for a minimal config in a real
//!   worktree — `workspace` declares `DataDep::Git`, so gix discovery
//!   still runs.
//! - `render_default_*`: `DEFAULT_SEGMENT_IDS` over the worktree
//!   fixture. Matches the first-run shape a fresh user gets.
//! - `render_all_no_usage_*`: every `BUILT_IN_SEGMENT_IDS` entry whose
//!   `data_deps()` does NOT include `DataDep::Usage` — those would
//!   route through the Keychain + JSONL + OAuth cascade and dominate
//!   measurements with I/O latency. See `all_no_usage_ids` for the
//!   filter and the maintenance hazard if a future `DataDep::Usage`
//!   segment lands with an unrelated id.
//!
//! All benches run against a `gix::init`-backed fixture repo
//! (`tempfile::TempDir`, shared across iterations via `OnceLock`) so
//! the numbers don't drift with whatever happens to be in the
//! linesmith checkout at measurement time. The fixture is intentionally
//! minimal; the bench's purpose is to detect algorithmic regressions
//! inside `run_with_context`, not to model dirty-state walks of large
//! repositories.

use std::io::Cursor;
use std::sync::OnceLock;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use tempfile::TempDir;

use linesmith::data_context::DataDep;
use linesmith::theme::{self, Capability};
use linesmith::{
    run_with_context,
    segments::{Segment, BUILT_IN_SEGMENT_IDS, DEFAULT_SEGMENT_IDS},
    RunContext,
};

const MINIMAL_PAYLOAD: &[u8] = include_bytes!("../tests/fixtures/claude_minimal.json");
const WORKTREE_PAYLOAD: &[u8] = include_bytes!("../tests/fixtures/claude_worktree.json");

const TERMINAL_WIDTH: u16 = 200;

const MINIMAL_IDS: &[&str] = &["model", "workspace"];

/// Skips every segment whose `data_deps()` includes `DataDep::Usage`
/// (the rate-limit family + `extra_usage`). Those would route through
/// the Keychain + JSONL + OAuth cascade during measurement and dominate
/// the bench numbers with I/O latency. Hand-maintained because
/// filtering by `data_deps()` would require constructing each segment
/// first.
fn all_no_usage_ids() -> Vec<&'static str> {
    BUILT_IN_SEGMENT_IDS
        .iter()
        .copied()
        .filter(|id| !id.starts_with("rate_limit_") && *id != "extra_usage")
        .collect()
}

/// Stable fixture repo, initialized once per bench process and shared
/// across iterations. Using `std::env::current_dir()` would tie
/// measurements to whatever's in the linesmith checkout (added
/// fixtures, dirty files, etc.), making the numbers drift PR-to-PR
/// even when render code is untouched. A `gix::init`-only repo
/// produces stable measurements at the cost of skipping dirty-state
/// walks of populated worktrees — that's an intentional trade for a
/// regression detector.
fn fixture_repo() -> &'static std::path::Path {
    static REPO: OnceLock<TempDir> = OnceLock::new();
    REPO.get_or_init(|| {
        let tmp = TempDir::new().expect("bench setup: TempDir");
        gix::init(tmp.path()).expect("bench setup: gix init");
        tmp
    })
    .path()
}

/// Builds the segment list for a bench config, with three sanity
/// checks. Any `build_segments` warnings panic (a typo'd or renamed
/// id would otherwise silently shrink the measured config); the
/// returned segment count must equal the requested id count; and
/// when `forbid_usage` is set, no constructed segment may declare
/// `DataDep::Usage` — that catches a future `DataDep::Usage` segment
/// landing with an id pattern `all_no_usage_ids`'s name-coupled
/// filter doesn't match (which would silently route the
/// Keychain + JSONL + OAuth cascade into measurements).
fn build_named_segments(ids: &[&str], name: &str, forbid_usage: bool) -> Vec<Box<dyn Segment>> {
    let cfg = build_config(ids, name);
    let mut warnings: Vec<String> = Vec::new();
    let segs = linesmith::build_segments(Some(&cfg), None, |w| warnings.push(w.to_string()));
    if !warnings.is_empty() {
        panic!("bench '{name}' build_segments emitted warnings: {warnings:?}");
    }
    if segs.len() != ids.len() {
        panic!(
            "bench '{name}' built {} segments but requested {}: ids={:?}",
            segs.len(),
            ids.len(),
            ids
        );
    }
    if forbid_usage {
        for (id, seg) in ids.iter().zip(segs.iter()) {
            if seg.data_deps().contains(&DataDep::Usage) {
                panic!(
                    "bench '{name}' includes segment '{id}' which declares DataDep::Usage; \
                     update all_no_usage_ids' filter to exclude it"
                );
            }
        }
    }
    segs
}

fn build_config(ids: &[&str], name: &str) -> linesmith::config::Config {
    let toml_src = format!(
        "[line]\nsegments = [{}]\n",
        ids.iter()
            .map(|id| format!("\"{id}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    toml_src
        .parse()
        .unwrap_or_else(|e| panic!("bench '{name}' synthetic config did not parse: {e}"))
}

fn run_context(cwd: std::path::PathBuf) -> RunContext<'static> {
    RunContext::new(
        theme::default_theme(),
        Capability::None,
        TERMINAL_WIDTH,
        Some(cwd),
    )
}

/// One full render: parse → DataContext → layout → write. The sink is
/// a `Vec<u8>` opaquely consumed via `criterion::black_box(&sink[..])`,
/// which keeps the optimizer from eliminating any upstream
/// `Write::write_all` calls.
fn render_once(
    segments: &[Box<dyn Segment>],
    payload: &[u8],
    ctx: &RunContext<'_>,
    sink: &mut Vec<u8>,
    name: &str,
) {
    sink.clear();
    let mut stderr_sink = std::io::sink();
    run_with_context(
        Cursor::new(payload),
        &mut *sink,
        &mut stderr_sink,
        segments,
        ctx,
    )
    .unwrap_or_else(|e| {
        panic!(
            "bench '{name}' render failed over {} bytes: {e}",
            payload.len()
        )
    });
    criterion::black_box(&sink[..]);
}

fn cold_bench(
    c: &mut Criterion,
    name: &'static str,
    ids: &[&str],
    payload: &'static [u8],
    forbid_usage: bool,
) {
    let cwd = fixture_repo().to_path_buf();
    c.bench_function(name, |b| {
        b.iter_batched_ref(
            || {
                (
                    build_named_segments(ids, name, forbid_usage),
                    Vec::with_capacity(4096),
                )
            },
            |(segs, sink)| render_once(segs, payload, &run_context(cwd.clone()), sink, name),
            BatchSize::PerIteration,
        );
    });
}

fn warm_bench(
    c: &mut Criterion,
    name: &'static str,
    ids: &[&str],
    payload: &'static [u8],
    forbid_usage: bool,
) {
    let cwd = fixture_repo().to_path_buf();
    let segs = build_named_segments(ids, name, forbid_usage);
    let ctx = run_context(cwd);
    let mut sink = Vec::with_capacity(4096);
    c.bench_function(name, |b| {
        b.iter(|| render_once(&segs, payload, &ctx, &mut sink, name));
    });
}

fn bench_minimal_cold(c: &mut Criterion) {
    cold_bench(
        c,
        "render_minimal_cold",
        MINIMAL_IDS,
        MINIMAL_PAYLOAD,
        false,
    );
}

fn bench_minimal_warm(c: &mut Criterion) {
    warm_bench(
        c,
        "render_minimal_warm",
        MINIMAL_IDS,
        MINIMAL_PAYLOAD,
        false,
    );
}

fn bench_default_cold(c: &mut Criterion) {
    cold_bench(
        c,
        "render_default_cold",
        DEFAULT_SEGMENT_IDS,
        WORKTREE_PAYLOAD,
        false,
    );
}

fn bench_default_warm(c: &mut Criterion) {
    warm_bench(
        c,
        "render_default_warm",
        DEFAULT_SEGMENT_IDS,
        WORKTREE_PAYLOAD,
        false,
    );
}

fn bench_all_no_usage_cold(c: &mut Criterion) {
    let ids = all_no_usage_ids();
    cold_bench(c, "render_all_no_usage_cold", &ids, WORKTREE_PAYLOAD, true);
}

fn bench_all_no_usage_warm(c: &mut Criterion) {
    let ids = all_no_usage_ids();
    warm_bench(c, "render_all_no_usage_warm", &ids, WORKTREE_PAYLOAD, true);
}

criterion_group!(
    render,
    bench_minimal_cold,
    bench_minimal_warm,
    bench_default_cold,
    bench_default_warm,
    bench_all_no_usage_cold,
    bench_all_no_usage_warm,
);
criterion_main!(render);
