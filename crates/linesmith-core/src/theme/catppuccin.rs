//! Catppuccin theme flavors. Role mappings follow the palette's
//! canonical usage: mauve is the brand accent, blue is the secondary,
//! green/yellow/red/teal cover the state roles. Dim variants are left
//! unset so `Theme::color`'s fallback chain resolves them to their
//! base role — Catppuccin doesn't ship explicit dim shades.
//!
//! Palette values come from the `catppuccin` crate so palette drift
//! upstream is caught by a dependency version bump rather than a
//! manual hex-value sync.

use super::{Color, Role, Theme};

const fn rgb(c: catppuccin::Rgb) -> Color {
    Color::TrueColor {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

/// Map one Catppuccin flavor's palette into a 16-slot role array.
/// Every flavor shares the same role→palette-name mapping; only the
/// concrete RGB values differ.
const fn flavor_to_theme_colors(p: &catppuccin::FlavorColors) -> [Option<Color>; Role::COUNT] {
    let mut c = [None; Role::COUNT];
    c[Role::Foreground as usize] = Some(rgb(p.text.rgb));
    c[Role::Background as usize] = Some(rgb(p.base.rgb));
    // `subtext0` is Catppuccin's canonical "secondary text that still
    // reads clearly." `overlay0` is the UI-overlay grey (borders,
    // dividers) and sits too dim on dark flavors to carry statusline
    // text like cost/effort.
    c[Role::Muted as usize] = Some(rgb(p.subtext0.rgb));
    c[Role::Primary as usize] = Some(rgb(p.mauve.rgb));
    c[Role::Accent as usize] = Some(rgb(p.blue.rgb));
    c[Role::Success as usize] = Some(rgb(p.green.rgb));
    c[Role::Warning as usize] = Some(rgb(p.yellow.rgb));
    c[Role::Error as usize] = Some(rgb(p.red.rgb));
    c[Role::Info as usize] = Some(rgb(p.teal.rgb));
    c[Role::Surface as usize] = Some(rgb(p.surface0.rgb));
    c[Role::Border as usize] = Some(rgb(p.surface2.rgb));
    // SuccessDim / WarningDim / ErrorDim / PrimaryDim / AccentDim stay
    // None; `Theme::color`'s fallback chain resolves them to the base
    // role, which matches Catppuccin's single-shade-per-hue palette.
    c
}

pub(super) const LATTE: Theme = Theme {
    name: "catppuccin-latte",
    colors: flavor_to_theme_colors(&catppuccin::PALETTE.latte.colors),
};

pub(super) const FRAPPE: Theme = Theme {
    name: "catppuccin-frappe",
    colors: flavor_to_theme_colors(&catppuccin::PALETTE.frappe.colors),
};

pub(super) const MACCHIATO: Theme = Theme {
    name: "catppuccin-macchiato",
    colors: flavor_to_theme_colors(&catppuccin::PALETTE.macchiato.colors),
};

pub(super) const MOCHA: Theme = Theme {
    name: "catppuccin-mocha",
    colors: flavor_to_theme_colors(&catppuccin::PALETTE.mocha.colors),
};

#[cfg(test)]
mod tests {
    use super::super::{built_in, builtin_names, Capability, Color, Role};

    #[test]
    fn all_four_flavors_registered() {
        for name in [
            "catppuccin-latte",
            "catppuccin-frappe",
            "catppuccin-macchiato",
            "catppuccin-mocha",
        ] {
            assert!(
                built_in(name).is_some(),
                "expected {name} in built-in registry"
            );
        }
    }

    #[test]
    fn builtin_names_lists_all_four_catppuccin_flavors() {
        let names: Vec<&str> = builtin_names().collect();
        for flavor in [
            "catppuccin-latte",
            "catppuccin-frappe",
            "catppuccin-macchiato",
            "catppuccin-mocha",
        ] {
            assert!(names.contains(&flavor), "missing {flavor} in builtin_names");
        }
    }

    #[test]
    fn every_flavor_maps_primary_to_its_canonical_mauve() {
        // Drift alarm. RGB values come from the Catppuccin v2 spec;
        // when the `catppuccin` crate bumps and shifts a palette, this
        // test fails per-flavor so the drift gets a deliberate review.
        for (name, r, g, b) in [
            ("catppuccin-latte", 136, 57, 239),
            ("catppuccin-frappe", 202, 158, 230),
            ("catppuccin-macchiato", 198, 160, 246),
            ("catppuccin-mocha", 203, 166, 247),
        ] {
            let t = built_in(name).expect(name);
            assert_eq!(
                t.color(Role::Primary),
                Color::TrueColor { r, g, b },
                "mauve drift in {name}"
            );
        }
    }

    #[test]
    fn latte_light_theme_maps_foreground_to_dark_text() {
        // Latte is the light flavor; text should be dark (low value
        // per-channel), distinguishing it from Mocha (bright text).
        let t = built_in("catppuccin-latte").expect("latte present");
        match t.color(Role::Foreground) {
            Color::TrueColor { r, g, b } => {
                let max = r.max(g).max(b);
                assert!(max < 128, "Latte text should be dark, got ({r},{g},{b})");
            }
            other => panic!("expected TrueColor, got {other:?}"),
        }
    }

    #[test]
    fn mocha_dark_theme_maps_foreground_to_light_text() {
        // Mocha is dark; text should be bright.
        let t = built_in("catppuccin-mocha").expect("mocha present");
        match t.color(Role::Foreground) {
            Color::TrueColor { r, g, b } => {
                let min = r.min(g).min(b);
                assert!(min > 128, "Mocha text should be light, got ({r},{g},{b})");
            }
            other => panic!("expected TrueColor, got {other:?}"),
        }
    }

    #[test]
    fn extended_dim_roles_fall_through_to_base_roles() {
        // Catppuccin flavors leave *_dim variants unset; fallback
        // chain should resolve them to the base role.
        let t = built_in("catppuccin-mocha").expect("mocha present");
        assert_eq!(t.color(Role::SuccessDim), t.color(Role::Success));
        assert_eq!(t.color(Role::ErrorDim), t.color(Role::Error));
        assert_eq!(t.color(Role::PrimaryDim), t.color(Role::Primary));
    }

    #[test]
    fn every_flavor_maps_every_base_role() {
        // Contract: no flavor leaves a base role unmapped. Catches a
        // field-rename upstream where e.g. `mauve` moves and we forget
        // to update `flavor_to_theme_colors`.
        for name in [
            "catppuccin-latte",
            "catppuccin-frappe",
            "catppuccin-macchiato",
            "catppuccin-mocha",
        ] {
            let t = built_in(name).expect(name);
            for role in [
                Role::Foreground,
                Role::Background,
                Role::Muted,
                Role::Primary,
                Role::Accent,
                Role::Success,
                Role::Warning,
                Role::Error,
                Role::Info,
            ] {
                assert!(
                    !matches!(t.color(role), Color::NoColor),
                    "{name} left {role:?} as NoColor",
                );
            }
        }
    }

    #[test]
    fn mocha_downgrades_truecolor_to_palette16_without_panicking() {
        // Catppuccin is truecolor; verify the downgrade path doesn't
        // crash on Mocha's palette values. This is the one built-in
        // that exercises `rgb_to_ansi16` end-to-end — `default` is
        // Palette16 and `minimal` is NoColor.
        let t = built_in("catppuccin-mocha").expect("mocha present");
        for role in [
            Role::Primary,
            Role::Success,
            Role::Warning,
            Role::Error,
            Role::Info,
        ] {
            let downgraded = t.color(role).downgrade(Capability::Palette16);
            assert!(
                matches!(downgraded, Color::Palette16(_)),
                "{role:?} didn't downgrade to Palette16: {downgraded:?}",
            );
        }
    }
}
