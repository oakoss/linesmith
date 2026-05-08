# ListScreen MoveSwap is advisory; caller owns cursor-on-swap

- Status: accepted
- Date: 2026-05-07
- Deciders: Jace
- Surfacing bead: lsm-9x14

## Context and Problem Statement

`ListScreen::handle_key` returns `ListOutcome::MoveSwap { from, to }` when the user reorders rows in move-mode. The widget currently mutates `state.cursor = to` _before_ returning, expecting the caller to swap `rows[from]` and `rows[to]` in their own data. If the caller forgets the `MoveSwap` arm in their match, the highlighted row visually advances but the underlying data stays unchanged — the user sees their cursor "carry" the row while the actual ordering doesn't move. Silent UI/data desync.

A second, related concern: the move-mode redispatch path. The `handle_key` clamp added during lsm-herx.4 review clears `state.move_mode = false` whenever `move_mode_supported = false`. If a caller flips support off mid-keypress (e.g., a confirmation modal opens), a user holding ↓ in move-mode sees their next ↓ silently reinterpreted from "swap with the row below" to "navigate down". Test `move_mode_supported_false_clears_stale_state_move_mode` pins the current reinterpretation as Consumed-with-navigate; whether that's correct depends on the cursor-mutation answer.

How should `MoveSwap` express the contract that "swap not yet committed; caller must complete it"? And how should the redispatch path behave when the user's intent (swap) becomes uninterpretable in the new mode (no move support)?

## Decision Drivers

- **Silent data loss is the worst failure mode.** A reorder bug where the cursor moves but the data doesn't is invisible until the user saves and notices their config out of order. Visible misbehavior beats silent misbehavior.
- **The caller already touches the data.** Any caller that handles `MoveSwap` is already calling `rows.swap(from, to)`. Adding a `state.set_cursor(to)` line is symmetric — both fields belong to the caller's row vector / cursor pair.
- **lsm-herx.7 hasn't shipped.** The items editor is the first production caller of `MoveSwap`; settling the contract before it lands costs nothing. Settling after costs a refactor across every consumer.
- **Plugin-segment caller cost.** Future plugin-authored screens will use `ListScreen` too. The contract should be hard to misuse from outside the workspace.

## Considered Options

- **Option 1 — Caller owns cursor on swap.** `MoveSwap { from, to }` carries the request; widget does NOT mutate `state.cursor`. Caller must `rows.swap(from, to)` AND `state.set_cursor(to)` to acknowledge.
- **Option 2 — Rename to `MoveSwapRequested` + harden docs.** Cosmetic; preserves the silent-desync footgun.
- **Option 3 — Debug-build acknowledgement tracking.** Widget records an outstanding swap; next `handle_key` `debug_assert!`s the swap was performed.
- **Option 4 — Leave as-is.** Document harder.

## Decision Outcome

Chosen option: **Option 1 — caller owns cursor on swap**, because (a) a missed `MoveSwap` arm becomes visible (the cursor doesn't move, mirroring the unchanged data), (b) the caller's two-line acknowledgement is symmetric with the data write it already does, and (c) widget state stays simple — no outstanding-swap tracking, no debug-only branches.

The redispatch path under Concern B becomes well-behaved as a side effect: when `move_mode_supported` flips off mid-key, the widget clears `state.move_mode` AND returns `ListOutcome::Unhandled` for that one keypress instead of falling through to `handle_normal_mode`. The caller sees an unhandled key and decides what to do; the user's next keypress lands in normal mode cleanly. Silently reinterpreting the trigger key was the surprising-behavior path.

### Contract (post-decision)

```rust
pub enum ListOutcome {
    Consumed,
    Activate,
    Action(char),
    /// Move-mode swap requested. Caller must:
    ///   1. swap rows[from] and rows[to] in their data
    ///   2. call state.set_cursor(to) to track the moved row
    /// Failing to do BOTH leaves the user's data and cursor out
    /// of sync. The widget intentionally does NOT mutate cursor
    /// here — a missed acknowledgement leaves the cursor frozen,
    /// which is visually obvious instead of silently desynced.
    MoveSwap { from: usize, to: usize },
    Unhandled,
}
```

Move-mode redispatch when `move_mode_supported` flips off:

```rust
// At the top of handle_key, after cursor clamp:
if !move_mode_supported {
    let was_in_move_mode = state.move_mode;
    state.move_mode = false;
    if was_in_move_mode {
        // Drop the trigger key — falling through to
        // handle_normal_mode would silently reinterpret a
        // swap-intent keypress as navigation.
        return ListOutcome::Unhandled;
    }
}
```

### Consequences

- Good, because a missed `MoveSwap` acknowledgement is now obvious (cursor frozen) instead of silent (cursor advances, data lags).
- Good, because the redispatch path no longer reinterprets keys across mode flips; the user's swap-intent keypress is dropped cleanly.
- Good, because `ListScreenState`'s mutation surface shrinks — cursor is set by the caller on swap, by the widget on plain navigation.
- Bad, because every `MoveSwap` call site grows by one line (`state.set_cursor(to)`). Cost is small and load-bearing.
- Bad, because the existing test `move_mode_supported_false_clears_stale_state_move_mode` pins Consumed-with-navigate; the test changes to expect `Unhandled` and the data is untouched.
- Neutral, because the variant name stays `MoveSwap` — the contract change is in the docs and the tests, not the type's name. Renaming to `MoveSwapRequested` was considered (Option 2) and rejected: the noun is fine; the contract is what was unclear.

### Confirmation

Revisit if:

- The items editor (lsm-herx.7) finds the two-line acknowledgement pattern awkward to use across a plugin / segment-list mix and a dedicated helper (e.g., `state.acknowledge_move_swap(from, to, &mut rows)`) emerges as cleaner. Type-system entanglement (the helper would need to know the row type) is the reason we don't pre-commit to one.
- A future screen genuinely wants the redispatch reinterpretation — i.e., "treat the dropped move-mode trigger as the corresponding normal-mode key". No current screen does. If one shows up, return `Action(c)` or `Consumed-with-cursor-move` from a dedicated arm rather than plumbing reinterpretation back into `handle_key`.

## Pros and Cons of the Options

### Option 1 — Caller owns cursor on swap

- Good: missed acknowledgement is visually obvious.
- Good: redispatch reinterpretation goes away naturally.
- Bad: every caller writes one extra line per `MoveSwap` arm.

### Option 2 — Rename + harder docs

- Good: zero behavior change; pure documentation.
- Bad: silent-desync footgun stays; "the docs say so" is the weakest enforcement.

### Option 3 — Debug-build acknowledgement tracking

- Good: catches silent desync in dev builds.
- Bad: widget gains an outstanding-swap state field; release builds carry the slot for nothing.
- Bad: a `debug_assert!` fires _after_ the bug — the assertion catches the symptom, not the cause.

### Option 4 — Leave as-is

- Good: zero churn.
- Bad: ships the footgun into the items editor and any future plugin-screen consumer.

## More Information

- Bead: lsm-9x14 (this design decision)
- Bead: lsm-herx.7 (Items Editor — first production caller; consumes the new contract)
- Companion: [ADR-0016](0016-tui-screen-state-machine.md) — TUI screen-state machine and `ListScreen` widget contract
- Implementation: `crates/linesmith/src/tui/list_screen.rs::handle_move_mode` (current MoveSwap site)
