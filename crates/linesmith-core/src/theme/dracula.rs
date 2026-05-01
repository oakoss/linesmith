//! Dracula theme. Dark variant; we map pink to the primary brand
//! slot and purple to the secondary accent, which matches how
//! Dracula-themed UIs typically present the palette.
//!
//! Palette values are hex literals copied from the upstream spec at
//! <https://draculatheme.com/contribute>. Project license is MIT
//! (<https://github.com/dracula/dracula-theme/blob/master/LICENSE>);
//! color hex values are facts and not copyrightable, but we credit
//! the source per project convention.

use super::{Color, Role, Theme};

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::TrueColor { r, g, b }
}

// https://draculatheme.com/contribute (palette section)
const BACKGROUND: Color = rgb(0x28, 0x2A, 0x36);
const CURRENT_LINE: Color = rgb(0x44, 0x47, 0x5A);
const FOREGROUND: Color = rgb(0xF8, 0xF8, 0xF2);
const COMMENT: Color = rgb(0x62, 0x72, 0xA4);
const CYAN: Color = rgb(0x8B, 0xE9, 0xFD);
const GREEN: Color = rgb(0x50, 0xFA, 0x7B);
const PINK: Color = rgb(0xFF, 0x79, 0xC6);
const PURPLE: Color = rgb(0xBD, 0x93, 0xF9);
const RED: Color = rgb(0xFF, 0x55, 0x55);
const YELLOW: Color = rgb(0xF1, 0xFA, 0x8C);

const fn theme_colors() -> [Option<Color>; Role::COUNT] {
    let mut c = [None; Role::COUNT];
    c[Role::Foreground as usize] = Some(FOREGROUND);
    c[Role::Background as usize] = Some(BACKGROUND);
    c[Role::Muted as usize] = Some(COMMENT);
    c[Role::Primary as usize] = Some(PINK);
    c[Role::Accent as usize] = Some(PURPLE);
    c[Role::Success as usize] = Some(GREEN);
    c[Role::Warning as usize] = Some(YELLOW);
    c[Role::Error as usize] = Some(RED);
    c[Role::Info as usize] = Some(CYAN);
    c[Role::Surface as usize] = Some(CURRENT_LINE);
    c[Role::Border as usize] = Some(COMMENT);
    c
}

pub(super) const DRACULA: Theme = Theme {
    name: "dracula",
    colors: theme_colors(),
};

#[cfg(test)]
mod tests {
    use super::super::{built_in, Capability, Color, Role};

    #[test]
    fn registered_in_builtin_registry() {
        assert!(built_in("dracula").is_some());
    }

    #[test]
    fn primary_maps_to_canonical_pink() {
        // Drift alarm: pink is Dracula's brand color. A regression
        // here means the palette source diverged from upstream.
        let t = built_in("dracula").expect("dracula present");
        assert_eq!(
            t.color(Role::Primary),
            Color::TrueColor {
                r: 0xFF,
                g: 0x79,
                b: 0xC6
            }
        );
    }

    #[test]
    fn dark_theme_foreground_is_light() {
        let t = built_in("dracula").expect("dracula present");
        match t.color(Role::Foreground) {
            Color::TrueColor { r, g, b } => {
                let min = r.min(g).min(b);
                assert!(min > 128, "Dracula text should be light, got ({r},{g},{b})");
            }
            other => panic!("expected TrueColor, got {other:?}"),
        }
    }

    #[test]
    fn extended_dim_roles_fall_through_to_base() {
        let t = built_in("dracula").expect("dracula present");
        assert_eq!(t.color(Role::SuccessDim), t.color(Role::Success));
        assert_eq!(t.color(Role::ErrorDim), t.color(Role::Error));
    }

    #[test]
    fn truecolor_downgrades_to_palette16_without_panicking() {
        let t = built_in("dracula").expect("dracula present");
        for role in [Role::Primary, Role::Success, Role::Error, Role::Info] {
            let downgraded = t.color(role).downgrade(Capability::Palette16);
            assert!(matches!(downgraded, Color::Palette16(_)));
        }
    }
}
