# Config TUI uses instant-apply (auto-save) for screen-level commits

- Status: accepted
- Date: 2026-05-09
- Deciders: Jace
- Surfacing bead: lsm-9o97
- Supersedes (in part): [ADR-0016](0016-tui-screen-state-machine.md) — save-UX section only

## Relationship to ADR-0016

[ADR-0016](0016-tui-screen-state-machine.md) defined the TUI's screen-state machine alongside its save-UX. This ADR changes the save-UX decisions; everything else from ADR-0016 stands.

**Changed by this ADR (save UX):**

- "Ctrl+S saves immediately" — replaced by per-screen instant-apply; `Ctrl+S` is a no-op force-flush for one release with a deprecation notice, then removed.
- "Dirty-check via stringify diff" — `Model::is_dirty()` and the dirty-tracking machinery are removed; the new model has no dirty state.
- Confirm-on-quit modal — dropped; quit is unconditional under instant apply.

**Retained from ADR-0016 (unchanged):**

- The `AppScreen` enum + per-variant state structs (one variant per screen).
- The Elm-style Model/Update/View loop with pure `(Model, Event) -> Model` updates.
- `ListScreen` and `PropertyScreen` reusable widget templates.
- Preview rendering as a persistent header through `crate::layout::render_to_runs`.
- Preview correctness contract (model→view path is deterministic; preview byte-matches what `linesmith` would render at the same cwd).
- Bottom-of-screen description string + one-line help row.

ccstatusline-parity remains a driver (not a binding contract) for the screen-state machine, as in ADR-0016. This ADR's chosen save-UX direction explicitly diverges from ccstatusline; the parity goal still applies to non-save concerns.

## Context and Problem Statement

[ADR-0016](0016-tui-screen-state-machine.md) defined the TUI's per-screen `update` / `view` loop and a 2-stage commit model: each screen's Enter writes to the in-memory `DocumentMut` (visible in `is_dirty()` + the live preview), and a global `Ctrl+S` flushes that document to disk via atomic rename. A `ConfirmQuit` modal warns when the user tries to leave with unsaved changes. The model is consistent across all five edit screens (theme picker, items editor, line picker, type picker, raw value editor) and matches the text-editor convention.

Manual TUI testing on the theme picker (lsm-herx.21) surfaced a UX complaint: the `Ctrl+S` keybind is invisible until the user already triggers the dirty-quit modal, and a structured config TUI feels closer to a settings dialog than a text editor. The user expected "I picked it, it's saved" semantics; HCI literature suggests this expectation is the default for settings panes (see `## Decision Drivers` below).

The research note `docs/research/tui-save-ux.md` surveyed prior art: ccstatusline (the parity target) uses explicit save plus a dirty-gated "Save & Exit" / "Exit without saving" menu; macOS Settings, VS Code Settings, Firefox preferences instant-apply; GNOME HIG, Apple HIG, Material Design all prefer instant apply for settings panes; htop and the current linesmith model share a SIGTERM/forgot-to-save footgun that auto-save eliminates.

Given that the TUI's screens edit one structured field each (a theme name, a segment list, a raw value for one key) rather than free-form prose, what's the right save model — auto-save, ccstatusline-style apply menu, or stay-the-course?

## Decision Drivers

- **User mental model.** Verbatim feedback that triggered this ADR was "I find it bad UX — either auto-save or have a apply menu option instead of ctrl+s." Both alternatives map to "make save not a hidden keybind." The mental-model question is whether the screens are settings or documents; the research concludes settings.
- **HCI guidance for settings panes.** GNOME HIG explicitly prefers instant apply unless changes take >1s or are destructive; Apple HIG and Material Design echo. NN/g's "removing Save when users expect one" caveat applies in the inverse direction here — the user expects auto-save and is surprised by explicit save.
- **ccstatusline parity tension.** The bead epic (lsm-herx) targets ccstatusline-parity, and ccstatusline does NOT auto-save (despite README wording — its actual code uses Ctrl+S + dirty-gated menu rows). Strict parity would mean the apply-menu option. But ccstatusline's screens are multi-field forms (a single screen edits color + bold + character + merge + hide for one widget); linesmith's are single-field per screen, so the "save batch of edits" semantic that justifies explicit save in ccstatusline doesn't pay off here.
- **Mechanical safety of auto-save.** linesmith's saves are atomic-rename on a single-file, single-user config. Each screen-level commit is already a complete, valid TOML mutation (no half-written `[line.0]` table). Auto-save is mechanically safe — no transactional risk, no partial-state-on-disk window.
- **Discoverability.** `Ctrl+S` is currently surfaced only inside the `ConfirmQuit` modal — the user has to trigger the failure path to learn the recovery keybind. Auto-save eliminates the keybind entirely; the apply-menu option would surface it as a row.
- **Failure-mode parity with status quo.** A user who kills the terminal mid-edit today loses uncommitted document state silently. Auto-save reduces that loss window to zero. Disk-full / read-only-mount errors must surface clearly under either model; the warnings panel is the existing channel.

## Considered Options

- **Option 1 — Per-screen instant-apply (auto-save).** Each screen's Enter-commits path also calls `Model::save()`. Drop the `ConfirmQuit` modal. Deprecate `Ctrl+S` as a no-op force-flush for one release. Add a footer keybind hint bar. Surface a transient "Saved" toast in the preview pane on success; persistent banner on save failure.
- **Option 2 — Apply menu (ccstatusline-exact).** Keep the 2-stage commit. Add dirty-gated "Save changes" / "Discard changes" rows to MainMenu (visible only when `is_dirty()`). Add a footer keybind hint bar that includes `Ctrl+S`. Keep the `ConfirmQuit` modal as the safety net.
- **Option 3 — Status quo plus footer hints.** Keep everything as-is, add only the footer keybind hint bar. Solves discoverability without touching the save model.

## Decision Outcome

Chosen option: **Option 1 — per-screen instant-apply**, because (a) HCI guidance overwhelmingly prefers instant apply for settings panes and the TUI's screens are structurally settings panes, not document editors, (b) the user's verbatim feedback was a mental-model complaint ("I shouldn't need a save key for this kind of UI"), not a discoverability complaint, so Option 3 doesn't address the actual issue, (c) ccstatusline parity is a guiding-not-binding goal — the research found ccstatusline's apply-menu pattern fits its multi-field forms but not linesmith's single-field screens, (d) auto-save is mechanically safe given atomic-rename + single-file constraints, and (e) the dropped `ConfirmQuit` modal + dropped `Ctrl+S` keybind reduce the keybind surface area, simplifying the screen-state machine that ADR-0016 established.

### Consequences

- Good, because the user's mental-model complaint is addressed directly: each pick lands on disk, and the toast confirms it.
- Good, because the screen-state machine simplifies: no dirty-tracking, no ConfirmQuit, no save keybind to plumb through 5 screens.
- Good, because the SIGTERM / forgot-to-save footgun (the same one [htop](https://github.com/htop-dev/htop/issues/949) hits) is eliminated.
- Good, because adopting the toast + footer hint bar buys discoverability we'd want under any save model.
- Bad, because we explicitly diverge from ccstatusline's pattern. Users porting from ccstatusline who expect "edit, save, exit" find linesmith's TUI behavior different. The research found this trade-off acceptable given ccstatusline's multi-field form structure doesn't apply to linesmith's single-field screens, but the divergence is real.
- Bad, because external-edit collisions become more frequent: a user editing the config in vim while the TUI is open will have their changes clobbered on the next picker action. (Today's `Ctrl+S` already clobbers them, but the user controls the timing; auto-save makes the clobber happen on every screen-level commit, so the collision frequency is higher even if the per-event blast radius is the same.) Mitigation tracked under `lsm-lhby` (file-mtime check or fsevents/inotify watcher).
- Bad, because users who want to "try a few options without committing" lose that semantic. The research found this semantic is theoretical — Up/Down on the cursor-driven preview already shows the effect without committing, and "exit without save" was the only escape. Picking "the previous selection" is the new revert path.
- Neutral, because the multi-step edit churn (3 disk writes for a 3-segment-add) is acceptable on tiny configs but worth measuring on slow filesystems / network homedirs before defending the choice in benchmarks.
- Neutral, because the footer hint bar is orthogonal — we'd add it under any save model — but ships with this ADR's implementation slice.
- Neutral, because the existing `Model::is_dirty` / `ConfirmQuit` / `apply_save` unit tests in `app.rs` get replaced by toast-rendering and save-on-commit tests; net test count is roughly unchanged. The 5 edit screens themselves don't reference these symbols today, so the rework surface is concentrated in `app.rs` plus whatever new toast/banner widget the implementation slice adds.

### Confirmation

Revisit if:

- ≥3 GitHub issues file external-edit-collision complaints against the auto-save behavior, or a sustained thread on the issue tracker covers the same ground. Fallback: add a file-modification-time check before each save; a follow-up research note covers this.
- Multi-step edit churn shows user-visible latency on slow filesystems (NFS homedirs, cloud-synced config) — measured by ad-hoc timing today, or by a save-write benchmark once one lands in `crates/linesmith/benches/`. Fallback: debounced / coalesced writes with a `flush-on-screen-exit` boundary.
- A user survey or sustained porting thread shows ccstatusline parity becoming a hard constraint. Fallback: Option 2 (apply menu) is a strict superset of the v0.1 instant-apply behavior, so dirty-state tracking and apply-menu rows can be re-added without breaking auto-save users.
- Users question whether saves landed (e.g. "did my pick actually save?" surfaces in an issue or community thread). Fallback: replace the transient toast with a persistent saved-state indicator in the footer hint bar.

## Implementation shape

### UX shape

```text
┌ preview ──────────────────────────────────────────────────┐
│ <rendered statusline at current theme>                    │
│ ⚠ <env warning>                                           │
│ ✓ Saved theme = "dracula"                  ← transient (~1.5s) │
└───────────────────────────────────────────────────────────┘
                pick theme
  > default        • press Enter to apply
    minimal
    dracula
    nord
    ...
[Enter] confirm   [Esc] back   [q] quit                  ← footer hint bar
```

On a save failure, the transient toast is replaced with a persistent banner:

```text
┌ preview ──────────────────────────────────────────────────┐
│ <rendered statusline at current theme>                    │
│ ❌ Couldn't save to /home/user/.config/linesmith/config.toml: │
│    Permission denied — fix the path or rerun with --config │
└───────────────────────────────────────────────────────────┘
```

The banner stays until the next successful save (or the user quits + restarts). Quit is unconditional under auto-save; there's no dirty state at exit time.

### Behavioral changes

| Today (2-stage)                             | Under instant apply                                                                                     |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| Enter commits to `DocumentMut` only         | Enter commits + calls `Model::save()`                                                                   |
| `Ctrl+S` flushes to disk                    | `Ctrl+S` is a no-op force-flush for one release with a deprecation notice; removed in the release after |
| `ConfirmQuit` modal on dirty quit           | Dropped — quit is unconditional                                                                         |
| Failure: warnings panel + dirty stays       | Failure: persistent banner + in-memory edit kept (don't roll back UI state)                             |
| Save discoverability: invisible until modal | Save discoverability: not needed; the toast confirms each commit                                        |
| Dirty-tracking via `Model::is_dirty()`      | `is_dirty()` removed (no consumers)                                                                     |

### Save-failure semantics

When `Model::save()` returns an error under instant apply:

1. The in-memory edit is **not** rolled back. The user sees their selection take effect in the preview; the next render reflects the new state. Rolling back UI state mid-flow is more confusing than a flash error.
2. A persistent banner replaces the transient toast: `"Couldn't save to <path>: <error>"`. The banner stays in the warnings panel until a subsequent commit succeeds.
3. `Model::save()` failure types map: `SaveOutcome::NoTarget` → banner + `lsm_warn!` ("save not available — no config path"); `SaveOutcome::Error { path, error }` → banner + `lsm_error!` (OS-level error path matches today's `apply_save` behavior); `SaveOutcome::Saved` → transient toast (replaces today's silent `lsm_debug!`); `SaveOutcome::Clean` → silent (the commit was a no-op like the implicit-default-pick case).
4. Since the in-memory document is the source of truth for the next render and the next commit will retry the write, transient failures (NFS hiccup, brief disk-full) self-heal once the user changes another field.

### Multi-step edit semantics

Items editor adding 3 segments to a line produces 3 atomic-rename writes (one per `Enter`). Each is a small (~hundreds of bytes) atomic write to a config the user controls. The research note evaluated this against the macOS Settings model (every preference toggle hits a plist) and concluded the IO is acceptable. If multi-line `[line.N]` configs prove too churny in practice, a debounced / coalesced write is an additive change behind the same UI.

### Ctrl+S deprecation path

- **v0.1 (this release):** `Ctrl+S` is a no-op force-flush — calls `Model::save()` and emits a one-line `lsm_warn!`: "Ctrl+S is no longer needed; changes save automatically." This honors muscle memory for users porting from ccstatusline / vim-style editors.
- **Next release:** the keybind is removed entirely. Press-and-no-effect is fine; the warning panel will guide users who hit it.

## Pros and Cons of the Options

### Option 1 — Per-screen instant-apply

- Good: matches user expectation for settings panes (macOS, VS Code, Firefox precedent).
- Good: aligns with GNOME HIG, Apple HIG, Material Design guidance.
- Good: simplifies the state machine (no dirty-tracking, no ConfirmQuit, no save keybind).
- Good: eliminates the SIGTERM footgun.
- Bad: diverges from ccstatusline's actual save pattern.
- Bad: external-edit collisions become more frequent (every commit can clobber concurrent vim edits, vs. only on Ctrl+S today).
- Bad: multi-step edits produce N disk writes instead of 1.

### Option 2 — Apply menu (ccstatusline-exact)

- Good: strict superset of today's behavior — no behavioral change beyond discoverability.
- Good: ccstatusline parity stays exact.
- Good: keeps try-and-discard semantics for users who want to experiment.
- Bad: doesn't fix the user's mental-model complaint — still a 2-stage flow, just with the save action made discoverable.
- Bad: keeps the `ConfirmQuit` modal and the dirty-tracking machinery.
- Bad: adds menu-state complexity (gating rows on `is_dirty()`, decision on visible-vs-greyed, where exactly to position).

### Option 3 — Status quo plus footer hints

- Good: smallest possible change.
- Good: zero risk to existing tests / behavior.
- Bad: doesn't address the user's actual complaint, only the symptom (discoverability).
- Bad: leaves the SIGTERM footgun in place.

## More Information

- Research: [`docs/research/tui-save-ux.md`](../research/tui-save-ux.md) — full survey of ccstatusline, k9s, lazygit, gitui, gh-dash, htop, btop, macOS / VS Code / Firefox / GitHub settings panes, GNOME HIG, Apple HIG, Material Design, NN/g auto-save guidance.
- Companion: [ADR-0015](0015-ratatui-for-tui-runtime.md) — TUI runtime choice (ratatui + crossterm).
- Partially supersedes: [ADR-0016](0016-tui-screen-state-machine.md) — save-UX section only; the screen-state machine, widgets, preview, and Update path stand. See `## Relationship to ADR-0016` above.
- Companion: [ADR-0023](0023-tui-items-editor-data-model.md) — items editor operates on `DocumentMut` directly; the auto-save change is additive to that model (the editor still mutates `DocumentMut`; the new wrinkle is that each commit also flushes).
- Surfacing bead: lsm-9o97 (TUI save UX redesign).
- Out of scope: external-edit detection (file-mtime check, inotify, fsevents) — separate follow-up research; the auto-save direction makes this slightly more pressing but doesn't block v0.1.
- Out of scope: plugin config hot-reload semantics — if a future plugin reloads on file change, auto-save means it reloads on every pick. Spec it explicitly when plugins land.
- Out of scope: reset-to-defaults action (Apple HIG / Material Design suggest yes; ccstatusline has no equivalent). Worth filing as a discoverability bead but doesn't gate this ADR.
