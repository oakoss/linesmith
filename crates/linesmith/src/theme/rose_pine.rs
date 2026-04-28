//! Rose Pine theme (main flavor). Iris maps to the primary brand
//! slot. The palette has no true green, so Foam (cyan-teal) covers
//! both Success and Info — Rose-Pine-themed apps typically reuse
//! Foam for success states rather than minting an off-palette green;
//! the spec itself doesn't prescribe a success role.
//!
//! Palette values are hex literals from the upstream spec at
//! <https://github.com/rose-pine/rose-pine-theme>. License is MIT
//! (<https://github.com/rose-pine/rose-pine-theme/blob/main/LICENSE>).

use super::{Color, Role, Theme};

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::TrueColor { r, g, b }
}

const BASE: Color = rgb(0x19, 0x17, 0x24);
const SURFACE: Color = rgb(0x1F, 0x1D, 0x2E);
const OVERLAY: Color = rgb(0x26, 0x23, 0x3A);
const MUTED: Color = rgb(0x6E, 0x6A, 0x86);
const TEXT: Color = rgb(0xE0, 0xDE, 0xF4);
const LOVE: Color = rgb(0xEB, 0x6F, 0x92);
const GOLD: Color = rgb(0xF6, 0xC1, 0x77);
const PINE: Color = rgb(0x31, 0x74, 0x8F);
const FOAM: Color = rgb(0x9C, 0xCF, 0xD8);
const IRIS: Color = rgb(0xC4, 0xA7, 0xE7);

const fn theme_colors() -> [Option<Color>; Role::COUNT] {
    let mut c = [None; Role::COUNT];
    c[Role::Foreground as usize] = Some(TEXT);
    c[Role::Background as usize] = Some(BASE);
    c[Role::Muted as usize] = Some(MUTED);
    c[Role::Primary as usize] = Some(IRIS);
    c[Role::Accent as usize] = Some(PINE);
    // No green in the palette; Foam is the upstream stand-in for
    // success-tier UI states.
    c[Role::Success as usize] = Some(FOAM);
    c[Role::Warning as usize] = Some(GOLD);
    c[Role::Error as usize] = Some(LOVE);
    c[Role::Info as usize] = Some(FOAM);
    c[Role::Surface as usize] = Some(SURFACE);
    c[Role::Border as usize] = Some(OVERLAY);
    c
}

pub(super) const ROSE_PINE: Theme = Theme {
    name: "rose-pine",
    colors: theme_colors(),
};

#[cfg(test)]
mod tests {
    use super::super::{built_in, Capability, Color, Role};

    #[test]
    fn registered_in_builtin_registry() {
        assert!(built_in("rose-pine").is_some());
    }

    #[test]
    fn primary_maps_to_canonical_iris() {
        let t = built_in("rose-pine").expect("rose-pine present");
        assert_eq!(
            t.color(Role::Primary),
            Color::TrueColor {
                r: 0xC4,
                g: 0xA7,
                b: 0xE7
            }
        );
    }

    #[test]
    fn dark_theme_foreground_is_light() {
        let t = built_in("rose-pine").expect("rose-pine present");
        match t.color(Role::Foreground) {
            Color::TrueColor { r, g, b } => {
                let min = r.min(g).min(b);
                assert!(
                    min > 128,
                    "Rose Pine text should be light, got ({r},{g},{b})"
                );
            }
            other => panic!("expected TrueColor, got {other:?}"),
        }
    }

    #[test]
    fn success_and_info_both_map_to_foam() {
        // Pin the no-green compromise: a future palette refresh
        // upstream would be the only reason to change this.
        let t = built_in("rose-pine").expect("rose-pine present");
        assert_eq!(t.color(Role::Success), t.color(Role::Info));
    }

    #[test]
    fn extended_dim_roles_fall_through_to_base() {
        let t = built_in("rose-pine").expect("rose-pine present");
        assert_eq!(t.color(Role::SuccessDim), t.color(Role::Success));
        assert_eq!(t.color(Role::ErrorDim), t.color(Role::Error));
    }

    #[test]
    fn truecolor_downgrades_to_palette16_without_panicking() {
        let t = built_in("rose-pine").expect("rose-pine present");
        for role in [Role::Primary, Role::Success, Role::Error, Role::Info] {
            let downgraded = t.color(role).downgrade(Capability::Palette16);
            assert!(matches!(downgraded, Color::Palette16(_)));
        }
    }
}
