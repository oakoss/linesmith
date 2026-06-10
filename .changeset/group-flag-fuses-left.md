---
linesmith-core: major
---

**BREAKING (pre-1.0): `LineItem::Segment` gained a `fuses_left: bool` field.**

ADR-0029 adds the `group` color-grouping flag. The builder records group membership on the segment stream through a new `fuses_left` field on `LineItem::Segment` (true when a segment shares a color group with the segment to its left). Because the `Segment` variant is a public, non-`#[non_exhaustive]` struct variant, the field breaks downstream crates that construct or exhaustively match it: add `fuses_left: …` to constructions and `..` to exhaustive matches.

`[line].segments` entries gain an optional `group` flag, orthogonal to `merge`: `group = true` fuses a segment with its right neighbor for color while keeping the separator between them, and `merge = true` implies grouping unless `group = false` overrides. The group-lead color render layer consumes `fuses_left`.
