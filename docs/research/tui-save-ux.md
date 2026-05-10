# TUI save UX: auto-save, explicit save, or apply menu

- Date: 2026-05-09
- Author: Claude Code research agent (for Jace Babin)
- Scope: Survey save-UX patterns across TUIs, GUI settings panes, and HCI guidelines, then recommend a direction for `linesmith config`.

## Question

linesmith's config TUI uses a 2-stage commit (Enter → in-memory `DocumentMut`, Ctrl+S → atomic-rename disk write). User feedback called this "bad UX" and offered two directions: (A) auto-save on every screen-level commit, or (B) make save discoverable as an Apply menu option. Which is right? What does ccstatusline (parity target) actually do, and what does wider prior art suggest?

## Sources

### Parity target

- ccstatusline source — `src/tui/App.tsx` (handleMainMenuSelect, useInput) and `src/tui/components/MainMenu.tsx` (menu items) — <https://github.com/sirmalloc/ccstatusline> main branch, fetched 2026-05-09 (MIT). File paths were verified at fetch time; if the repo refactors, re-resolve via blame on `handleMainMenuSelect` / `hasChanges`.
- ccstatusline README ("Your settings are automatically saved to `~/.config/ccstatusline/settings.json`") — <https://github.com/sirmalloc/ccstatusline/blob/main/README.md>

### TUI prior art

- k9s `:config` → `$EDITOR`, reactive watch — <https://k9scli.io/topics/config/>; reactive UI option <https://github.com/derailed/k9s>
- lazygit press `e` from status pane to edit config in `$EDITOR`, then auto-reloads via `ReloadChangedUserConfigFiles` — <https://deepwiki.com/jesseduffield/lazygit/5.1-configuration-system>; <https://github.com/jesseduffield/lazygit/issues/1158>
- gitui — config via RON files in `~/.config/gitui/` (theme.ron, key_bindings.ron); no in-app config editor — <https://github.com/gitui-org/gitui>
- gh-dash — YAML config; reload via `--config` or restart, no in-app editor — <https://www.gh-dash.dev/configuration/>
- htop — F2 setup menu, **F10 to save and exit; settings only persist on clean exit, lost on SIGTERM** — <https://github.com/htop-dev/htop/issues/949>; <https://github.com/htop-dev/htop/issues/1046>
- btop — Esc/M opens Options menu; settings stored at `~/.config/btop/btop.conf`, applied immediately and persisted at exit; no explicit Save action — <https://deepwiki.com/aristocratos/btop/2.3-configuration-system>; <https://github.com/aristocratos/btop/issues/574>

### GUI / web settings precedent

- macOS Ventura System Settings — toggles, immediate apply, no Save button — <https://9to5mac.com/2022/11/01/mac-system-settings-macos-ventura/>
- VS Code Settings editor — graphical settings auto-apply on change (no Apply button) — <https://code.visualstudio.com/docs/configure/settings>
- Firefox preferences — "When you close the Settings page... changes are automatically saved" — <https://support.mozilla.org/en-US/kb/how-to-fix-preferences-wont-save>
- GitHub repo settings — explicit "Save changes" button at the bottom of every settings panel — <https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/managing-repository-settings>

### HCI / design guidance

- GNOME HIG: "instant apply" preferred; explicit Apply only if change >1s or destructive; explicit dialogs use Apply/Cancel/OK triplet — <https://wiki.gnome.org/Design/HIG/UtilityWindows>; <https://blogs.gnome.org/desrt/2007/08/24/non-instant-apply-preferences-dialogs/>
- Apple HIG (Settings): "minimize the number of settings... let people modify task-specific options without going to a settings area" — <https://developer.apple.com/design/human-interface-guidelines/settings>
- Material Design v1 settings pattern: toggles imply immediate apply; "checkbox + Save" if a setting can't take effect immediately — <https://m1.material.io/patterns/settings.html>
- Nielsen Norman Group, "Don't Prioritize Efficiency Over Expectations": removing a Save button when users expect one "takes users out of autopilot mode... reduces users' control over the interface"; auto-save acceptable but visual feedback is mandatory — <https://www.nngroup.com/articles/efficiency-vs-expectations/>
- ui-patterns.com autosave pattern catalogue — <https://ui-patterns.com/patterns/autosave>
- GitLab Pajamas design system, Saving and feedback — <https://design.gitlab.com/product-foundations/saving-and-feedback>

### linesmith internals (current state, as of 2026-05-09)

Symbol references — grep / blame within `crates/linesmith/src/tui/app.rs`:

- `apply_save` — Ctrl+S handler.
- `Model::save` — atomic-rename flush; emits `SaveOutcome::{Saved, Clean, NoTarget, Error}`.
- `apply_quit` — checks `is_dirty()` and routes through `ConfirmQuit`.
- `render_confirm_quit` — renders `[y]/[q] discard and quit    [n]/Esc cancel    [Ctrl+S] save`, the only place save keybinds are surfaced today.
- `Model::is_dirty` — string-comparison dirty check.

## Findings

### 1. ccstatusline does NOT auto-save — it uses explicit save with smart menu gating

Despite the README's "automatically saved" wording (which refers to "the file location is automatic, you don't have to choose it"), ccstatusline's actual code uses **explicit save with a dirty-state-gated menu**:

- A global `useInput` handler binds `Ctrl+S` to `saveSettings(settings)` (App.tsx).
- `MainMenu.tsx` builds menu items conditionally on `hasChanges`:
  - When dirty: shows "Save & Exit" (value `save`), a separator, "Exit without saving", another separator, "Like ccstatusline? Star us on GitHub".
  - When clean: shows "Exit", a separator, the Star item.
- `hasChanges` is computed by `JSON.stringify(settings) !== JSON.stringify(originalSettings)`; the `save` action calls `saveSettings`, then `setOriginalSettings(cloneSettings(settings))`, then `setHasChanges(false)`, then `exit()`.
- There is no separate "Discard" action; "Exit without saving" simply calls `exit()` without persisting.
- No persistent footer/keybind hint bar — Ctrl+S is invisible until the user has dirtied state and seen the new menu items appear.

This is exactly the pattern the lsm-9o97 bead's "Option B" sketches, with two clarifying details: (a) the apply row is **gated on dirty state**, not always-visible; (b) "Exit without saving" sits next to "Save & Exit" so the discard path is symmetrical.

### 2. TUI prior art splits cleanly into two camps

**Editor-shell-out (no in-app config UI):** lazygit (`e` opens `$EDITOR`, auto-reloads on save), gitui (edit `theme.ron` / `key_bindings.ron` directly), k9s (`:config` opens `$EDITOR`, reactive UI optionally watches disk), gh-dash (edit `config.yml`, restart). These tools sidestep the save-UX question entirely by handing it to the user's text editor — `Ctrl+S` is the editor's convention, not the TUI's. Not directly applicable to linesmith: `linesmith config` exists to avoid hand-editing TOML.

**In-app settings UI:** htop and btop are the closest analogues. They diverge:

- **htop** — F2 setup → F10 to "save and exit". Settings only persist on clean exit; if you're killed by SIGTERM/SIGKILL, "all unsaved changes are lost." This is the same trap as linesmith's Ctrl+S today: a magic keybind users have to know about, and there's a real footgun if they don't exit cleanly. F2 and F10 are at least surfaced in htop's bottom hint bar.
- **btop** — Options menu (Esc/M) applies changes live and persists `btop.conf` at exit; no explicit "save" action. Users get the macOS-Settings mental model. The on-disk persistence is opaque but reliable.

ccstatusline lands between these: explicit save like htop, but **discoverable via a menu item that appears only when needed**, and the menu item literally bundles save with exit ("Save & Exit") so users can't accidentally lose work the way htop's SIGTERM trap allows.

### 3. GUI/web precedent overwhelmingly favors instant apply for "settings"

macOS System Settings, Firefox preferences, VS Code Settings editor, and Chrome settings all auto-apply and auto-persist. GitHub repository settings is the outlier — explicit "Save changes" buttons everywhere, but those settings have **transactional semantics with side effects** (changing a default branch, toggling Issues, etc.) and many fields per panel, neither of which apply to linesmith's per-screen edits.

The cross-platform split observed by HCI literature: macOS users expect autosave; Windows users expect Apply. linesmith's audience is presumed to skew macOS and Linux (CLI tools targeting Claude Code primarily install on Unix-likes); a user-base survey would refine this.

### 4. HCI guidance: pick by mental model, not by efficiency

- **GNOME HIG (instant apply)**: instant apply unless the change takes >1s, has destructive consequences, or requires multiple coordinated fields. linesmith's edits (theme pick, segment add/remove, raw value edit on a single TOML key) all clear that bar — they're each <1ms, idempotent, and locally scoped.
- **Apple HIG (Settings)**: minimize settings; surface task-specific options where they affect things. linesmith's TUI is exactly this — pick a theme, see the preview update.
- **Material Design**: toggles imply immediate apply; explicit save only when the setting can't take effect immediately.
- **Nielsen Norman Group**: removing a Save button is risky if users expect one; autosave demands explicit "Saved" feedback. **The harm comes from removing the button silently, not from autosaving per se.**

The literature converges: **settings panes should auto-apply unless the change is expensive, destructive, or transactional**. linesmith's per-screen edits are none of those.

### 5. linesmith-specific observations

- The TUI is **not a text editor.** Each "screen" edits one structured field (a theme name, a segment list, a raw value for one key). The "try-and-discard" semantic the 2-stage commit preserves is theoretical — there's no way to "experiment with a draft" across screens that isn't already representable as "go back and pick the previous option."
- The current `ConfirmQuit` modal already documents the magic keybind (`[Ctrl+S] save`) — but only as a last-resort hint after the user has already triggered the dirty-quit warning. That's exactly inverted: the keybind is invisible until you're trying to leave.
- `Model::is_dirty` is by-string comparison against `original_text`. Auto-save would simplify this — `original_text` could re-snapshot after each commit, so dirty is always false from a save-state POV.
- linesmith's model is single-file, single-user, atomic-rename. There's no transactional risk: every screen-level commit is already a complete, valid TOML mutation (the items_editor doesn't leave a half-written `[line.0]` table). Auto-save is mechanically safe.
- **Multi-step edits** (e.g., adding 3 segments to a line) DO produce 3 disk writes under per-commit auto-save, but each is small (~hundreds of bytes), atomic, and on a config the user controls. This is closer to "every preference toggle in macOS System Settings hits a plist" than to "every keystroke saves a doc"; the churn is acceptable and matches user mental model.
- Failure modes (disk full, read-only mount): under explicit Ctrl+S, `apply_save` warns and keeps dirty. Under auto-save, the screen-level commit needs to surface "save failed" without rolling back the in-memory edit (because rolling back UI state mid-flow is more confusing than a flash error). Since these are rare and recoverable (free disk space, fix permissions), the UI just needs a persistent "couldn't write to <path>" banner until the next successful save.
- Concurrent external edit (vim while TUI is open) is rare but real. linesmith already has no defense against it under explicit-save either — Ctrl+S clobbers external edits the same way auto-save would. Out of scope for save-UX; would be a separate "external change detected" feature.

### 6. The footer / hint bar is orthogonal but cheap

Every prior-art TUI with an in-app settings UI surfaces keybinds in a bottom hint bar (htop F-key bar, btop's hint line, ratatui ecosystem convention). linesmith currently surfaces keybinds only inside the ConfirmQuit modal. Adding a per-screen hint bar would solve **a separate problem** (Ctrl+S being invisible) without committing to a save-model pivot. But the hint bar by itself doesn't fix the underlying mental-model mismatch — it makes the magic keybind discoverable, which still leaves the user wondering "wait, why isn't this a settings dialog?"

## Conclusions

**Recommendation: per-screen instant-apply (auto-save), merging the in-memory commit and the disk write into one transition.**

The literature is consistent: settings panes auto-apply, document editors save explicitly. linesmith's TUI is a settings pane. Each screen edits one structured field; the "draft across multiple screens" semantic the 2-stage pattern preserves isn't actually used by anything (the preview header doesn't materially benefit from "edited but not saved" — once the user has hit Enter on the theme picker, they want that theme persisted). NN/g's caveat about removing the Save button applies when users expect one — and the feedback that triggered this bead is the inverse signal: the user expects auto-save and finds the explicit-save model surprising.

Specifics:

1. **Each screen's "Enter commits" path also calls `Model::save()`.** Failure surfaces in the warnings panel as today; success is silent.
2. **Drop the ConfirmQuit modal.** With auto-save there's no dirty state at quit time. Esc / q quits unconditionally.
3. **Drop the explicit Ctrl+S keybind** as a user-facing feature. Keep it as a no-op force-flush for one release with a deprecation note in the warnings panel ("Ctrl+S is no longer needed; changes save automatically"), then remove. Honors muscle memory.
4. **Add a footer hint bar** with the new minimal keybind set: `[Enter] confirm  [Esc] back  [q] quit`. Per-screen overrides as needed (e.g., items_editor's `a`/`d`/`m`).
5. **Surface a transient "Saved <field>" toast** at the top of the preview pane on each commit, fading after ~1.5s. Addresses NN/g's "feedback is mandatory" requirement and confirms each pick persisted.
6. **Failure feedback:** if `Model::save()` returns an error, surface a persistent banner ("Couldn't write to <path>: <error>") that stays until the next successful save. Don't roll back the in-memory edit.

Why not Option B (apply menu): It's the smaller change and matches ccstatusline's exact pattern, which is a real argument for parity. But linesmith's screens are smaller than ccstatusline's (single-field per screen vs. ccstatusline's multi-field forms), so the "save batch of edits" model doesn't pay for itself the way it does in ccstatusline. And the user's verbatim feedback explicitly named "auto-save OR apply menu" — between them, auto-save is the one that actually matches the user's mental model rather than just making the existing model discoverable.

Why not "just add a footer hint bar and keep Ctrl+S": this fixes discoverability without fixing the mental-model mismatch. The user feedback was not "I couldn't find the save key" — it was "I shouldn't need to press a save key for this kind of UI."

**If we're wrong about that, the fallback is Option B**, exactly as ccstatusline implements it: dirty-gated "Save & Exit" + "Exit without saving" rows on MainMenu, plus a footer hint bar showing Ctrl+S. This is a strict superset of the current 2-stage pattern with no behavioral change beyond discoverability, so it's a low-risk pivot if the auto-save direction proves controversial.

## Implications / actions

### ADRs this should drive

- **New ADR (proposed): "Config TUI save model: instant apply"** — codify the per-screen auto-save decision, the dropped ConfirmQuit modal, the Ctrl+S deprecation path, the footer hint bar, the saved-toast feedback. Cite GNOME HIG and NN/g for the mental-model framing; cite ccstatusline as the parity-target whose pattern we're explicitly diverging from (with reasoning).

### Beads this implies (file once ADR is accepted)

- Replace `Model::is_dirty()` consumers and rework the 5 edit screens (theme_picker, items_editor, line_picker, type_picker, raw_value_editor) to call save() on commit.
- Remove ConfirmQuit modal + dirty-gated quit logic.
- Footer/status-bar widget (orthogonal but ship together).
- Saved-toast / save-failure banner widget.
- Ctrl+S deprecation: keep as no-op force-flush + warning panel notice for one release, then delete in the release after.
- Test the multi-line `[line.N]` edit churn case under auto-save (3 segment adds = 3 disk writes; verify atomic-rename throughput is fine on slow filesystems / network homedirs).

### Follow-up research needed

- **External-change detection** (vim editing config while TUI is open). Out of scope for save-UX, but the auto-save direction makes this slightly worse (we'll clobber external edits more eagerly than Ctrl+S did). Worth a separate spike before users hit it.
- **Plugin config hot-reload**. If a future plugin reloads on file change, auto-save means it reloads on every Enter — possibly desirable, possibly noisy. Spec it explicitly when plugins land.

## Open questions

- **Should there be a "reset to defaults" action?** Apple HIG and Material Design both suggest yes; ccstatusline has no equivalent. Probably out of scope for the save-UX ADR but worth filing as a discoverability bead.
- **Does the saved-toast belong in the preview pane or the warnings panel?** Preview is more visible; warnings panel is the existing diagnostics channel. The "Saved <field>" message is positive feedback, not a diagnostic — preview pane probably wins.
- **What's the right wording for the deprecation notice?** "Ctrl+S is no longer needed" risks confusing users who didn't know about it; "Changes save automatically" is positive and self-explanatory. Bikeshed in the ADR.
- **Should multi-step edits debounce the disk write?** 3 segments \* 3 atomic-rename writes is ~3× the IO of a batched Apply. Probably premature optimization (these are tiny files), but worth measuring before defending the choice.

## Raw data

### Comparison matrix: in-app config UI save behavior

| Tool                 | Save model                                  | Discoverable?  | Discard path                   | Failure mode                      |
| -------------------- | ------------------------------------------- | -------------- | ------------------------------ | --------------------------------- |
| ccstatusline         | Explicit (Ctrl+S + dirty-gated menu)        | Menu only      | "Exit without saving" menu row | Silent (in-memory loss)           |
| linesmith (today)    | Explicit (Ctrl+S + ConfirmQuit modal)       | Modal only     | ConfirmQuit "y" answer         | Warning panel + dirty stays       |
| htop                 | Explicit (F10 save & exit)                  | Footer F-bar   | Quit without F10               | "Lost on SIGTERM" footgun         |
| btop                 | Implicit (apply on change, persist on exit) | None           | None — changes are live        | Persistence on exit, not on apply |
| macOS Settings       | Instant apply                               | N/A            | None — toggles flip back       | OS-level errors, rare             |
| VS Code Settings     | Instant apply                               | N/A            | None                           | Surfaces inline errors            |
| GitHub repo settings | Explicit "Save changes" button              | Always visible | Navigate away                  | Inline form validation            |
| Firefox preferences  | Instant apply, persist on close             | N/A            | None                           | Silent on disk error              |

### Verbatim quotes worth keeping

- ccstatusline `MainMenu.tsx`: `if (hasChanges) { menuItems.push({ label: '💾 Save & Exit', value: 'save', ... }); ... menuItems.push({ label: '❌ Exit without saving' ... }); }`
- linesmith `render_confirm_quit`: `"  [y]/[q] discard and quit    [n]/Esc cancel    [Ctrl+S] save"` — the only place Ctrl+S is surfaced today
- NN/g, "Don't Prioritize Efficiency Over Expectations": "Removing the Save button takes users out of autopilot mode, forcing them to spend time looking for the omitted button and figuring out what action to take next."
- GNOME HIG: "Update values or settings immediately to reflect the changes made in the window. This is known as 'instant apply'. Do not make the user press an OK or Apply button to make the changes happen, unless either: the change will take more than about one second to apply..."
- Material Design v1: "Toggles are typically used when a setting takes effect immediately... if a setting doesn't take effect immediately and requires user confirmation, a toggle UI isn't the right choice."
- htop issue #949 / #1046: settings only persist on clean F10 exit; "If your terminal session is killed (SIGTERM, SIGKILL), all unsaved changes are lost." This is the failure mode linesmith's current Ctrl+S model also has if a user forgets to press it.
