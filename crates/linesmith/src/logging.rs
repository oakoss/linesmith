//! In-crate structured-logging facade.
//!
//! The statusline is a single-shot, stderr-free-by-default process; a
//! full `tracing`/`log` stack would bloat the binary for a narrow
//! diagnostic surface. This module exposes level-gated emission
//! through two macros, [`lsm_warn!`] and [`lsm_debug!`], controlled by
//! the [`LINESMITH_LOG`](ENV_VAR) env var.
//!
//! Default level is [`Level::Warn`] so genuine drops (cache-write
//! failures, lock-write failures) surface without opt-in. Silent
//! [`Ok(None)`] hide paths in rate-limit segments log at
//! [`Level::Debug`] and require `LINESMITH_LOG=debug` to appear.
//!
//! Structural failures that always warrant a user-visible signal
//! (segment render panics, fatal plugin init errors) emit through
//! [`lsm_error!`], which bypasses the level gate. `LINESMITH_LOG=off`
//! quiets chatter; it is not a silence-all-signals switch, because a
//! broken statusline needs a stderr line the user can grep. Scripts
//! that want absolute silence can `2>/dev/null`.
//!
//! Output format: `linesmith [<level>]: <message>`. A future doctor
//! command can buffer and reformat the same emissions.
//!
//! Not a general-purpose logger: no filtering by target, no structured
//! fields, no sink swap. Add those when a call site needs them.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU8, Ordering};

/// Logger severity. Variants are ordered `Off < Warn < Debug` so a
/// call fires when its own level is `<=` the configured level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Off = 0,
    /// Default. Real data-loss drops (cache/lock write failures,
    /// gix-discovery errors) surface without opt-in.
    Warn = 1,
    /// Opt-in verbosity. Every `Ok(None)` hide path in a segment
    /// emits a one-line diagnostic naming the gate that triggered.
    Debug = 2,
}

// The `AtomicU8` store round-trips raw discriminants through
// `set_level` / `level()`. Pin the layout so a reorder of the variants
// becomes a compile error instead of a silent flip.
const _: () = assert!(Level::Off as u8 == 0);
const _: () = assert!(Level::Warn as u8 == 1);
const _: () = assert!(Level::Debug as u8 == 2);

pub const ENV_VAR: &str = "LINESMITH_LOG";

const DEFAULT_LEVEL: Level = Level::Warn;
static LEVEL: AtomicU8 = AtomicU8::new(DEFAULT_LEVEL as u8);

/// Apply `raw` (the snapshotted `LINESMITH_LOG` value, `None` if
/// unset) to the process-wide level. On an unrecognized value the
/// logger resets to [`DEFAULT_LEVEL`] and writes one line to
/// `warn_sink`; the driver threads the injected CLI stderr through so
/// tests and embedders don't see ambient stderr pollution.
pub fn apply(raw: Option<&str>, warn_sink: &mut dyn Write) {
    match decide_init(raw) {
        InitDecision::Keep => {}
        InitDecision::Set(l) => set_level(l),
        InitDecision::Warn(bad) => {
            let _ = writeln!(
                warn_sink,
                "linesmith: {ENV_VAR}={bad:?} unrecognized; using default ({DEFAULT_LEVEL:?})"
            );
            set_level(DEFAULT_LEVEL);
        }
    }
}

/// Pure decision form of [`apply`]: what to do given the raw env-var
/// read. Split out so the decision tree is unit-testable without
/// touching the process env.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InitDecision<'a> {
    /// Env var unset; leave the logger at its prior level.
    Keep,
    /// Env var parsed; call [`set_level`] with this value.
    Set(Level),
    /// Env var set but unparseable.
    Warn(&'a str),
}

pub(crate) fn decide_init(raw: Option<&str>) -> InitDecision<'_> {
    match raw {
        None => InitDecision::Keep,
        Some(s) => match Level::parse(s) {
            Some(l) => InitDecision::Set(l),
            None => InitDecision::Warn(s),
        },
    }
}

/// Override the process-wide level. Exposed for tests and embedders
/// that want to pick a level without touching the env.
pub fn set_level(l: Level) {
    LEVEL.store(l as u8, Ordering::Relaxed);
}

#[must_use]
pub fn level() -> Level {
    from_u8(LEVEL.load(Ordering::Relaxed))
}

/// Reconstruct a [`Level`] from its stored byte. An out-of-range byte
/// is a store bug; `debug_assert!` surfaces it in tests while release
/// builds saturate to the verbose end so nothing is accidentally
/// suppressed.
fn from_u8(n: u8) -> Level {
    match n {
        0 => Level::Off,
        1 => Level::Warn,
        2 => Level::Debug,
        _ => {
            debug_assert!(false, "logging::LEVEL holds out-of-range byte {n}");
            Level::Debug
        }
    }
}

/// `true` when `at_least` or a more verbose level is active. A call
/// fires when its own level is `<=` the currently configured level.
#[must_use]
pub fn is_enabled(at_least: Level) -> bool {
    level() >= at_least
}

/// Emit `msg` at `lvl` when the configured level allows. A broken
/// stderr (closed pipe, full disk) drops the write silently — the
/// statusline has no recovery path and panicking would nuke an
/// otherwise-good render.
pub fn emit(lvl: Level, msg: &str) {
    if !is_enabled(lvl) {
        return;
    }
    let tag = match lvl {
        Level::Off => return,
        Level::Warn => "warn",
        Level::Debug => "debug",
    };
    let _ = writeln!(io::stderr().lock(), "linesmith [{tag}]: {msg}");
}

/// Emit a structural-failure diagnostic. Bypasses the level gate:
/// even `LINESMITH_LOG=off` does not suppress it, because the only
/// things that reach this function are render failures a user has no
/// other way of seeing.
pub fn emit_error(msg: &str) {
    emit_error_to(msg, &mut io::stderr().lock());
}

/// Same as [`emit_error`] but to a caller-supplied sink. Separate
/// from `emit_error` so unit tests can assert the message without
/// capturing process stderr.
pub(crate) fn emit_error_to(msg: &str, sink: &mut dyn Write) {
    let _ = writeln!(sink, "linesmith [error]: {msg}");
}

impl Level {
    /// Parse the [`ENV_VAR`] string. Accepts `warn` → `Warn`, `debug`
    /// / `trace` / `all` → `Debug`, `off` / `none` / `0` → `Off`.
    /// `error` and `info` are rejected on purpose: the 3-level ladder
    /// has no `Error` slot, and silently collapsing `error` to `warn`
    /// would ship `LINESMITH_LOG=error` users every warn-level line.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "0" => Some(Level::Off),
            "warn" | "warning" => Some(Level::Warn),
            "debug" | "trace" | "all" => Some(Level::Debug),
            _ => None,
        }
    }
}

/// Emit a warning-level diagnostic. Fires at [`DEFAULT_LEVEL`] and up.
#[macro_export]
macro_rules! lsm_warn {
    ($($arg:tt)*) => {
        $crate::logging::emit($crate::logging::Level::Warn, &format!($($arg)*))
    };
}

/// Emit a debug-level diagnostic. Gated behind `LINESMITH_LOG=debug`;
/// the `format!` call is skipped when suppressed.
#[macro_export]
macro_rules! lsm_debug {
    ($($arg:tt)*) => {
        if $crate::logging::is_enabled($crate::logging::Level::Debug) {
            $crate::logging::emit($crate::logging::Level::Debug, &format!($($arg)*));
        }
    };
}

/// Emit a structural-failure diagnostic that bypasses the level gate.
/// Reserved for failures a user has no other way of seeing — segment
/// render errors, fatal plugin init, contract violations — so a user
/// who set `LINESMITH_LOG=off` still sees "the statusline broke."
#[macro_export]
macro_rules! lsm_error {
    ($($arg:tt)*) => {
        $crate::logging::emit_error(&format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests that mutate LEVEL run serially — parallel cargo-test would
    // otherwise flake when two tests disagree on the expected level.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        M.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn default_level_is_warn() {
        let _g = lock();
        set_level(DEFAULT_LEVEL);
        assert_eq!(level(), Level::Warn);
        assert!(is_enabled(Level::Warn));
        assert!(!is_enabled(Level::Debug));
    }

    #[test]
    fn debug_enables_every_lower_level() {
        let _g = lock();
        set_level(Level::Debug);
        assert!(is_enabled(Level::Warn));
        assert!(is_enabled(Level::Debug));
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn off_suppresses_every_level() {
        let _g = lock();
        set_level(Level::Off);
        assert!(!is_enabled(Level::Warn));
        assert!(!is_enabled(Level::Debug));
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn parse_accepts_common_aliases() {
        assert_eq!(Level::parse("warn"), Some(Level::Warn));
        assert_eq!(Level::parse("WARN"), Some(Level::Warn));
        assert_eq!(Level::parse(" warn "), Some(Level::Warn));
        assert_eq!(Level::parse("warning"), Some(Level::Warn));
        assert_eq!(Level::parse("debug"), Some(Level::Debug));
        assert_eq!(Level::parse("trace"), Some(Level::Debug));
        assert_eq!(Level::parse("all"), Some(Level::Debug));
        assert_eq!(Level::parse("off"), Some(Level::Off));
        assert_eq!(Level::parse("none"), Some(Level::Off));
        assert_eq!(Level::parse("0"), Some(Level::Off));
    }

    #[test]
    fn parse_rejects_error_and_info_aliases() {
        // `error` / `info` reject intentionally — no Error variant
        // exists to route them to, and silently promoting either to
        // `warn` would mislead a user asking for errors-only.
        assert_eq!(Level::parse("error"), None);
        assert_eq!(Level::parse("info"), None);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(Level::parse("verbose"), None);
        assert_eq!(Level::parse(""), None);
        assert_eq!(Level::parse("debug2"), None);
    }

    #[test]
    fn decide_init_keeps_default_when_env_unset() {
        assert_eq!(decide_init(None), InitDecision::Keep);
    }

    #[test]
    fn decide_init_parses_recognized_levels() {
        assert_eq!(decide_init(Some("debug")), InitDecision::Set(Level::Debug));
        assert_eq!(decide_init(Some("warn")), InitDecision::Set(Level::Warn));
        assert_eq!(decide_init(Some("off")), InitDecision::Set(Level::Off));
    }

    #[test]
    fn decide_init_warns_on_garbage() {
        assert_eq!(decide_init(Some("loud")), InitDecision::Warn("loud"));
        assert_eq!(decide_init(Some("")), InitDecision::Warn(""));
    }

    #[test]
    fn apply_writes_warning_to_injected_sink_and_resets_to_default() {
        let _g = lock();
        set_level(Level::Off);
        let mut sink = Vec::<u8>::new();
        apply(Some("loud"), &mut sink);
        let written = String::from_utf8(sink).expect("utf8");
        assert!(
            written.contains("LINESMITH_LOG=\"loud\""),
            "expected the unrecognized value echoed, got {written:?}"
        );
        assert!(written.contains("unrecognized"));
        // Garbage must reset to DEFAULT_LEVEL so a stale prior
        // set_level(Off) doesn't persist.
        assert_eq!(level(), DEFAULT_LEVEL);
    }

    #[test]
    fn apply_keeps_level_when_env_unset() {
        let _g = lock();
        set_level(Level::Debug);
        let mut sink = Vec::<u8>::new();
        apply(None, &mut sink);
        assert!(sink.is_empty(), "no-env must not write: {sink:?}");
        assert_eq!(level(), Level::Debug);
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn apply_sets_recognized_level_without_writing() {
        let _g = lock();
        set_level(Level::Off);
        let mut sink = Vec::<u8>::new();
        apply(Some("debug"), &mut sink);
        assert!(sink.is_empty());
        assert_eq!(level(), Level::Debug);
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn lsm_debug_skips_format_when_suppressed() {
        // Pin the documented gating: `format!` arg-eval must be
        // skipped when debug is off. Regression would silently
        // reintroduce allocation cost the macro exists to avoid.
        use std::cell::Cell;
        use std::fmt;
        struct CountingDisplay<'a>(&'a Cell<u32>);
        impl fmt::Display for CountingDisplay<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.set(self.0.get() + 1);
                f.write_str("x")
            }
        }

        let _g = lock();
        let counter = Cell::new(0u32);

        set_level(Level::Warn);
        lsm_debug!("{}", CountingDisplay(&counter));
        assert_eq!(counter.get(), 0, "format! must not run when suppressed");

        set_level(Level::Debug);
        lsm_debug!("{}", CountingDisplay(&counter));
        assert_eq!(counter.get(), 1, "format! must run when enabled");

        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn from_u8_roundtrips_known_bytes() {
        assert_eq!(from_u8(0), Level::Off);
        assert_eq!(from_u8(1), Level::Warn);
        assert_eq!(from_u8(2), Level::Debug);
    }

    #[test]
    #[should_panic(expected = "out-of-range byte")]
    fn from_u8_debug_panics_on_out_of_range() {
        // Only covered in debug builds; release saturates to Debug.
        let _ = from_u8(99);
    }

    #[test]
    fn emit_error_bypasses_off_level() {
        let _g = lock();
        set_level(Level::Off);
        let mut sink = Vec::<u8>::new();
        emit_error_to("render panic", &mut sink);
        let written = String::from_utf8(sink).expect("utf8");
        assert_eq!(written, "linesmith [error]: render panic\n");
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn emit_error_fires_at_every_level() {
        let _g = lock();
        for l in [Level::Off, Level::Warn, Level::Debug] {
            set_level(l);
            let mut sink = Vec::<u8>::new();
            emit_error_to("x", &mut sink);
            assert!(
                !sink.is_empty(),
                "emit_error must fire at level {l:?}, got empty sink"
            );
        }
        set_level(DEFAULT_LEVEL);
    }
}
