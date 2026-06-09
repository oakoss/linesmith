---
linesmith-core: major
---

**BREAKING (pre-1.0 minor): `theme::Role` gained a `Timer` variant and is now `#[non_exhaustive]`.**

ADR-0028 adds `Role::Timer`, a neutral tertiary role for the duration/timer family so timers read distinct from `Muted` grey. Because `Role` is a public enum, the addition breaks downstream crates that match it exhaustively; `Role` is now `#[non_exhaustive]` so future role additions stay non-breaking — but downstream exhaustive matches must add a `_ => …` arm.

Catppuccin flavors map `Timer` to pink (`#f5c2e7` in mocha); every other theme falls back to `Muted`. `session_duration` now renders with `Role::Timer`, and `role:timer` is reachable from style strings, user themes (`[roles.extended] timer`), and rhai plugins.
