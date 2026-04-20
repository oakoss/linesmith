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

use std::sync::Arc;

use chrono::Utc;
use rhai::packages::{Package, StandardPackage};
use rhai::Engine;

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

/// Build the shared rhai engine used by every plugin segment. Returns
/// an `Arc` so the layout engine can clone cheaply into each
/// `RhaiSegment`. The engine is immutable after this call.
#[must_use]
pub fn build_engine() -> Arc<Engine> {
    let mut engine = Engine::new_raw();
    // `new_raw()` starts with nothing registered. Load rhai's
    // StandardPackage so common script helpers (`str.len()`,
    // `arr.push(x)`, `map.keys()`, arithmetic, iterators, …) are
    // available to non-trivial plugins.
    engine.register_global_module(StandardPackage::new().as_shared_module());
    // `print` / `debug` are built-in rhai statements whose output
    // routes through the engine's on_print / on_debug callbacks.
    // `Engine::new()` defaults them to stdout / stderr — a leak for
    // untrusted plugin code. Point both at no-ops so plugin authors
    // can call them without crashing but nothing reaches the host's
    // stdout / stderr.
    engine.on_print(|_| {});
    engine.on_debug(|_, _, _| {});
    configure_limits(&mut engine);
    lock_down_symbols(&mut engine);
    register_host_fns(&mut engine);
    Arc::new(engine)
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

/// Host-registered `log(msg)` for plugin scripts. Writes the message
/// to stderr prefixed with `linesmith plugin: `. Per-plugin rate
/// limiting is the segment wrapper's job — it owns the plugin id and
/// per-run counters; this function is the raw sink.
fn rhai_log(msg: &str) {
    eprintln!("linesmith plugin: {msg}");
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

    // --- host fn formatting ---------------------------------------

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

    // --- host fns callable from scripts ----------------------------

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

    // --- host fn boundary coverage --------------------------------

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
