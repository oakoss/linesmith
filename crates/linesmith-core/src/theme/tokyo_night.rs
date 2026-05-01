//! Tokyo Night theme (Storm variant). Magenta maps to the primary
//! brand slot; blue/cyan cover info-tier roles; storm-style
//! background tones differentiate elevated surfaces. Other variants
//! (Night, Moon, Day) can ship as siblings if there's user demand.
//!
//! Palette values are hex literals from the upstream Storm palette
//! at <https://github.com/folke/tokyonight.nvim>. License is
//! Apache 2.0 (<https://github.com/folke/tokyonight.nvim/blob/main/LICENSE>).

use super::{Color, Role, Theme};

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::TrueColor { r, g, b }
}

const BACKGROUND: Color = rgb(0x24, 0x28, 0x3B);
const BACKGROUND_ALT: Color = rgb(0x1F, 0x23, 0x35);
const FOREGROUND: Color = rgb(0xC0, 0xCA, 0xF5);
const COMMENT: Color = rgb(0x56, 0x5F, 0x89);
const BLUE: Color = rgb(0x7A, 0xA2, 0xF7);
const CYAN: Color = rgb(0x7D, 0xCF, 0xFF);
const GREEN: Color = rgb(0x9E, 0xCE, 0x6A);
const RED: Color = rgb(0xF7, 0x76, 0x8E);
const YELLOW: Color = rgb(0xE0, 0xAF, 0x68);
const MAGENTA: Color = rgb(0xBB, 0x9A, 0xF7);

const fn theme_colors() -> [Option<Color>; Role::COUNT] {
    let mut c = [None; Role::COUNT];
    c[Role::Foreground as usize] = Some(FOREGROUND);
    c[Role::Background as usize] = Some(BACKGROUND);
    c[Role::Muted as usize] = Some(COMMENT);
    c[Role::Primary as usize] = Some(MAGENTA);
    c[Role::Accent as usize] = Some(BLUE);
    c[Role::Success as usize] = Some(GREEN);
    c[Role::Warning as usize] = Some(YELLOW);
    c[Role::Error as usize] = Some(RED);
    c[Role::Info as usize] = Some(CYAN);
    c[Role::Surface as usize] = Some(BACKGROUND_ALT);
    c[Role::Border as usize] = Some(COMMENT);
    c
}

pub(super) const TOKYO_NIGHT: Theme = Theme {
    name: "tokyo-night",
    colors: theme_colors(),
};

#[cfg(test)]
mod tests {
    use super::super::{built_in, Capability, Color, Role};

    #[test]
    fn registered_in_builtin_registry() {
        assert!(built_in("tokyo-night").is_some());
    }

    #[test]
    fn primary_maps_to_canonical_magenta() {
        let t = built_in("tokyo-night").expect("tokyo-night present");
        assert_eq!(
            t.color(Role::Primary),
            Color::TrueColor {
                r: 0xBB,
                g: 0x9A,
                b: 0xF7
            }
        );
    }

    #[test]
    fn dark_theme_foreground_is_light() {
        let t = built_in("tokyo-night").expect("tokyo-night present");
        match t.color(Role::Foreground) {
            Color::TrueColor { r, g, b } => {
                let min = r.min(g).min(b);
                assert!(
                    min > 128,
                    "Tokyo Night text should be light, got ({r},{g},{b})"
                );
            }
            other => panic!("expected TrueColor, got {other:?}"),
        }
    }

    #[test]
    fn extended_dim_roles_fall_through_to_base() {
        let t = built_in("tokyo-night").expect("tokyo-night present");
        assert_eq!(t.color(Role::SuccessDim), t.color(Role::Success));
        assert_eq!(t.color(Role::ErrorDim), t.color(Role::Error));
    }

    #[test]
    fn truecolor_downgrades_to_palette16_without_panicking() {
        let t = built_in("tokyo-night").expect("tokyo-night present");
        for role in [Role::Primary, Role::Success, Role::Error, Role::Info] {
            let downgraded = t.color(role).downgrade(Capability::Palette16);
            assert!(matches!(downgraded, Color::Palette16(_)));
        }
    }
}
