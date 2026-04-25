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

| #   | Scenario                                    | `used_percentage` (stdin) | `/context` truth | `context_window_size` correct?          | Render anomaly?                                                              | Pass/Fail |
| --- | ------------------------------------------- | ------------------------- | ---------------- | --------------------------------------- | ---------------------------------------------------------------------------- | --------- |
| 1   | 200k-context session, low fill, pre-compact | 21                        | 21%              | yes — `200_000`                         | none                                                                         | **Pass**  |
| 2   | Same session after `/compact`               | TBD                       | TBD              | TBD                                     | TBD                                                                          | TBD       |
| 3   | Session resumed via `/resume`               | TBD                       | TBD              | TBD                                     | TBD                                                                          | TBD       |
| 4   | During a 429 rate-limit response            | TBD                       | TBD              | TBD                                     | TBD                                                                          | TBD       |
| 5   | 1M-context variant (Opus 4.7 1M)            | 34                        | 34%              | yes — `1_000_000`, not stuck at 200_000 | none once [lsm-ts7k](../../.beads/issues.jsonl) (effort parser) is installed | **Pass**  |

Row 5 notes:

- `model.id = "claude-opus-4-7[1m]"` — the `[1m]` suffix marks the 1M variant. ccstatusline PR #265 reported the hint dropped from `model.display_name`; `model.id` is unaffected.
- Capture timestamps: `2026-04-24T18:46:50Z` through `2026-04-24T19:52:42Z` in `~/.linesmith-captures/stdin.jsonl`; percentages stepped 34→35 as the session grew, consistent with the `/context` output.
- All captures fell within `[0, 100]`; no `>100` post-compact drift of the kind reported in Anthropic #37163 observed at this fill level.
- Before the effort-parser fix (lsm-ts7k), linesmith rendered `?` even though the percentage itself was correct — a separate issue the capture surfaced.

Row 1 notes:

- `model.id = "claude-sonnet-4-6"` (no `[1m]` suffix); `model.display_name = "Sonnet 4.6"`. `exceeds_200k_tokens = false` corroborates the 200k window.
- Confirmed against a fresh CC instance opened on the standard 200k Sonnet 4.6 with 19 captures clustered between `2026-04-25T02:55:09Z` and `2026-04-25T02:57:56Z`. Reported `42.2k/200k tokens (21%)` in `/context`; stdin reported pct=21 in lockstep. Delta: 0. Higher-fill 200k captures (post-`/compact` and beyond) would tighten the row further but the 21% point already validates the integer-percentage and 200k-denominator contracts.

### Sanity checks against the capture dataset

Findings from `~/.linesmith-captures/stdin.jsonl` after the lsm-mdd7 wrapper accumulated 710 records (all-integer percentages) across one extended Claude Code session on Opus 4.7 1M (fill range 15%→72%, no `/compact` or `/resume` triggered):

- **`used_percentage + remaining_percentage = 100` holds for 708/710 captures.** The two outliers are pre-API-call records where the entire `context_window` object is `null` (expected per the contract; `current_usage` is also null in those records).
- **`total_input_tokens + total_output_tokens` does NOT equal `used_percentage × context_window_size / 100`.** At pct=15 with size=1M the formula predicts 150,000 tokens; the actual record had `total_in=1913`, `total_out=47304` (sum 49,217). The 100,783-token gap is more than covered by `cache_read_input_tokens=144,775` plus `cache_creation_input_tokens=1313` (the cache totals exceed the gap by ~45k, so the residual isn't cleanly explained by cache alone). **Likely implication** (interpretation, not CC-documented): `used_percentage` is consistent with counting cache-resident tokens against the window while `total_*_tokens` count only billable (non-cache-hit) tokens. **Affects the P2 `context_usable` segment formula** in §Conclusions; revisit before implementing.
- **`current_usage` is per-turn, not cumulative.** Per-turn `input_tokens=1` against cumulative `total_input_tokens=1913` at the same capture confirms the naming. Resolves open question #5.
- **No `>100` clamp triggers across the dataset.** The fill range only reached 72%; the Anthropic #37163 post-`/compact` drift didn't reproduce because `/compact` was never invoked. Row 2 of the matrix still needs a live capture to test that scenario.

### Observed failures (none in lsm-mdd7 dataset, 710 captures)

In the 15-72% fill range on Opus 4.7 1M: no `?` renders, no parse errors, no `>100` values, no `current_usage`/`total_*` contradictions.

Two parser bugs DID surface during the capture session, both unrelated to `used_percentage` correctness:

- effort object form rejected by `parse_effort` (filed as lsm-ts7k, fixed)
- force-color path collapsing TrueColor themes to Palette16 (filed as lsm-05d1, fixed)

Per-failure template for future captures (rows 1-4 of the matrix):

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
- ~~Does `context_window_size` change to 1_000_000 immediately when switching to a 1M model, or is it stuck at 200_000 until the next stdin refresh?~~ **Resolved 2026-04-24 via row 5:** `context_window_size` reports `1_000_000` on every 1M-model capture; the ccstatusline PR #265 bug (payload stuck at 200k) does not reproduce in CC 2.1.119.
- Is there an observable `/resume` marker in the stdin payload, or does the percentage just "jump" on the next post-resume message?
- Are 429 responses even visible at the statusline layer, or are they intercepted upstream and never produce a stdin event?
- ~~Does `current_usage` reflect the _current turn_ only, or the cumulative session? (Naming suggests current turn; would clarify the relationship to `total_input_tokens`.)~~ **Resolved 2026-04-25 via dataset analysis:** `current_usage` is per-turn. Per-turn `input_tokens=1` against cumulative `total_input_tokens=1913` at the same capture confirms the naming. See §Sanity checks against the capture dataset.
