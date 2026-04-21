//! Rhai engine construction for the plugin runtime.
//!
//! [`build_engine`] returns a single shared `Arc<rhai::Engine>`
//! configured per `docs/specs/plugin-api.md` §Resource ceilings and
//! §Host-registered APIs. Every plugin segment invokes `call_fn` on
//! this shared engine with its compiled AST; the engine is not
//! recreated per plugin or per render.
//!
//! Sandboxing posture (plugin-api.md §Requirements → Functional):
//! - No filesystem or network functions registered → unknown-symbol
//!   errors at script parse/runtime for any `fs::*` / `http::*` call.
//! - `import` and `eval` symbols disabled → scripts cannot load other
//!   files or compile strings at runtime.
//! - Resource limits cap operations, call depth, expression nesting,
//!   and string / array / map size.
//!
//! Wallclock-timeout enforcement (via `on_progress`) is the segment
//! wrapper's job — it owns the per-render `Instant` — not the engine's.
//! This module enforces operation-count limits only.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use rhai::packages::{Package, StandardPackage};
use rhai::{Dynamic, Engine};

/// Max script operations per plugin invocation.
pub const MAX_OPERATIONS: u64 = 50_000;
/// Max call depth for user-defined functions.
pub const MAX_CALL_LEVELS: usize = 16;
/// Max expression nesting (functions, other).
pub const MAX_EXPR_DEPTH: usize = 32;
/// Max length of any rhai string.
pub const MAX_STRING_SIZE: usize = 1024;
/// Max length of any rhai array.
pub const MAX_ARRAY_SIZE: usize = 256;
/// Max entry count of any rhai map.
pub const MAX_MAP_SIZE: usize = 256;
/// Default per-render wallclock budget per `plugin-api.md` §Resource ceilings.
pub const DEFAULT_RENDER_DEADLINE_MS: u64 = 50;

/// Host-side marker passed to `EvalAltResult::ErrorTerminated` when the
/// `on_progress` callback aborts the script for exceeding its
/// wallclock deadline. The segment wrapper inspects the token type
/// to produce a clearer error than rhai's generic "Script terminated".
///
/// Must be a host-only type (not a string) so a plugin can't forge a
/// deadline classification by `throw`-ing a coincidentally-equal
/// string payload — rhai's `Dynamic` keeps the underlying Rust type
/// intact, and the type itself is module-private to host code.
#[derive(Clone)]
pub(crate) struct DeadlineAbortMarker;
/// `on_progress` is called per rhai operation; checking the deadline
/// every op would burn cycles on `Instant::now()`. Stride amortises:
/// at 256 ops between checks, the worst-case overrun is roughly the
/// per-op cost × 256 — sub-ms for typical script ops, larger if a
/// plugin sits in a host-fn-heavy loop. The 50 ms budget should be
/// read as "50 ms + (stride − 1) × per-op cost".
const DEADLINE_CHECK_STRIDE: u64 = 256;

// Compile-time invariants for the stride: stride=0 panics with
// modulo-by-zero on the first op; stride >= MAX_OPERATIONS silently
// disables deadline enforcement because the op-limit fires first.
const _: () = assert!(DEADLINE_CHECK_STRIDE > 0);
const _: () = assert!(DEADLINE_CHECK_STRIDE < MAX_OPERATIONS);

thread_local! {
    /// Per-render wallclock deadline. The plugin-segment wrapper sets
    /// this just before invoking the script and clears it after, so
    /// the engine's `on_progress` callback can abort runaway scripts.
    static RENDER_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };

    /// Identifier of the plugin currently being rendered. Lets the
    /// host `log` function attribute output to a specific plugin and
    /// rate-limit per id without changing the rhai function surface.
    static CURRENT_PLUGIN_ID: RefCell<Option<String>> = const { RefCell::new(None) };

    /// Total `log()` invocations emitted to stderr per plugin id for
    /// the lifetime of this thread (== process for a one-shot CLI).
    /// Per-plugin rate limit per `plugin-api.md` §Host-registered APIs.
    /// **Per-thread, not per-process:** if rendering ever goes
    /// multi-threaded (parallel segment render), each thread gets its
    /// own quota. Switch to `Mutex<HashMap>` then.
    static LOG_EMITTED: RefCell<HashMap<String, u32>> = RefCell::new(HashMap::new());
}

/// Maximum `log()` lines per plugin per process. Higher counts get
/// silently dropped to keep a chatty plugin from flooding stderr.
pub const LOG_LINES_PER_PLUGIN: u32 = 1;

/// Install a per-render deadline visible to the engine's `on_progress`
/// callback. Pass `None` to clear after the render completes.
pub fn set_render_deadline(deadline: Option<Instant>) {
    RENDER_DEADLINE.with(|d| d.set(deadline));
}

/// Tag the active plugin so the host `log()` function can attribute
/// output for rate-limiting. Pass `None` to clear after the render.
pub fn set_current_plugin_id(id: Option<&str>) {
    CURRENT_PLUGIN_ID.with(|cell| {
        *cell.borrow_mut() = id.map(str::to_owned);
    });
}

/// Drop every emission count for the current thread. Wholesale clear
/// is the contract — callers depending on per-id reset will need a
/// new helper. Test-only; the production rate-limit is process-
/// lifetime and intentionally does not expose a reset path to plugins.
#[cfg(test)]
pub(crate) fn reset_log_counts() {
    LOG_EMITTED.with(|cell| cell.borrow_mut().clear());
}

/// Snapshot of the current thread's `RENDER_DEADLINE`. Used by the
/// segment wrapper's `debug_assert!` leak-check and by tests; the
/// production render path doesn't need to read the deadline back.
pub(crate) fn render_deadline_snapshot() -> Option<Instant> {
    RENDER_DEADLINE.with(Cell::get)
}

/// Snapshot of the current thread's `CURRENT_PLUGIN_ID`. Same niche
/// as [`render_deadline_snapshot`].
pub(crate) fn current_plugin_id_snapshot() -> Option<String> {
    CURRENT_PLUGIN_ID.with(|c| c.borrow().clone())
}

/// Build the shared rhai engine used by every plugin segment. Returns
/// an `Arc` so the layout engine can clone cheaply into each
/// `RhaiSegment`. The engine is immutable after this call.
#[must_use]
pub fn build_engine() -> Arc<Engine> {
    let mut engine = Engine::new_raw();
    // `new_raw()` registers nothing; StandardPackage adds the common
    // script helpers (`str.len()`, `arr.push(x)`, iterators, …).
    engine.register_global_module(StandardPackage::new().as_shared_module());
    // No-op `print`/`debug` overrides; default routing leaks to host
    // stdout/stderr. The no-op-routing test pins this contract.
    engine.on_print(|_| {});
    engine.on_debug(|_, _, _| {});
    install_deadline_callback(&mut engine);
    configure_limits(&mut engine);
    lock_down_symbols(&mut engine);
    register_host_fns(&mut engine);
    Arc::new(engine)
}

fn install_deadline_callback(engine: &mut Engine) {
    engine.on_progress(|ops| {
        if ops % DEADLINE_CHECK_STRIDE != 0 {
            return None;
        }
        let deadline = RENDER_DEADLINE.with(Cell::get)?;
        if Instant::now() >= deadline {
            Some(Dynamic::from(DeadlineAbortMarker))
        } else {
            None
        }
    });
}

fn configure_limits(engine: &mut Engine) {
    engine.set_max_operations(MAX_OPERATIONS);
    engine.set_max_call_levels(MAX_CALL_LEVELS);
    engine.set_max_expr_depths(MAX_EXPR_DEPTH, MAX_EXPR_DEPTH);
    engine.set_max_string_size(MAX_STRING_SIZE);
    engine.set_max_array_size(MAX_ARRAY_SIZE);
    engine.set_max_map_size(MAX_MAP_SIZE);
}

fn lock_down_symbols(engine: &mut Engine) {
    // `import` loads other rhai files — plugins get one script file;
    // loading siblings would bypass the registry and discovery model.
    engine.disable_symbol("import");
    // `eval` compiles an arbitrary string at runtime — lets a plugin
    // author evade the operation-count budget of the original AST.
    engine.disable_symbol("eval");
}

fn register_host_fns(engine: &mut Engine) {
    engine.register_fn("log", rhai_log);
    engine.register_fn("format_duration", rhai_format_duration);
    // Rhai's Dynamic dispatch does not promote i64 → f64 for overload
    // resolution, so register both arities. Without the i64 variant a
    // plugin author writing `format_cost_usd(1)` instead of
    // `format_cost_usd(1.0)` hits "function not found" at render time.
    engine.register_fn("format_cost_usd", rhai_format_cost_usd);
    engine.register_fn("format_cost_usd", |n: i64| rhai_format_cost_usd(n as f64));
    engine.register_fn("format_tokens", rhai_format_tokens);
    engine.register_fn("format_countdown_until", rhai_format_countdown_until);
}

// Compile-time guarantee that `Arc<Engine>` stays thread-safe. Relies
// on the `sync` feature flag in `Cargo.toml`; if that flag gets
// dropped, this line breaks with a clear trait-bound error at the
// engine constructor instead of surfacing later in the segment
// wrapper that depends on the trait bound.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Arc<Engine>>();
};

/// Host-registered `log(msg)` for plugin scripts. Writes one line per
/// plugin per process to stderr; subsequent calls from the same
/// plugin are silently dropped to keep a chatty plugin from flooding
/// stderr. The active plugin id comes from a thread-local set by
/// `RhaiSegment::render`; calls outside a render (e.g. tests that
/// `eval` directly) attribute to a synthetic `<unscoped>` bucket.
///
/// Bumping the counter *before* `eprintln!` is deliberate: a chatty
/// plugin should pay at most a single `to_owned` per process for its
/// id, not one per dropped call.
fn rhai_log(msg: &str) {
    /// Sentinel id for `log()` calls outside a render scope.
    const UNSCOPED: &str = "<unscoped>";

    let allowed = LOG_EMITTED.with(|cell| {
        let mut counts = cell.borrow_mut();
        let id_str = CURRENT_PLUGIN_ID.with(|c| c.borrow().clone());
        let key: &str = id_str.as_deref().unwrap_or(UNSCOPED);
        match counts.get_mut(key) {
            Some(n) if *n >= LOG_LINES_PER_PLUGIN => None,
            Some(n) => {
                *n += 1;
                Some(key.to_owned())
            }
            None => {
                counts.insert(key.to_owned(), 1);
                Some(key.to_owned())
            }
        }
    });
    if let Some(id) = allowed {
        eprintln!("linesmith plugin {id}: {msg}");
    }
}

/// Format a duration in milliseconds as `"1h 23m"` / `"45m"` / `"12s"`.
/// Negative inputs render as `"0s"`.
fn rhai_format_duration(ms: i64) -> String {
    if ms <= 0 {
        return "0s".to_string();
    }
    let total_seconds = ms / 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        if minutes > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{hours}h")
        }
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}

/// Format a dollar amount as `"$1.23"` (two decimal places).
fn rhai_format_cost_usd(dollars: f64) -> String {
    format!("${dollars:.2}")
}

/// Format a token count as `"1.2k"` / `"3.5M"` / `"999"`. rhai's
/// integer type is i64; we clamp negatives to 0 and format unsigned
/// magnitudes.
///
/// The `M` threshold is set slightly below 1_000_000 (at 999_500) so
/// that one-decimal rounding of the `k` branch can't produce the
/// nonsensical `"1000.0k"` for values like 999_950.
fn rhai_format_tokens(count: i64) -> String {
    let n = count.max(0);
    if n >= 999_500 {
        let m = n as f64 / 1_000_000.0;
        format!("{m:.1}M")
    } else if n >= 1_000 {
        let k = n as f64 / 1_000.0;
        format!("{k:.1}k")
    } else {
        format!("{n}")
    }
}

/// Format an RFC 3339 timestamp string as a coarse countdown relative
/// to now (`"2h 13m"` / `"45m"` / `"6d"` / `"now"`). Parse failures
/// surface as the literal `"?"` so the statusline degrades visibly.
fn rhai_format_countdown_until(rfc3339_ts: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(rfc3339_ts) {
        Ok(dt) => crate::segments::format_countdown_until(dt.with_timezone(&Utc), Utc::now()),
        Err(_) => "?".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_evaluates_basic_arithmetic() {
        let engine = build_engine();
        let n: i64 = engine.eval("1 + 2").expect("eval ok");
        assert_eq!(n, 3);
    }

    #[test]
    fn infinite_loop_trips_operation_limit() {
        let engine = build_engine();
        let err = engine.eval::<()>("loop {}").unwrap_err();
        // Rhai wraps this as a `NumberOfOperations` variant; the
        // message contains the limit literal.
        assert!(
            format!("{err}").contains("operations"),
            "expected operation-limit error, got: {err}"
        );
    }

    /// RAII guard for the per-render thread-locals so a test panic
    /// can't leak state into siblings on the same thread. The
    /// production [`super::super::segment::RhaiSegment::render`] uses
    /// the same pattern.
    struct ThreadLocalGuard;

    impl ThreadLocalGuard {
        fn install_deadline(at: Instant) -> Self {
            set_render_deadline(Some(at));
            Self
        }

        fn install_plugin_id(id: &str) -> Self {
            set_current_plugin_id(Some(id));
            Self
        }
    }

    impl Drop for ThreadLocalGuard {
        fn drop(&mut self) {
            set_render_deadline(None);
            set_current_plugin_id(None);
        }
    }

    #[test]
    fn past_deadline_aborts_long_running_script() {
        let engine = build_engine();
        let _guard = ThreadLocalGuard::install_deadline(Instant::now());
        let err = engine.eval::<()>("loop {}").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("terminated"),
            "expected `Script terminated` from on_progress abort, got: {msg}"
        );
    }

    #[test]
    fn far_future_deadline_does_not_abort_quick_script() {
        let engine = build_engine();
        let _guard = ThreadLocalGuard::install_deadline(
            Instant::now() + std::time::Duration::from_secs(3600),
        );
        let n: i64 = engine.eval("1 + 2 + 3").expect("quick eval ok");
        assert_eq!(n, 6);
    }

    #[test]
    fn no_deadline_set_does_not_abort_quick_script() {
        // Belt-and-suspenders: ensure cleared deadlines don't abort
        // normal evaluation. A leak from a prior test on this thread
        // would surface here as an unexpected error.
        set_render_deadline(None);
        let engine = build_engine();
        let n: i64 = engine.eval("4 * 5").expect("eval ok");
        assert_eq!(n, 20);
    }

    #[test]
    fn log_emits_first_call_then_silences() {
        // Pin the per-plugin rate-limit: three `log()` calls under
        // the same id collapse to exactly LOG_LINES_PER_PLUGIN
        // emissions. Reset first so cross-test ordering on this
        // thread can't preload the counter.
        reset_log_counts();
        let engine = build_engine();
        let _guard = ThreadLocalGuard::install_plugin_id("log_emits_first_call_then_silences");
        engine
            .eval::<()>(r#"log("first"); log("second"); log("third");"#)
            .expect("eval ok");
        let count = LOG_EMITTED.with(|cell| {
            cell.borrow()
                .get("log_emits_first_call_then_silences")
                .copied()
                .unwrap_or(0)
        });
        assert_eq!(
            count, LOG_LINES_PER_PLUGIN,
            "expected exactly {LOG_LINES_PER_PLUGIN} emission(s), counted {count}"
        );
    }

    #[test]
    fn log_under_distinct_plugin_ids_each_gets_its_own_quota() {
        reset_log_counts();
        let engine = build_engine();
        for id in ["log_quota_a", "log_quota_b"] {
            let _guard = ThreadLocalGuard::install_plugin_id(id);
            engine.eval::<()>(r#"log("hi");"#).expect("eval ok");
        }
        let counts = LOG_EMITTED.with(|cell| {
            let map = cell.borrow();
            (
                map.get("log_quota_a").copied().unwrap_or(0),
                map.get("log_quota_b").copied().unwrap_or(0),
            )
        });
        assert_eq!(counts, (LOG_LINES_PER_PLUGIN, LOG_LINES_PER_PLUGIN));
    }

    #[test]
    fn log_outside_render_attributes_to_unscoped_bucket() {
        // Pin the sentinel id used when CURRENT_PLUGIN_ID is unset so
        // a future rename (`<none>`, `<anon>`) doesn't silently
        // scatter eval-callsite logs across new buckets.
        reset_log_counts();
        let engine = build_engine();
        engine.eval::<()>(r#"log("from-eval");"#).expect("eval ok");
        let count = LOG_EMITTED.with(|cell| cell.borrow().get("<unscoped>").copied());
        assert_eq!(count, Some(LOG_LINES_PER_PLUGIN));
    }

    #[test]
    fn import_is_disabled() {
        let engine = build_engine();
        // `import "foo"` would normally parse; disabling the symbol
        // turns it into a parse error.
        let err = engine.eval::<()>(r#"import "foo" as bar;"#).unwrap_err();
        assert!(
            format!("{err}").to_lowercase().contains("import"),
            "expected import-related error, got: {err}"
        );
    }

    #[test]
    fn eval_symbol_is_disabled() {
        let engine = build_engine();
        let err = engine.eval::<()>(r#"eval("1 + 1")"#).unwrap_err();
        assert!(
            format!("{err}").to_lowercase().contains("eval"),
            "expected eval-related error, got: {err}"
        );
    }

    #[test]
    fn unregistered_fs_call_fails_at_runtime() {
        // We register nothing filesystem-related. A plugin that calls
        // `fs::read("/etc/passwd")` hits "function not found."
        let engine = build_engine();
        let err = engine.eval::<()>(r#"fs::read("/etc/passwd")"#).unwrap_err();
        let msg = format!("{err}").to_lowercase();
        assert!(
            msg.contains("fs::read") || msg.contains("not found") || msg.contains("function"),
            "expected function-not-found error, got: {err}"
        );
    }

    #[test]
    fn print_and_debug_are_silent_no_ops() {
        // `print` / `debug` are rhai built-ins that route through the
        // engine's on_print / on_debug callbacks. Our builder points
        // both at no-op closures so plugin scripts can call them
        // without crashing AND without reaching the host's stdout or
        // stderr. `Engine::new()` defaults would leak to stdout; this
        // test pins the no-op routing as the required posture.
        let engine = build_engine();
        // Eval returns Ok(()) — the call succeeded but produced
        // nothing the host can observe.
        engine
            .eval::<()>(
                r#"print("this would leak to stdout under Engine::new"); debug("this too");"#,
            )
            .expect("print/debug call must succeed as a no-op");
    }

    #[test]
    fn format_duration_sub_minute_renders_seconds() {
        assert_eq!(rhai_format_duration(45_000), "45s");
    }

    #[test]
    fn format_duration_negative_clamps_to_zero() {
        assert_eq!(rhai_format_duration(-1), "0s");
    }

    #[test]
    fn format_duration_renders_hours_and_minutes() {
        assert_eq!(rhai_format_duration(3_600_000 + 23 * 60 * 1000), "1h 23m");
    }

    #[test]
    fn format_duration_renders_minutes_only_under_an_hour() {
        assert_eq!(rhai_format_duration(12 * 60 * 1000), "12m");
    }

    #[test]
    fn format_duration_drops_minutes_on_round_hour() {
        assert_eq!(rhai_format_duration(2 * 3_600_000), "2h");
    }

    #[test]
    fn format_cost_usd_two_decimals() {
        assert_eq!(rhai_format_cost_usd(1.234), "$1.23");
        assert_eq!(rhai_format_cost_usd(0.0), "$0.00");
    }

    #[test]
    fn format_tokens_under_1k_renders_literal() {
        assert_eq!(rhai_format_tokens(42), "42");
        assert_eq!(rhai_format_tokens(0), "0");
    }

    #[test]
    fn format_tokens_thousands_get_k_suffix() {
        assert_eq!(rhai_format_tokens(1200), "1.2k");
    }

    #[test]
    fn format_tokens_millions_get_m_suffix() {
        assert_eq!(rhai_format_tokens(3_500_000), "3.5M");
    }

    #[test]
    fn format_tokens_negative_clamps_to_zero() {
        assert_eq!(rhai_format_tokens(-5), "0");
    }

    #[test]
    fn format_countdown_until_bad_rfc3339_renders_marker() {
        assert_eq!(rhai_format_countdown_until("not a timestamp"), "?");
    }

    #[test]
    fn format_countdown_until_past_timestamp_says_now() {
        // 2001-09-09 is safely in the past forever.
        assert_eq!(rhai_format_countdown_until("2001-09-09T01:46:40Z"), "now");
    }

    #[test]
    fn host_format_cost_usd_invokable_from_script() {
        let engine = build_engine();
        let s: String = engine.eval(r#"format_cost_usd(1.99)"#).expect("eval ok");
        assert_eq!(s, "$1.99");
    }

    #[test]
    fn host_format_tokens_invokable_from_script() {
        let engine = build_engine();
        let s: String = engine.eval(r#"format_tokens(1500)"#).expect("eval ok");
        assert_eq!(s, "1.5k");
    }

    #[test]
    fn host_log_invokable_from_script() {
        // Smoke test: `log` is registered and callable. Actual stderr
        // capture lives in the segment wrapper that owns output
        // routing.
        let engine = build_engine();
        engine
            .eval::<()>(r#"log("hello from rhai");"#)
            .expect("eval ok");
    }

    #[test]
    fn host_format_duration_invokable_from_script() {
        let engine = build_engine();
        let s: String = engine.eval(r#"format_duration(45000)"#).expect("eval ok");
        assert_eq!(s, "45s");
    }

    #[test]
    fn host_format_countdown_until_invokable_from_script() {
        let engine = build_engine();
        let s: String = engine
            .eval(r#"format_countdown_until("2001-09-09T01:46:40Z")"#)
            .expect("eval ok");
        assert_eq!(s, "now");
    }

    #[test]
    fn host_format_cost_usd_accepts_integer_literal() {
        // Regression guard for the i64 overload: plugins writing
        // `format_cost_usd(1)` must not hit "function not found."
        let engine = build_engine();
        let s: String = engine.eval(r#"format_cost_usd(2)"#).expect("eval ok");
        assert_eq!(s, "$2.00");
    }

    #[test]
    fn format_tokens_boundary_at_exactly_1000() {
        // `>= 1_000` branch triggers at 1000 exactly; guard against a
        // future refactor to `>` that would silently regress.
        assert_eq!(rhai_format_tokens(1_000), "1.0k");
    }

    #[test]
    fn format_tokens_boundary_at_exactly_1_000_000() {
        assert_eq!(rhai_format_tokens(1_000_000), "1.0M");
    }

    #[test]
    fn standard_package_string_helpers_work() {
        // The spec's sample plugin (plugin-api.md §Plugin script
        // contract) uses `model_name.len()`. If StandardPackage isn't
        // loaded, that call fails with "function not found."
        let engine = build_engine();
        let len: i64 = engine.eval(r#""hello".len()"#).expect("eval ok");
        assert_eq!(len, 5);
    }

    #[test]
    fn standard_package_array_helpers_work() {
        let engine = build_engine();
        let n: i64 = engine
            .eval(r#"let xs = [1, 2, 3]; xs.len()"#)
            .expect("eval ok");
        assert_eq!(n, 3);
    }

    #[test]
    fn format_tokens_near_million_boundary_rolls_to_m() {
        // Regression guard for rounding drift: 999_950 / 1_000 rounds
        // to 1000.0, which would display as "1000.0k" without the
        // boundary guard. Must roll to "1.0M" instead.
        assert_eq!(rhai_format_tokens(999_950), "1.0M");
        assert_eq!(rhai_format_tokens(999_999), "1.0M");
    }

    #[test]
    fn format_tokens_just_below_rollover_boundary_stays_k() {
        // Paired with the above: 999_499 is comfortably under the
        // M threshold; must format as "999.5k".
        assert_eq!(rhai_format_tokens(999_499), "999.5k");
    }

    #[test]
    fn format_countdown_until_future_timestamp_renders_duration() {
        // Exercises the RFC 3339 → DateTime → countdown path end-to-
        // end (not just the bad-input fallback). Shape assertion only
        // so the test isn't time-sensitive: result is neither "?"
        // (parse failure) nor "now" (for a timestamp > 1 minute out).
        let target = chrono::Utc::now() + chrono::Duration::hours(2);
        let rendered = rhai_format_countdown_until(&target.to_rfc3339());
        assert_ne!(rendered, "?", "expected successful parse + format");
        assert_ne!(rendered, "now", "expected future-duration output");
        assert!(!rendered.is_empty());
    }
}
