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
//! Output format on stderr: `linesmith [<level>]: <message>`. The
//! TUI installs a [`CapturedSink`] in place of [`StderrSink`] for
//! the duration of the alt-screen so macro emissions don't paint
//! over the rendered frame; captured entries use the compact
//! `[<level>] <message>` form (no `linesmith` prefix, no colon)
//! since the surrounding UI provides context.
//!
//! Not a general-purpose logger: no filtering by target, no
//! structured fields. Add those when a call site needs them.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::mem;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

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

/// Pluggable destination for diagnostic emissions. The default
/// [`StderrSink`] preserves the `linesmith [<level>]: <msg>` format
/// external scripts grep for; the TUI swaps in a [`CapturedSink`]
/// for the duration of the alt-screen so macro emissions land in
/// the warnings panel instead of corrupting the painted frame.
///
/// `&self` (not `&mut self`) so a sink instance can be shared via
/// `Arc<dyn LogSink>`. The `Send + Sync` bound is what `Arc<dyn _>`
/// requires and lets background plugin emitters share the slot
/// safely with the render thread. The active-sink slot is an
/// `RwLock`: emits take read locks (concurrent) and `install_sink`
/// takes the write lock, which waits for in-flight emits to drain
/// before swapping. This applies only to dispatches that go
/// through [`emit`] / [`emit_error`] — a thread that holds its own
/// `Arc<dyn LogSink>` (the prior returned by `install_sink`, or a
/// future plugin handle) can still drive that sink directly after
/// a swap.
///
/// Implementations must not call back into [`emit`] / [`emit_error`]
/// or the `lsm_warn!` / `lsm_debug!` / `lsm_error!` macros from
/// within their own `emit` / `emit_error`. The slot's read lock is
/// held across the dispatch, and `std::sync::RwLock`'s reentrancy
/// is platform-defined — a recursive read can deadlock against a
/// pending `install_sink` on writer-preferring runtimes (musl,
/// Windows SRW). lsm-261p tracks evaluating a fair RwLock so this
/// invariant can relax.
pub trait LogSink: Send + Sync {
    /// Emit a level-gated diagnostic. The caller has already
    /// confirmed the level is enabled — the sink writes
    /// unconditionally. Implementations defensively no-op on
    /// `Level::Off` rather than relying on the caller; the macros
    /// never pass `Off`, but `emit()` is `pub`.
    fn emit(&self, lvl: Level, msg: &str);

    /// Emit a structural-failure diagnostic. Always fires regardless
    /// of the configured level — `LINESMITH_LOG=off` users still
    /// see "the statusline broke" because there's no other channel.
    fn emit_error(&self, msg: &str);
}

/// Default sink. Writes `linesmith [<tag>]: <msg>` lines to
/// `io::stderr().lock()` and drops the write on a closed pipe /
/// full disk: the statusline has no recovery path, and panicking
/// here would nuke an otherwise-good render.
#[derive(Debug, Default)]
pub struct StderrSink;

impl LogSink for StderrSink {
    fn emit(&self, lvl: Level, msg: &str) {
        let tag = match lvl {
            Level::Off => return,
            Level::Warn => "warn",
            Level::Debug => "debug",
        };
        let _ = writeln!(io::stderr().lock(), "linesmith [{tag}]: {msg}");
    }

    fn emit_error(&self, msg: &str) {
        let _ = writeln!(io::stderr().lock(), "linesmith [error]: {msg}");
    }
}

/// Default cap for the [`CapturedSink`] inter-frame ring. Sized for a
/// chatty render (one warn per segment across a multi-line config) plus
/// burst headroom from future background emitters. Each entry is a
/// short formatted string, so the worst-case memory at the cap is on
/// the order of tens of KiB — small enough to be permanently allocated
/// for the alt-screen lifetime.
pub const CAPTURED_SINK_DEFAULT_CAP: usize = 256;

/// One captured emission, tagged with the [`Instant`] it landed at the
/// sink. Consumers that display the buffer over time (the TUI warnings
/// panel once v0.2 background plugins can emit between draws) compute
/// relative age against `Instant::now()` at render time; the monotonic
/// clock means a host suspending mid-session doesn't show negative
/// ages or jump backwards.
///
/// `text` carries the existing compact form (`[<tag>] <msg>`); the age
/// is metadata, not embedded in the text, so non-temporal renderers
/// (boot-failure stderr flush) can ignore the timestamp entirely.
#[derive(Debug, Clone)]
pub struct CapturedEntry {
    pub at: Instant,
    pub text: String,
}

/// Detailed drain result. Carries the entries plus a `dropped` count
/// for emissions silently evicted by the bounded buffer since the
/// previous drain, so the consumer can surface a "(N earlier entries
/// dropped)" line instead of letting the loss go unnoticed.
#[derive(Debug, Default)]
pub struct CapturedDrain {
    pub entries: Vec<CapturedEntry>,
    pub dropped: usize,
}

/// Buffering sink that accumulates formatted entries for an
/// interactive consumer to drain. The TUI installs one for the
/// alt-screen lifetime so `lsm_warn!` / `lsm_error!` / `lsm_debug!`
/// surface in the live-preview warnings panel instead of painting
/// over the rendered frame.
///
/// Captured format is `[<tag>] <msg>` — the surrounding UI prefixes
/// each line with its own marker (e.g. `⚠`), so the `linesmith`
/// prefix and colon would be redundant noise.
///
/// The buffer is bounded at [`CAPTURED_SINK_DEFAULT_CAP`] with
/// drop-oldest eviction and a counter exposed via [`Self::drain_detailed`].
/// Today every emit happens during a synchronous render so drain
/// follows within microseconds; once v0.2 background plugins emit
/// between draws, the cap is what keeps a long-idle session from
/// accumulating megabytes of unread diagnostics. The [`Self::emit_seq`]
/// counter lets an idle event loop notice a new emission and trigger
/// a redraw without polling the mutex on every tick.
#[derive(Debug)]
pub struct CapturedSink {
    entries: Mutex<VecDeque<CapturedEntry>>,
    cap: usize,
    /// Entries evicted since the last drain. Reset on every drain
    /// (legacy `drain` routes through `drain_detailed` and so resets
    /// it too) — only `drain_detailed` returns the count to the
    /// consumer, so callers that need eviction visibility must use
    /// it exclusively and not interleave with legacy `drain`.
    ///
    /// Updated under the `entries` mutex so a concurrent push's
    /// eviction increment can't race the drain's reset.
    dropped: AtomicUsize,
    /// Lifetime emission counter — monotonically increments on every
    /// `push`, never resets. Paired with `last_drain_seq` to drive the
    /// TUI's rising-edge wake (see the `emit_seq` method doc for the
    /// pre-draw / post-drain watermark math).
    emit_seq: AtomicUsize,
    /// Snapshot of `emit_seq` at the moment of the most recent drain.
    /// Lets the event loop tell apart "in-frame emits the render
    /// already drained and showed the user" from "background emits
    /// that arrived during the draw but were not drained" — the
    /// former bumps `emit_seq` AND `last_drain_seq` in lockstep, the
    /// latter advances only `emit_seq`. The watermark advances to
    /// `max(pre_draw_emit_seq, last_drain_seq)` so neither bites.
    last_drain_seq: AtomicUsize,
}

impl Default for CapturedSink {
    fn default() -> Self {
        Self::with_capacity(CAPTURED_SINK_DEFAULT_CAP)
    }
}

impl CapturedSink {
    /// Construct a sink with an explicit cap. A `cap` of zero is
    /// clamped to one — a literal zero-cap would drop every emission,
    /// leaving the consumer with nothing but a rising `dropped` counter.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            entries: Mutex::new(VecDeque::with_capacity(cap.min(64))),
            cap,
            dropped: AtomicUsize::new(0),
            emit_seq: AtomicUsize::new(0),
            last_drain_seq: AtomicUsize::new(0),
        }
    }

    /// Legacy text-only drain. Strips timestamps and dropped-count
    /// metadata; used by callers (boot-failure stderr flush, the
    /// `_test_capture_warns` helper) that only need the existing
    /// `[<tag>] <msg>` strings. Resets the pending counter so an
    /// event loop watching this sink stops requesting redraws.
    #[must_use]
    pub fn drain(&self) -> Vec<String> {
        let detailed = self.drain_detailed();
        detailed.entries.into_iter().map(|e| e.text).collect()
    }

    /// Structured drain. Returns every buffered entry with its
    /// emission timestamp and the number of entries evicted by the
    /// cap since the previous drain. Resets both counters.
    ///
    /// Counter resets happen while holding the `entries` mutex.
    /// A concurrent emit serializes on the same lock, so its
    /// increment is either fully observed by this drain (and
    /// reflected in the returned counts) or strictly ordered
    /// after the reset (and visible to the next drain) — never
    /// silently clobbered.
    #[must_use]
    pub fn drain_detailed(&self) -> CapturedDrain {
        let mut g = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let entries: Vec<CapturedEntry> = g.drain(..).collect();
        // Stamp last_drain_seq with the emit_seq value at this exact
        // moment so the event loop can tell drained-and-rendered
        // emissions apart from background emissions that arrived
        // after the drain. `emit_seq` is read under the mutex (same
        // critical section as the push side) so the snapshot reflects
        // every emit that contributed to the drained entries.
        let seq_at_drain = self.emit_seq.load(Ordering::Acquire);
        self.last_drain_seq.store(seq_at_drain, Ordering::Release);
        let dropped = self.dropped.swap(0, Ordering::AcqRel);
        CapturedDrain { entries, dropped }
    }

    /// Current cap. Lock-free read so consumers that render the
    /// drop-notice ("(N earlier entries dropped — cap M)") pick up
    /// the live value rather than baking in the `Default` constant.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Monotonic lifetime emission counter. The TUI event loop drives
    /// a rising-edge wake by comparing this against a watermark of
    /// `max(pre_draw_emit_seq, last_drain_seq)`:
    ///
    /// - **Pre-draw snapshot** keeps background emissions that arrive
    ///   *after* the in-frame drain on the unseen side of the watermark
    ///   — the next idle tick redraws them.
    /// - **`last_drain_seq`** lets in-frame emissions (the render
    ///   itself emitting warnings while building the frame) advance
    ///   the watermark — they were drained and shown to the user via
    ///   the warnings panel, so they shouldn't re-trigger a redraw.
    ///
    /// Acquire-load pairs with the Release store in `push` so on
    /// weakly-ordered architectures (ARM, RISC-V) the wake signal
    /// doesn't lag a poll tick.
    #[must_use]
    pub fn emit_seq(&self) -> usize {
        self.emit_seq.load(Ordering::Acquire)
    }

    /// `emit_seq` value at the moment of the most recent drain.
    /// Always `<= emit_seq()`. See [`Self::emit_seq`] for how the
    /// event loop combines the two into its watermark.
    #[must_use]
    pub fn last_drain_seq(&self) -> usize {
        self.last_drain_seq.load(Ordering::Acquire)
    }

    /// Test helper: assert against the buffer without consuming it.
    #[cfg(test)]
    fn snapshot(&self) -> Vec<String> {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|e| e.text.clone())
            .collect()
    }

    fn push(&self, text: String) {
        let entry = CapturedEntry {
            at: Instant::now(),
            text,
        };
        let mut g = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if g.len() == self.cap {
            g.pop_front();
            self.dropped.fetch_add(1, Ordering::Release);
        }
        g.push_back(entry);
        self.emit_seq.fetch_add(1, Ordering::Release);
    }
}

impl LogSink for CapturedSink {
    fn emit(&self, lvl: Level, msg: &str) {
        let tag = match lvl {
            Level::Off => return,
            Level::Warn => "warn",
            Level::Debug => "debug",
        };
        self.push(format!("[{tag}] {msg}"));
    }

    fn emit_error(&self, msg: &str) {
        self.push(format!("[error] {msg}"));
    }
}

/// Process-wide active sink. `OnceLock` because `Arc::new(...)`
/// allocates and `Arc::new` is not const-stable, so the slot can't
/// be a plain `static`. Init fires on first emission or first sink
/// swap, whichever comes first.
///
/// `RwLock` rather than `Mutex` so concurrent emits run without
/// blocking each other — emits take the read lock for the duration of
/// `sink.emit(...)`, `install_sink` takes the write lock and drains
/// in-flight emits before swapping. Closes the race where the old
/// `clone-the-Arc-then-emit` pattern let a concurrent install land
/// an emission on the just-orphaned slot.
///
/// Reader/writer fairness is platform-dependent on `std::sync::RwLock`.
/// Background emitters should not loop-emit without backoff: a
/// sustained read-side load can delay a writer (install) on a
/// reader-preferring runtime. See lsm-261p for the fair-RwLock
/// follow-up that will make this bound explicit.
static SINK: OnceLock<RwLock<Arc<dyn LogSink>>> = OnceLock::new();

/// Test-only serialization helper. Tests that install a custom
/// sink (or mutate `LEVEL`) must take this lock first — without
/// it, a parallel test installing its own sink can race the
/// active-sink slot and steal each other's emissions.
///
/// `#[doc(hidden)]` because this isn't part of the supported
/// public API. Cross-crate tests in this workspace use it; future
/// removal isn't a SemVer-breaking change. The leading underscore
/// signals "not for production code".
#[doc(hidden)]
pub fn _test_serial_lock() -> std::sync::MutexGuard<'static, ()> {
    static M: OnceLock<Mutex<()>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// RAII restorer for [`THREAD_SINK`]. Mirrors [`SinkGuard`] so the
/// thread-local resets on every exit path from [`_test_capture_warns`],
/// including unwinding panic under the dev/test profile.
#[must_use = "binding to `_` drops the guard immediately, which restores the prior thread-local sink before the helper's window opens; bind to a real name to hold it"]
struct ThreadSinkGuard {
    /// The prior thread-local value, which may itself be `Some(sink)`
    /// (nested helper) or `None` (first-time install). `Drop` takes
    /// the field out via `Option::take` and assigns it back into the
    /// thread-local; the field's outer `Option` is the borrow-checker
    /// affordance for that `take`, not a guard-active sentinel.
    prior: Option<Arc<CapturedSink>>,
}

impl Drop for ThreadSinkGuard {
    fn drop(&mut self) {
        // `cell.replace` returns the old value out from under the
        // borrow so its `Drop` runs after the `RefMut` is released —
        // matters if a future sink's Drop ever emits (which would
        // re-enter `with_thread_sink` and try to borrow this cell).
        let prior = self.prior.take();
        let _old = THREAD_SINK.with(|cell| cell.replace(prior));
    }
}

/// Test-only helper: run `f` with a thread-local [`CapturedSink`]
/// shadowing the active sink for the calling thread, then return
/// `f`'s result paired with the drained captured entries (captured-
/// sink format: `[warn] <msg>` for warns, `[error] <msg>` for
/// structural failures).
///
/// The thread-local sink is consulted **before** the process-wide
/// level gate inside [`emit`], so calls through [`emit`] or
/// [`emit_error`] (i.e. `lsm_warn!` / `lsm_error!`) capture regardless
/// of `LINESMITH_LOG` or a peer test's `set_level`. Peer threads stay
/// routed to the global sink, so parallel `cargo test` workers don't
/// pollute each other's warn counts.
///
/// **Caveat for `lsm_debug!`:** the macro pre-gates on
/// `is_enabled(Debug)` before calling `emit`, so a thread-local
/// capture won't see debug emissions unless the process-wide level
/// is `Debug`. Tests asserting debug output should hold
/// [`_test_serial_lock`] and `set_level(Level::Debug)` directly
/// rather than relying on the helper.
///
/// Holds [`_test_serial_lock`] for hermeticity with other tests that
/// mutate process-wide state through the same lock. **Not reentrant**:
/// a nested call on the same thread deadlocks at the serial lock —
/// move shared setup outside the closure rather than nesting helpers.
///
/// `#[doc(hidden)]` and leading-underscore for the same reasons
/// [`_test_serial_lock`] is: cross-crate test access without a
/// SemVer commitment.
#[doc(hidden)]
pub fn _test_capture_warns<F, T>(f: F) -> (T, Vec<String>)
where
    F: FnOnce() -> T,
{
    let _serial = _test_serial_lock();
    let sink = Arc::new(CapturedSink::default());
    let prior = THREAD_SINK.with(|cell| cell.replace(Some(sink.clone())));
    let _restore = ThreadSinkGuard { prior };
    let result = f();
    let captured = sink.drain();
    (result, captured)
}

fn sink_slot() -> &'static RwLock<Arc<dyn LogSink>> {
    SINK.get_or_init(|| RwLock::new(Arc::new(StderrSink)))
}

/// Replace the active sink and return the prior one. Use
/// [`SinkGuard`] for scoped install/restore; this raw function is
/// `pub(crate)` because the only documented in-tree caller is
/// `SinkGuard::install` itself. If a real out-of-crate embedder
/// shows up, promote it back to `pub` then.
///
/// Takes the slot's write lock, which blocks until every in-flight
/// `emit`/`emit_error` (each holding a read lock for its duration)
/// returns. Wait is bounded per-emission but cumulative — on a
/// reader-preferring `std::sync::RwLock` runtime, a sustained
/// background-emit load can keep the swap pending.
pub(crate) fn install_sink(new_sink: Arc<dyn LogSink>) -> Arc<dyn LogSink> {
    let mut g = sink_slot().write().unwrap_or_else(|p| p.into_inner());
    mem::replace(&mut *g, new_sink)
}

/// RAII handle that installs a custom sink and restores the prior
/// one on drop. The TUI uses this so stderr emission resumes after
/// the alt-screen exits in the normal-return path. Note: the
/// workspace's release profile sets `panic = "abort"`, so on a
/// release-build panic this `Drop` does **not** run — the panic
/// hook (in [`super::tui`]) is what restores the terminal under
/// abort, and stderr is owned by the alt-screen until then. Under
/// the default unwind profile (dev/test), stack unwinding drops
/// the guard normally.
///
/// Nested guards in nested scopes restore in reverse construction
/// order so long as Rust's normal stack-LIFO drop applies (no
/// explicit `mem::take` of the field, no moves into a heap-owned
/// container that delays drop). Don't move the guard into
/// `Box`/`Arc`/`Vec` and expect LIFO.
#[must_use = "binding to `_` drops the guard immediately, which restores the prior sink right away; bind to `_g` (or any real name) to hold it for the scope"]
pub struct SinkGuard {
    /// `Option` only so `Drop` can `take()` the prior out of
    /// `&mut self`. Invariant: always `Some` outside `Drop`. If a
    /// future `defuse(self) -> Arc<dyn LogSink>` method is added,
    /// it must consume `self` via `mem::forget` rather than
    /// `take()`-ing this field, or restore semantics silently
    /// regress.
    prior: Option<Arc<dyn LogSink>>,
}

impl SinkGuard {
    /// Install `new_sink` as the active sink, capturing the prior
    /// one for restoration on drop.
    pub fn install(new_sink: Arc<dyn LogSink>) -> Self {
        Self {
            prior: Some(install_sink(new_sink)),
        }
    }
}

impl Drop for SinkGuard {
    fn drop(&mut self) {
        if let Some(prior) = self.prior.take() {
            install_sink(prior);
        }
    }
}

// Per-thread sink overlay for testability. `None` in production;
// `_test_capture_warns` swaps in a thread-private CapturedSink for
// the duration of a test closure. Two concurrent captures installing
// the *global* SinkGuard would race the slot and see each other's
// emissions; the thread-local shadow makes the destination
// per-thread, so parallel `cargo test` workers each get their own
// captured set.
thread_local! {
    static THREAD_SINK: RefCell<Option<Arc<CapturedSink>>> = const { RefCell::new(None) };
}

/// Look up the calling thread's installed test sink, if any, and
/// invoke `f` on it. Clones the `Arc` out before calling `f` so the
/// `RefCell` borrow is released by the time `f` runs — a future sink
/// impl that recursed into `emit` would otherwise hit a borrow
/// panic. `try_with` returns false if the thread-local has already
/// been destroyed (TLS teardown ordering during thread exit), routing
/// the emission to the global sink rather than panicking.
#[must_use = "callers must skip the global sink when the thread-local fired, or the emission double-routes"]
fn with_thread_sink<F: FnOnce(&CapturedSink)>(f: F) -> bool {
    let sink = THREAD_SINK
        .try_with(|cell| cell.borrow().clone())
        .ok()
        .flatten();
    if let Some(sink) = sink {
        f(&sink);
        true
    } else {
        false
    }
}

/// Emit `msg` at `lvl` through the active [`LogSink`]. Production
/// path: the default sink writes to stderr; the TUI's
/// [`CapturedSink`] buffers for in-frame display; the level gate
/// suppresses below the configured threshold.
///
/// A thread-local capture (installed by [`_test_capture_warns`])
/// shadows the active sink **and** bypasses the level gate for the
/// calling thread — tests assert that a warn fires, not whether the
/// runtime gate happens to be open. Mirrors the existing always-fire
/// semantics of [`emit_error`].
pub fn emit(lvl: Level, msg: &str) {
    if with_thread_sink(|sink| sink.emit(lvl, msg)) {
        return;
    }
    if !is_enabled(lvl) {
        return;
    }
    let guard = sink_slot().read().unwrap_or_else(|p| p.into_inner());
    guard.emit(lvl, msg);
}

/// Emit a structural-failure diagnostic. Bypasses the level gate:
/// even `LINESMITH_LOG=off` does not suppress it, because the only
/// things that reach this function are render failures a user has no
/// other way of seeing.
pub fn emit_error(msg: &str) {
    if with_thread_sink(|sink| sink.emit_error(msg)) {
        return;
    }
    let guard = sink_slot().read().unwrap_or_else(|p| p.into_inner());
    guard.emit_error(msg);
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

    // Tests that mutate LEVEL or the active sink run serially —
    // parallel cargo-test would otherwise flake when two tests
    // disagree on the expected state. Wraps the same mutex
    // cross-crate tests use via `_test_serial_lock` so logging
    // tests and consumer tests (e.g. `tui::preview`) coordinate.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        super::_test_serial_lock()
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
    #[cfg(debug_assertions)]
    #[should_panic(expected = "out-of-range byte")]
    fn from_u8_debug_panics_on_out_of_range() {
        // Only covered in debug builds; release saturates to Debug.
        let _ = from_u8(99);
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn from_u8_saturates_out_of_range_to_debug_in_release() {
        // Out-of-range bytes saturate to Debug so nothing is suppressed.
        assert_eq!(from_u8(3), Level::Debug);
        assert_eq!(from_u8(99), Level::Debug);
        assert_eq!(from_u8(u8::MAX), Level::Debug);
    }

    #[test]
    fn captured_sink_records_warn_emit_with_compact_format() {
        // Pin the captured-sink shape: `[<tag>] <msg>` with no
        // `linesmith` prefix and no colon. The TUI warnings panel
        // already wraps each line with `⚠`, so the prefix is noise.
        let _g = lock();
        set_level(Level::Warn);
        let captured = Arc::new(CapturedSink::default());
        let _restore = SinkGuard::install(captured.clone());
        emit(Level::Warn, "hello");
        assert_eq!(captured.snapshot(), vec!["[warn] hello".to_string()]);
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn captured_sink_records_error_bypassing_off_level() {
        // emit_error always fires — including when the level gate
        // would otherwise suppress every emission. Pin both the
        // bypass and the `[error] msg` capture format.
        let _g = lock();
        set_level(Level::Off);
        let captured = Arc::new(CapturedSink::default());
        let _restore = SinkGuard::install(captured.clone());
        emit_error("render panic");
        assert_eq!(
            captured.snapshot(),
            vec!["[error] render panic".to_string()]
        );
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn captured_sink_skips_debug_emit_when_level_warn() {
        // The level gate runs in `emit()` *before* dispatching to
        // the sink. Pin that a Debug emission with the level set to
        // Warn never reaches the sink — otherwise `LINESMITH_LOG`
        // would silently stop gating once the TUI installed a
        // capture sink.
        let _g = lock();
        set_level(Level::Warn);
        let captured = Arc::new(CapturedSink::default());
        let _restore = SinkGuard::install(captured.clone());
        emit(Level::Debug, "verbose");
        assert!(captured.snapshot().is_empty());
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn emit_error_fires_at_every_level() {
        // emit_error bypasses the level gate at every setting, not
        // only Off. Pin Off + Warn + Debug so a future "optimize
        // emit_error to share the emit() short-circuit" refactor
        // breaks the Warn/Debug arms here, not only at Off.
        let _g = lock();
        for l in [Level::Off, Level::Warn, Level::Debug] {
            set_level(l);
            let captured = Arc::new(CapturedSink::default());
            let _restore = SinkGuard::install(captured.clone());
            emit_error("structural failure");
            assert_eq!(
                captured.snapshot(),
                vec!["[error] structural failure".to_string()],
                "emit_error must fire at level {l:?}",
            );
        }
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn captured_sink_drain_returns_entries_and_empties_buffer() {
        let _g = lock();
        set_level(Level::Warn);
        let captured = Arc::new(CapturedSink::default());
        let _restore = SinkGuard::install(captured.clone());
        emit(Level::Warn, "first");
        emit(Level::Warn, "second");
        let drained = captured.drain();
        assert_eq!(
            drained,
            vec!["[warn] first".to_string(), "[warn] second".to_string()]
        );
        // Drain consumes — second drain is empty.
        assert!(captured.drain().is_empty());
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn captured_sink_evicts_oldest_when_full_and_counts_drops() {
        // Bounded ring contract: at cap, the next emit evicts the
        // front entry and bumps the dropped counter. A regression
        // that drops the *new* entry instead would leave the
        // user staring at stale warnings while fresh ones vanish.
        let captured = CapturedSink::with_capacity(2);
        captured.emit(Level::Warn, "first");
        captured.emit(Level::Warn, "second");
        captured.emit(Level::Warn, "third");
        let detailed = captured.drain_detailed();
        let texts: Vec<String> = detailed.entries.iter().map(|e| e.text.clone()).collect();
        assert_eq!(
            texts,
            vec!["[warn] second".to_string(), "[warn] third".to_string()],
            "drop-oldest must evict 'first', keep latest two",
        );
        assert_eq!(detailed.dropped, 1, "exactly one entry evicted");
    }

    #[test]
    fn captured_sink_legacy_drain_also_resets_dropped_counter() {
        // Legacy `drain` delegates to `drain_detailed` and discards
        // the count, but it still resets `dropped`. Pin this so a
        // future refactor that inlines `drain` with its own pop
        // doesn't silently leak the eviction count into the next
        // detailed drain.
        let captured = CapturedSink::with_capacity(1);
        captured.emit(Level::Warn, "a");
        captured.emit(Level::Warn, "b"); // evicts 'a' → dropped = 1
        let _ = captured.drain();
        captured.emit(Level::Warn, "c");
        let detailed = captured.drain_detailed();
        assert_eq!(detailed.dropped, 0);
    }

    #[test]
    fn captured_sink_dropped_counter_resets_on_detailed_drain() {
        // Detailed drain hands the eviction count to the consumer
        // and zeroes it so the next drain only sees evictions that
        // happened *since* the last detailed pull. Otherwise the
        // counter would monotonically rise forever and the panel
        // would keep showing "5 dropped" long after they were
        // surfaced.
        let captured = CapturedSink::with_capacity(1);
        captured.emit(Level::Warn, "a");
        captured.emit(Level::Warn, "b"); // evicts 'a'
        let first = captured.drain_detailed();
        assert_eq!(first.dropped, 1);
        captured.emit(Level::Warn, "c");
        let second = captured.drain_detailed();
        assert_eq!(second.dropped, 0, "counter must reset between drains");
    }

    #[test]
    fn captured_sink_with_capacity_clamps_zero_to_one() {
        // A literal zero-cap would silently drop every emission —
        // the consumer would see an empty `entries` and a rising
        // `dropped` count with no payload. Clamp to one so the
        // sink at least preserves the most recent entry.
        let captured = CapturedSink::with_capacity(0);
        captured.emit(Level::Warn, "only");
        let detailed = captured.drain_detailed();
        assert_eq!(detailed.entries.len(), 1);
        assert_eq!(detailed.entries[0].text, "[warn] only");
    }

    #[test]
    fn captured_sink_records_emit_timestamp() {
        // Each entry stamps `Instant::now()` at emit so a renderer
        // can show relative age. Two emits separated by `sleep`
        // produce strictly increasing timestamps.
        let captured = CapturedSink::default();
        captured.emit(Level::Warn, "first");
        std::thread::sleep(std::time::Duration::from_millis(2));
        captured.emit(Level::Warn, "second");
        let detailed = captured.drain_detailed();
        assert_eq!(detailed.entries.len(), 2);
        assert!(
            detailed.entries[1].at > detailed.entries[0].at,
            "second emit must have a later timestamp than the first",
        );
    }

    #[test]
    fn captured_sink_last_drain_seq_marks_in_frame_emits_as_seen() {
        // The run_loop's watermark uses last_drain_seq to advance
        // past in-frame emits the render already showed the user.
        // Pin the contract: after a drain, last_drain_seq equals
        // emit_seq, and a background emit landing after the drain
        // does NOT advance last_drain_seq (so the watermark stays
        // below cur_emit_seq and the next idle tick wakes).
        let captured = CapturedSink::default();
        assert_eq!(captured.last_drain_seq(), 0);
        captured.emit(Level::Warn, "in-frame-a");
        captured.emit(Level::Warn, "in-frame-b");
        let _ = captured.drain_detailed();
        assert_eq!(captured.last_drain_seq(), 2, "drain marks both as seen");
        assert_eq!(captured.emit_seq(), 2);
        captured.emit(Level::Warn, "background");
        assert_eq!(
            captured.last_drain_seq(),
            2,
            "post-drain emit must NOT advance last_drain_seq",
        );
        assert_eq!(captured.emit_seq(), 3, "but emit_seq advances");
    }

    #[test]
    fn captured_sink_emit_seq_is_monotonic_across_drains() {
        // The wake signal must NOT reset on drain — sampling
        // post-drain would otherwise miss-mark emissions that
        // arrived during the draw (between the in-frame drain and
        // the loop's post-draw watermark update) as already-seen.
        // Pin the monotonic contract so a future refactor that
        // re-introduces a drain reset breaks here, not in the run-
        // loop watermark.
        let captured = CapturedSink::default();
        assert_eq!(captured.emit_seq(), 0);
        captured.emit(Level::Warn, "a");
        captured.emit(Level::Warn, "b");
        assert_eq!(captured.emit_seq(), 2);
        let _ = captured.drain_detailed();
        assert_eq!(captured.emit_seq(), 2, "drain must not reset emit_seq");
        captured.emit(Level::Warn, "c");
        assert_eq!(captured.emit_seq(), 3);
        let _ = captured.drain();
        assert_eq!(
            captured.emit_seq(),
            3,
            "legacy drain must not reset emit_seq"
        );
    }

    #[test]
    fn sink_guard_restores_prior_sink_on_drop_lifo_three_deep() {
        // The TUI relies on RAII restore for stderr emissions to
        // resume after the alt-screen exits. A three-level nest
        // catches a regression where Drop accidentally restores the
        // first-installed sink (or any non-immediate prior) — a
        // two-level nest would let that bug pass.
        let _g = lock();
        set_level(Level::Warn);
        let outer = Arc::new(CapturedSink::default());
        let _outer_g = SinkGuard::install(outer.clone());
        {
            let middle = Arc::new(CapturedSink::default());
            let _middle_g = SinkGuard::install(middle.clone());
            {
                let inner = Arc::new(CapturedSink::default());
                let _inner_g = SinkGuard::install(inner.clone());
                emit(Level::Warn, "inner");
                assert_eq!(inner.snapshot(), vec!["[warn] inner".to_string()]);
                assert!(middle.snapshot().is_empty());
                assert!(outer.snapshot().is_empty());
            }
            // _inner_g dropped → middle is active again.
            emit(Level::Warn, "middle");
            assert_eq!(middle.snapshot(), vec!["[warn] middle".to_string()]);
            assert!(outer.snapshot().is_empty());
        }
        // _middle_g dropped → outer is active again.
        emit(Level::Warn, "outer");
        assert_eq!(outer.snapshot(), vec!["[warn] outer".to_string()]);
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn install_sink_blocks_until_in_flight_emit_completes_and_emission_lands_on_prior_sink() {
        // Pin the lsm-67go race fix. Before the RwLock conversion,
        // `current_sink` cloned the Arc and dropped the slot lock
        // before calling `.emit()`, so `install_sink` could swap mid-emit
        // and the emission landed on the just-orphaned sink. Now `emit`
        // holds the read lock across the call; `install_sink` waits on
        // the write lock.
        //
        // Two halves pinned:
        //  - Install blocks while emit is in progress.
        //  - Emission lands on the prior sink; the new sink stays empty
        //    (a double-routing regression would fail this, not a call counter).
        run_install_race_test(EmitKind::Warn);
    }

    #[test]
    fn install_sink_blocks_until_in_flight_emit_error_completes() {
        // Mirror of the warn-path race test for `emit_error`, which
        // bypasses the level gate on a separate code path. A regression
        // that reverted only `emit_error` would slip through the warn test.
        //
        // `set_level(Level::Off)` confirms the level-gate bypass still
        // routes through the read-lock-held path.
        run_install_race_test(EmitKind::Error);
    }

    #[derive(Clone, Copy)]
    enum EmitKind {
        Warn,
        Error,
    }

    fn run_install_race_test(kind: EmitKind) {
        use std::sync::atomic::AtomicBool;
        use std::sync::Barrier;
        use std::time::Duration;

        struct BarrierSink {
            inside: Arc<Barrier>,
            release: Arc<Barrier>,
            inner: Arc<CapturedSink>,
        }
        impl LogSink for BarrierSink {
            fn emit(&self, lvl: Level, msg: &str) {
                self.inside.wait();
                self.release.wait();
                self.inner.emit(lvl, msg);
            }
            fn emit_error(&self, msg: &str) {
                self.inside.wait();
                self.release.wait();
                self.inner.emit_error(msg);
            }
        }

        let _g = lock();
        match kind {
            EmitKind::Warn => set_level(Level::Warn),
            EmitKind::Error => set_level(Level::Off),
        }
        let inside = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let prior_inner = Arc::new(CapturedSink::default());
        let prior_sink: Arc<dyn LogSink> = Arc::new(BarrierSink {
            inside: inside.clone(),
            release: release.clone(),
            inner: prior_inner.clone(),
        });
        // `SinkGuard` provides RAII restore so an assertion-induced
        // panic still puts the pre-test sink back. A manual
        // install_sink → assert → install_sink pair would skip the
        // restore on panic and leave a `BarrierSink` installed for
        // every subsequent test, hanging them at `inside.wait()`.
        let _restore = SinkGuard::install(prior_sink);

        let emit_handle = std::thread::spawn(move || match kind {
            EmitKind::Warn => emit(Level::Warn, "racy"),
            EmitKind::Error => emit_error("racy"),
        });
        inside.wait();

        let new_captured = Arc::new(CapturedSink::default());
        let new_sink: Arc<dyn LogSink> = new_captured.clone();
        let install_done = Arc::new(AtomicBool::new(false));
        let install_done_clone = install_done.clone();
        let install_handle = std::thread::spawn(move || {
            let prior = install_sink(new_sink);
            install_done_clone.store(true, Ordering::Release);
            prior
        });

        // Install must block while the emit holds the read lock.
        // A regression to clone-then-emit would let install land in
        // microseconds; 75ms is generous enough to distinguish.
        std::thread::sleep(Duration::from_millis(75));
        assert!(
            !install_done.load(Ordering::Acquire),
            "install_sink must block while {kind:?} emit is in progress",
            kind = match kind {
                EmitKind::Warn => "warn",
                EmitKind::Error => "error",
            },
        );

        release.wait();
        emit_handle.join().expect("emit thread");
        let from_install = install_handle.join().expect("install thread");
        assert!(install_done.load(Ordering::Acquire));

        let expected = match kind {
            EmitKind::Warn => "[warn] racy",
            EmitKind::Error => "[error] racy",
        };
        assert_eq!(
            prior_inner.snapshot(),
            vec![expected.to_string()],
            "emission must land on the prior sink",
        );
        assert!(
            new_captured.snapshot().is_empty(),
            "post-install sink must not see an emission issued before the swap, got {:?}",
            new_captured.snapshot(),
        );

        // `from_install` holds the racing thread's prior (the
        // `BarrierSink`). Drop explicitly so the slot's strong-count
        // settles before `_restore` runs install_sink again — the
        // new sink (`new_captured`) is currently in the slot; the
        // guard's drop replaces it with the pre-test sink.
        drop(from_install);
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn install_sink_returns_prior_for_manual_restore() {
        // The raw `install_sink` is `pub(crate)`; this pins the
        // round-trip contract `SinkGuard::install` itself depends
        // on. Force the level explicitly: the test runs serially,
        // but a future test that leaves `Off` would otherwise
        // silently suppress the emit and break the assertion.
        let _g = lock();
        set_level(Level::Warn);
        let captured = Arc::new(CapturedSink::default());
        let prior = install_sink(captured.clone());
        emit(Level::Warn, "captured");
        assert_eq!(captured.snapshot(), vec!["[warn] captured".to_string()]);
        let _ = install_sink(prior);
    }

    #[test]
    fn test_capture_warns_returns_function_result_and_captured_emissions() {
        let (result, captured) = _test_capture_warns(|| {
            emit(Level::Warn, "first");
            emit(Level::Warn, "second");
            42
        });
        assert_eq!(result, 42);
        assert_eq!(
            captured,
            vec!["[warn] first".to_string(), "[warn] second".to_string()]
        );
    }

    #[test]
    fn test_capture_warns_captures_emit_error_regardless_of_level() {
        let (_, captured) = _test_capture_warns(|| {
            emit_error("structural failure");
        });
        assert_eq!(captured, vec!["[error] structural failure".to_string()]);
    }

    #[test]
    fn emit_routes_to_thread_local_sink_even_when_global_level_is_off() {
        // `emit` consults the thread-local sink before the level gate,
        // so a peer test that left `LEVEL=Off` can't silently suppress
        // emissions inside a capture window. Setup is inline rather than
        // via `_test_capture_warns` because we already hold the serial
        // lock (the helper takes it; std::sync::Mutex is non-reentrant).
        let _g = lock();
        set_level(Level::Off);
        let sink = Arc::new(CapturedSink::default());
        let prior = THREAD_SINK.with(|cell| cell.replace(Some(sink.clone())));
        let _restore = ThreadSinkGuard { prior };
        emit(Level::Warn, "still captured");
        assert_eq!(sink.drain(), vec!["[warn] still captured".to_string()]);
        set_level(DEFAULT_LEVEL);
    }

    #[test]
    fn test_capture_warns_subsequent_call_starts_empty() {
        // No state leaks across invocations: each call installs a
        // fresh CapturedSink and the thread-local restores to its
        // prior value on return.
        let _ = _test_capture_warns(|| emit(Level::Warn, "first"));
        let (_, second) = _test_capture_warns(|| {});
        assert!(
            second.is_empty(),
            "second call must start with no captured entries, got {second:?}"
        );
    }

    #[test]
    fn concrete_sink_types_remain_thread_safe() {
        // `Arc<dyn LogSink>` requires Send+Sync via the trait
        // bound, so the trait-object case is enforced at compile
        // time without a runtime assertion. The concrete-type pins
        // here catch a future field addition (e.g. `Cell`,
        // `RefCell`) that would auto-derive a non-Sync `StderrSink`
        // or `CapturedSink` — at which point neither could be
        // wrapped in `Arc<dyn LogSink>` and the trait-object
        // bound's compile error would name the trait, not the
        // field. Naming the concrete types here makes the failure
        // point at the right line.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StderrSink>();
        assert_send_sync::<CapturedSink>();
    }
}
