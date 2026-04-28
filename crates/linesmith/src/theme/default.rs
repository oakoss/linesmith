//! `default` theme. Uses the standard 16-color ANSI palette so it
//! renders on every terminal without needing runtime downgrade.
//! User themes ship truecolor and pay for that downgrade; this one
//! is the conservative anchor for `built_in("default")`, the
//! `default_theme()` fallback, and `cargo test`'s baseline.

use super::{AnsiColor, Color, Role, Theme};

const fn ansi(c: AnsiColor) -> Color {
    Color::Palette16(c)
}

const fn theme_colors() -> [Option<Color>; Role::COUNT] {
    let mut c = [None; Role::COUNT];
    c[Role::Foreground as usize] = Some(ansi(AnsiColor::BrightWhite));
    c[Role::Background as usize] = Some(Color::NoColor);
    c[Role::Muted as usize] = Some(ansi(AnsiColor::BrightBlack));
    c[Role::Primary as usize] = Some(ansi(AnsiColor::BrightMagenta));
    c[Role::Accent as usize] = Some(ansi(AnsiColor::BrightBlue));
    c[Role::Success as usize] = Some(ansi(AnsiColor::BrightGreen));
    c[Role::Warning as usize] = Some(ansi(AnsiColor::BrightYellow));
    c[Role::Error as usize] = Some(ansi(AnsiColor::BrightRed));
    c[Role::Info as usize] = Some(ansi(AnsiColor::BrightCyan));
    c
}

pub(super) const DEFAULT: Theme = Theme {
    name: "default",
    colors: theme_colors(),
};
