//! Nord theme. Dark, blue-leaning; nord8 (Frost light blue) maps to
//! the primary brand slot. Aurora set covers the state roles; Polar
//! Night provides background and elevated surfaces.
//!
//! Palette values are hex literals from the upstream spec at
//! <https://www.nordtheme.com/docs/colors-and-palettes>. Project
//! license is MIT (<https://github.com/nordtheme/nord/blob/main/LICENSE.md>).

use super::{Color, Role, Theme};

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::TrueColor { r, g, b }
}

// https://www.nordtheme.com/docs/colors-and-palettes
const NORD0: Color = rgb(0x2E, 0x34, 0x40); // background
const NORD1: Color = rgb(0x3B, 0x42, 0x52); // surface
const NORD2: Color = rgb(0x43, 0x4C, 0x5E); // border
const NORD3: Color = rgb(0x4C, 0x56, 0x6A); // muted
const NORD6: Color = rgb(0xEC, 0xEF, 0xF4); // foreground
const NORD7: Color = rgb(0x8F, 0xBC, 0xBB); // info / frost teal
const NORD8: Color = rgb(0x88, 0xC0, 0xD0); // primary / frost light blue
const NORD10: Color = rgb(0x5E, 0x81, 0xAC); // accent / frost dark blue
const NORD11: Color = rgb(0xBF, 0x61, 0x6A); // error / aurora red
const NORD13: Color = rgb(0xEB, 0xCB, 0x8B); // warning / aurora yellow
const NORD14: Color = rgb(0xA3, 0xBE, 0x8C); // success / aurora green

const fn theme_colors() -> [Option<Color>; Role::COUNT] {
    let mut c = [None; Role::COUNT];
    c[Role::Foreground as usize] = Some(NORD6);
    c[Role::Background as usize] = Some(NORD0);
    c[Role::Muted as usize] = Some(NORD3);
    c[Role::Primary as usize] = Some(NORD8);
    c[Role::Accent as usize] = Some(NORD10);
    c[Role::Success as usize] = Some(NORD14);
    c[Role::Warning as usize] = Some(NORD13);
    c[Role::Error as usize] = Some(NORD11);
    c[Role::Info as usize] = Some(NORD7);
    c[Role::Surface as usize] = Some(NORD1);
    c[Role::Border as usize] = Some(NORD2);
    c
}

pub(super) const NORD: Theme = Theme {
    name: "nord",
    colors: theme_colors(),
};

#[cfg(test)]
mod tests {
    use super::super::{built_in, Capability, Color, Role};

    #[test]
    fn registered_in_builtin_registry() {
        assert!(built_in("nord").is_some());
    }

    #[test]
    fn primary_maps_to_canonical_nord8() {
        let t = built_in("nord").expect("nord present");
        assert_eq!(
            t.color(Role::Primary),
            Color::TrueColor {
                r: 0x88,
                g: 0xC0,
                b: 0xD0
            }
        );
    }

    #[test]
    fn dark_theme_foreground_is_light() {
        let t = built_in("nord").expect("nord present");
        match t.color(Role::Foreground) {
            Color::TrueColor { r, g, b } => {
                let min = r.min(g).min(b);
                assert!(min > 128, "Nord text should be light, got ({r},{g},{b})");
            }
            other => panic!("expected TrueColor, got {other:?}"),
        }
    }

    #[test]
    fn extended_dim_roles_fall_through_to_base() {
        let t = built_in("nord").expect("nord present");
        assert_eq!(t.color(Role::SuccessDim), t.color(Role::Success));
        assert_eq!(t.color(Role::ErrorDim), t.color(Role::Error));
    }

    #[test]
    fn truecolor_downgrades_to_palette16_without_panicking() {
        let t = built_in("nord").expect("nord present");
        for role in [Role::Primary, Role::Success, Role::Error, Role::Info] {
            let downgraded = t.color(role).downgrade(Capability::Palette16);
            assert!(matches!(downgraded, Color::Palette16(_)));
        }
    }
}
