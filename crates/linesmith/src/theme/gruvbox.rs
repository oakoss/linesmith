//! Gruvbox theme (dark medium variant). Bright variants of the
//! aurora-style colors carry the state roles; bg0/fg1 anchor
//! background/foreground per upstream defaults.
//!
//! Role mapping note: `bright_blue` (`#83A598`, blue-leaning cyan)
//! maps to `Accent`, while `bright_aqua` (`#8EC07C`, green-leaning)
//! maps to `Info`. The palette has both `bright_green` and
//! `bright_aqua`, and we reserve the deeper green for `Success` and
//! the lighter aqua for `Info` so the state hues stay visually
//! distinct from the accent role.
//!
//! Palette values are hex literals from the upstream spec at
//! <https://github.com/morhetz/gruvbox>. License is MIT/X11 (declared
//! in the upstream README).

use super::{Color, Role, Theme};

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::TrueColor { r, g, b }
}

const BG0: Color = rgb(0x28, 0x28, 0x28);
const BG2: Color = rgb(0x50, 0x49, 0x45);
const BG3: Color = rgb(0x66, 0x5C, 0x54);
const FG1: Color = rgb(0xEB, 0xDB, 0xB2);
const FG4: Color = rgb(0xA8, 0x99, 0x84);
const BRIGHT_RED: Color = rgb(0xFB, 0x49, 0x34);
const BRIGHT_GREEN: Color = rgb(0xB8, 0xBB, 0x26);
const BRIGHT_YELLOW: Color = rgb(0xFA, 0xBD, 0x2F);
const BRIGHT_BLUE: Color = rgb(0x83, 0xA5, 0x98);
const BRIGHT_PURPLE: Color = rgb(0xD3, 0x86, 0x9B);
const BRIGHT_AQUA: Color = rgb(0x8E, 0xC0, 0x7C);

const fn theme_colors() -> [Option<Color>; Role::COUNT] {
    let mut c = [None; Role::COUNT];
    c[Role::Foreground as usize] = Some(FG1);
    c[Role::Background as usize] = Some(BG0);
    c[Role::Muted as usize] = Some(FG4);
    c[Role::Primary as usize] = Some(BRIGHT_PURPLE);
    c[Role::Accent as usize] = Some(BRIGHT_BLUE);
    c[Role::Success as usize] = Some(BRIGHT_GREEN);
    c[Role::Warning as usize] = Some(BRIGHT_YELLOW);
    c[Role::Error as usize] = Some(BRIGHT_RED);
    c[Role::Info as usize] = Some(BRIGHT_AQUA);
    c[Role::Surface as usize] = Some(BG2);
    c[Role::Border as usize] = Some(BG3);
    c
}

pub(super) const GRUVBOX: Theme = Theme {
    name: "gruvbox",
    colors: theme_colors(),
};

#[cfg(test)]
mod tests {
    use super::super::{built_in, Capability, Color, Role};

    #[test]
    fn registered_in_builtin_registry() {
        assert!(built_in("gruvbox").is_some());
    }

    #[test]
    fn primary_maps_to_canonical_bright_purple() {
        let t = built_in("gruvbox").expect("gruvbox present");
        assert_eq!(
            t.color(Role::Primary),
            Color::TrueColor {
                r: 0xD3,
                g: 0x86,
                b: 0x9B
            }
        );
    }

    #[test]
    fn dark_theme_foreground_is_light() {
        let t = built_in("gruvbox").expect("gruvbox present");
        match t.color(Role::Foreground) {
            Color::TrueColor { r, g, b } => {
                let min = r.min(g).min(b);
                assert!(min > 128, "Gruvbox text should be light, got ({r},{g},{b})");
            }
            other => panic!("expected TrueColor, got {other:?}"),
        }
    }

    #[test]
    fn extended_dim_roles_fall_through_to_base() {
        let t = built_in("gruvbox").expect("gruvbox present");
        assert_eq!(t.color(Role::SuccessDim), t.color(Role::Success));
        assert_eq!(t.color(Role::ErrorDim), t.color(Role::Error));
    }

    #[test]
    fn truecolor_downgrades_to_palette16_without_panicking() {
        let t = built_in("gruvbox").expect("gruvbox present");
        for role in [Role::Primary, Role::Success, Role::Error, Role::Info] {
            let downgraded = t.color(role).downgrade(Capability::Palette16);
            assert!(matches!(downgraded, Color::Palette16(_)));
        }
    }
}
