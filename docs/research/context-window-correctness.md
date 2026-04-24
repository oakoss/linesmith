# Context window percentage correctness across Claude Code edge cases

- Date: 2026-04-23
- Author: Claude Code research session (lsm-4wb)
- Scope: Determine whether `context_window.used_percentage` from Claude Code's statusline stdin is trustworthy across 1M-context sessions, post-`/compact`, post-`/resume`, and during 429 responses; document the ccstatusline `ContextPercentage` vs `ContextPercentageUsable` distinction and whether linesmith needs an analogous split.

## Question

Can linesmith pass Claude Code's `context_window.used_percentage` through to the `context` segment unchanged, or do we need a derived "usable percentage" that compensates for inaccuracies reported in the upstream issue clusters? For each of the five target scenarios: does the stdin number match the visible reality the user sees in the Claude Code REPL?

## Sources

- `docs/research/claude-code-statusline-api.md` — the stdin JSON contract, including the pre-existing warning that `used_percentage` is "unreliable under 1M context, post-`/compact`, post-`/resume`, and during 429s."
- `docs/research/user-demand.md` §3 — aggregates the ~12 ccstatusline issues and ~5 Anthropic-side issues reporting wrong percentages; cited en masse as motivating signal, not individually. Load-bearing for this investigation: Anthropic #37163 (post-compact > 100%), ccstatusline #105 (maintainer's `Ctx(u)` definition), ccstatusline PR #265 (Sonnet 4.6 1M display-name bug), ccstatusline PR #319 (proposed autocompact formula).
- ccstatusline source on `main` — `ContextPercentage` and `ContextPercentageUsable` widgets (see Findings §ccstatusline-split).
- linesmith `crates/linesmith/src/input.rs` — `ContextWindow` struct, `parse_context_window` validator. `crates/linesmith/src/driver.rs:719-730` for the parse-error fallback behavior.

## Findings

### Linesmith's current behavior

`parse_context_window` (input.rs:395-424) reads `used_percentage` into the `Percent` newtype via `Percent::from_f64`, which rejects any value outside `0.0..=100.0`; the parser surfaces the rejection as `ParseError::InvalidValue`. We do not compensate, clamp, or re-derive the percentage from tokens — the segment sees whatever Claude Code sent, subject to the in-range validator.

Our `ContextWindow` struct (input.rs:56-63) captures only `used`, `size`, `total_input_tokens`, and `total_output_tokens`. The stdin JSON also carries `current_usage` (input/output/cache_creation/cache_read), but we don't parse it today; it's neither on the struct nor mirrored to plugins via `ctx_mirror.rs:262-279`. A usable-percentage calculation from `total_input_tokens / (size * 0.8)` is possible with the current struct; a ccstatusline-parity calculation using the `current_usage` sub-fields would need `ContextWindow` extended first.

Implication: if Claude Code ever emits `used_percentage > 100` (observed on Anthropic #37163 post-compact), `parse_context_window` returns `ParseError::InvalidValue` and the driver renders `?` on stdout with the diagnostic on stderr (driver.rs:719-730). The statusline degrades to a marker, not blank — still worse than ccstatusline's clamped passthrough. Flag for the test matrix below.

### ccstatusline split — `ContextPercentage` vs `ContextPercentageUsable`

**Confirmed from ccstatusline `main` branch source:**

- `ContextPercentage` (`src/widgets/ContextPercentage.ts:38-58`) is near-raw: when stdin supplies `context_window.used_percentage`, it is displayed via `.toFixed(1)` after a `clampPercentage(0, 100)` pass in `src/utils/context-window.ts:78-82`. So a stdin value of 101.3 would render as 100.0, not as the original number. The transcript-JSONL fallback (pre-Claude-Code-v2.0.65) divides `contextLength / maxTokens * 100`.
- `ContextPercentageUsable` (`src/widgets/ContextPercentageUsable.ts:38-59`) **never uses `used_percentage`** — it always re-derives from token counts against a different denominator: `usableTokens = floor(maxTokens * 0.8)`, where 0.8 is the threshold at which Claude Code auto-compacts.

**The split is a UX abstraction, not a correctness fix.** Maintainer `sirmalloc` confirmed in ccstatusline #105: `Ctx(u)` = "usable context — the amount before auto-compaction occurs." At `Ctx(u): 100%` auto-compact fires; the full window is at 125% on the same scale. Draft PR #319 proposes replacing the flat 0.8 with Claude Code's real autocompact formula (`effectiveWindow - bufferTokens`, ≈167k of 200k, ≈967k of 1M) plus a `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` env var.

**No ccstatusline code path branches on post-`/compact`, post-`/resume`, or 429.** The only documented Claude-Code-adjacent correctness issue they surfaced: ccstatusline PR #265 (closed, unmerged) reports that `claude-sonnet-4-6`'s stdin payload omits the `(1M context)` hint from `model.display_name`, so ccstatusline's `parseContextWindowSize` regex misses it and `getContextConfig` falls through to the 200k default. The stdin `context_window_size` field carries the correct value; ccstatusline just doesn't read it on that path. Linesmith reads `context_window_size` directly, so this specific bug does not cross over — but the underlying Claude Code quirk (display_name not advertising the 1M cap) is worth checking during live tests.

**Implications for linesmith:**

- A `context_usable` segment is cheap to add. The simplest formula using fields we already capture is `total_input_tokens / (context_window_size * 0.8)`. A ccstatusline-parity formula using stdin `current_usage.input_tokens + cache_creation_input_tokens + cache_read_input_tokens` would require extending `ContextWindow` to parse the `current_usage` object (small change; not a blocker).
- The 5 target edge cases may be less buggy than `user-demand.md` implied. The upstream issue cluster could reflect user confusion about what the percentages mean (80% cliff vs 100% window) as much as genuine stdin-data bugs.
- The one confirmed upstream bug (`context_window_size` wrong for 1M models) is worth detecting: if `model.display_name` implies 1M but `context_window_size == 200000`, that's ccstatusline #265 surfacing in linesmith too. Could warn or override.

### Test matrix — live Claude Code observations

To be filled in by running linesmith against a live Claude Code session in each scenario. The test procedure for each row:

1. Reach the described state in a Claude Code session.
2. Capture the stdin JSON linesmith receives (`LINESMITH_LOG=debug` or a shell wrapper that `tee`s stdin).
3. Compare `context_window.used_percentage` to what Claude Code's own `/context` command reports.
4. Record the delta and any rendering anomaly in the linesmith output.

| #   | Scenario                                        | `used_percentage` (stdin) | `/context` truth | `context_window_size` correct? | Render anomaly? | Pass/Fail |
| --- | ----------------------------------------------- | ------------------------- | ---------------- | ------------------------------ | --------------- | --------- |
| 1   | 200k-context session, ~40-60% fill, pre-compact | TBD                       | TBD              | TBD                            | TBD             | TBD       |
| 2   | Same session after `/compact`                   | TBD                       | TBD              | TBD                            | TBD             | TBD       |
| 3   | Session resumed via `/resume`                   | TBD                       | TBD              | TBD                            | TBD             | TBD       |
| 4   | During a 429 rate-limit response                | TBD                       | TBD              | TBD                            | TBD             | TBD       |
| 5   | 1M-context variant (Opus 4.7 1M)                | TBD                       | TBD              | TBD                            | TBD             | TBD       |

Additional sanity checks to run while capturing each stdin:

- Does `used_percentage + remaining_percentage` equal 100 (the one Claude Code doc claims)?
- Is `total_input_tokens + total_output_tokens` consistent with `used_percentage * context_window_size / 100`?
- Does `current_usage` ever contradict the rolled-up `total_*` values?

### Observed failures (to populate after live tests)

Each confirmed failure becomes a follow-up bead referenced here. Template:

- **Scenario N — `summary of glitch`**: Claude Code sent `X`, real value is `Y`. Filed as `lsm-XXXX`.

## Conclusions

**Pending live tests.** The ccstatusline-source research (see §ccstatusline-split) establishes that the main competing tool trusts `used_percentage` verbatim and has no data-correctness compensation for the five scenarios. That reframes the investigation: the loud upstream issue cluster may reflect user confusion about the 80% auto-compact cliff (ccstatusline's `Ctx(u)` widget) more than genuine stdin-data bugs.

Provisional positions to confirm or reject after live tests:

- **P1 (v0.1, confirmable without live tests):** `Percent::from_f64` rejects values outside `0.0..=100.0` as `ParseError::InvalidValue`, which collapses the whole stdin parse and degrades the statusline to `?`. If Claude Code ever emits 101.2 (reported upstream in Anthropic #37163), the user sees `?` instead of a percentage — a marker, not a blank statusline, but still worse than ccstatusline's clamped passthrough. Widen the validator to clamp-with-warn in the segment, matching ccstatusline's `clampPercentage` behavior. Defensive parse is cheap.
- **P2 (feature, moderate confidence):** ship a `context_usable` segment (or an opt-in `denominator = "usable"` on the existing segment). The simplest formula is `total_input_tokens / (context_window_size * 0.8)` using fields already on `ContextWindow`. A ccstatusline-parity formula (`input + cache_creation + cache_read`) would need `ContextWindow` extended to parse `current_usage` from stdin.
- **P3 (diagnostic, low confidence until live):** detect the Sonnet-4.6-style 1M-cap mismatch — if `model.display_name` or `model.id` implies 1M but `context_window_size == 200000`, warn or override. Only useful if we observe the upstream bug during live tests.
- **Probably rejected after live tests:** the "`/compact`-aware render mode" idea — no ccstatusline evidence that it's needed, and Claude Code's stdin payload doesn't seem to expose a mid-compact state anyway.

## Implications / actions

- `Percent::from_f64` widening — filed as **lsm-mxcd**. Severity is moderate, but the failure mode is observed upstream (Anthropic #37163) and the defensive parse is cheap. Do not wait on live tests.
- Live-capture test matrix — filed as **lsm-mdd7**. Populates the 5 pass/fail rows in §Test matrix and triggers per-failure follow-ups if any scenario diverges.
- Open an ADR proposing a `context_usable` segment. The minimum-viable version uses `total_input_tokens / (context_window_size * 0.8)` from the current struct; the ccstatusline-parity version needs `ContextWindow` extended to parse `current_usage`. Pick one in the ADR, informed by whatever the live tests in lsm-mdd7 reveal about `current_usage` behavior.

## Open questions

- What does `used_percentage` actually output during an in-progress `/compact`? Is there a transition state (both 0 and 100 have been reported)?
- Does `context_window_size` change to 1_000_000 immediately when switching to a 1M model, or is it stuck at 200_000 until the next stdin refresh?
- Is there an observable `/resume` marker in the stdin payload, or does the percentage just "jump" on the next post-resume message?
- Are 429 responses even visible at the statusline layer, or are they intercepted upstream and never produce a stdin event?
- Does `current_usage` reflect the _current turn_ only, or the cumulative session? (Naming suggests current turn; would clarify the relationship to `total_input_tokens`.)
