//! `minimal` theme. Every role is unset so `Theme::color`'s fallback
//! chain returns `NoColor`, forcing decoration-only output (bold /
//! dim / italic / underline). Useful for users who want zero color
//! noise and rely on shell-level styling, or for diagnostic captures
//! where ANSI bytes would clutter the comparison.

use super::{Role, Theme};

pub(super) const MINIMAL: Theme = Theme {
    name: "minimal",
    colors: [None; Role::COUNT],
};
