//! Capability- and environment-aware footer warnings.
//!
//! Surfaces remediation hints when the user's terminal can't render
//! the configured theme correctly: `NO_COLOR` set, stdout not a TTY,
//! palette-tier mismatch, VSCode contrast-ratio shim, tmux without
//! truecolor passthrough. Rendered as additional rows in the preview
//! header's warnings panel so the user sees them on every screen.
//!
//! Compute reads a snapshotted [`EnvironmentSnapshot`] rather than
//! process env directly so tests stay deterministic without
//! `unsafe { std::env::set_var }` (which is also `unsafe` since
//! Rust 1.83) and so all checks within one render see a consistent
//! view of the environment. The boot-path call site uses
//! [`EnvironmentSnapshot::from_process`] to capture the live env per
//! render — cheap (a handful of env lookups, all libc-local).

use linesmith_core::theme::Capability;

use crate::config::ColorPolicy;

/// Snapshotted environment signals the warning compute reads.
#[derive(Debug, Default, Clone)]
pub(super) struct EnvironmentSnapshot {
    pub(super) no_color: bool,
    pub(super) term_program: Option<String>,
    /// Collapses two distinct signals (`$TMUX` set OR `$TERM` starts
    /// with `tmux`) into one bool because the v0.1 warning text only
    /// asks "are we under tmux?". Split into a `TmuxSignal` enum if
    /// a future diagnostic needs to disambiguate (e.g., to detect
    /// `screen` started inside tmux).
    pub(super) tmux_active: bool,
}

impl EnvironmentSnapshot {
    pub(super) fn from_process() -> Self {
        // Non-UTF-8 env values collapse to "absent" via `.ok()` /
        // `.unwrap_or(false)`. That's the right answer for the
        // "starts_with('tmux')" / `eq_ignore_ascii_case("vscode")`
        // checks below — neither can match a non-UTF-8 string. A
        // user with non-UTF-8 TERM is rare enough that we don't
        // emit a debug breadcrumb; if the v0.2 follow-up needs it,
        // the surface to extend is small.
        Self {
            no_color: std::env::var_os("NO_COLOR").is_some(),
            term_program: std::env::var("TERM_PROGRAM").ok(),
            tmux_active: std::env::var_os("TMUX").is_some()
                || std::env::var("TERM")
                    .map(|v| v.starts_with("tmux"))
                    .unwrap_or(false),
        }
    }
}

/// Palette tier carried by [`EnvironmentWarning::LimitedPalette`].
/// Subset of [`Capability`] limited to the tiers where colors ARE
/// rendering but at reduced fidelity. The full `Capability` enum
/// also has `None` (colors off) and `TrueColor` (full fidelity) —
/// both excluded here because neither warrants a "palette tier"
/// warning. Compile-time enforcement: a future caller can't
/// construct `LimitedPalette(Capability::TrueColor)` because the
/// type doesn't admit it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LimitedTier {
    Palette16,
    Palette256,
}

impl LimitedTier {
    /// Project a [`Capability`] onto a [`LimitedTier`]. Returns
    /// `None` for `Capability::None` (colors off) and
    /// `Capability::TrueColor` (no degradation) — the compute uses
    /// this at the ladder boundary to convert to the typed payload.
    fn from_capability(cap: Capability) -> Option<Self> {
        match cap {
            Capability::Palette16 => Some(Self::Palette16),
            Capability::Palette256 => Some(Self::Palette256),
            Capability::None | Capability::TrueColor => None,
        }
    }
}

/// One environment warning. Variants are mutually exclusive at the
/// color-availability ladder (NoColorSet > NoColorSupport > LimitedPalette)
/// — only the most-specific applicable variant fires. The terminal-
/// specific variants (VsCodeContrastShim, TmuxNoTrueColorPassthrough)
/// are additive and can co-occur with the ladder variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum EnvironmentWarning {
    /// `NO_COLOR` env var is set under `color = "auto"` policy —
    /// colors are off. Suppressed under `color = "always"` (the
    /// user override wins) and irrelevant under `color = "never"`
    /// (the user already disabled colors).
    NoColorSet,
    /// `supports-color` reports `Capability::None` under
    /// `color = "auto"` despite NO_COLOR being unset. Causes
    /// include `TERM=dumb`, unset/empty `$TERM`, or a piped /
    /// redirected stdout. The original "not a TTY" naming
    /// over-attributed the cause to one specific case; in the TUI,
    /// stdout is always a TTY (crossterm wouldn't initialize
    /// otherwise), so the real reason is almost always a `$TERM`
    /// problem. Suppressed under `color = "always"` (which rescues
    /// to `TrueColor`) and irrelevant under `color = "never"`.
    NoColorSupport,
    /// Terminal advertises a palette tier below truecolor; richer
    /// themes will downgrade to the carried tier.
    LimitedPalette(LimitedTier),
    /// VSCode integrated terminal detected. Its
    /// `terminal.integrated.minimumContrastRatio` setting (default
    /// 4.5) silently shifts theme colors to meet WCAG contrast,
    /// which can make a configured theme render differently than
    /// expected. Set to 1 to disable. Fires under any color policy
    /// since the shim affects rendered colors regardless of how
    /// they reached the terminal.
    VsCodeContrastShim,
    /// tmux detected and capability is below truecolor. Emitted by
    /// `compute_environment_warnings` only when colors are actually
    /// rendering at a reduced tier (Palette16 or Palette256) — if
    /// colors are off (`Capability::None`) the Tc passthrough
    /// remediation wouldn't help, and if colors are at TrueColor
    /// the user's setup is already correct.
    TmuxNoTrueColorPassthrough,
}

impl EnvironmentWarning {
    /// One-line user-facing message. Includes both the symptom and
    /// the remediation imperative so the warning panel is actionable
    /// on its own (no second screen needed).
    #[must_use]
    pub(super) fn message(&self) -> String {
        match self {
            Self::NoColorSet => {
                "NO_COLOR is set; theme colors are disabled. Unset NO_COLOR to enable.".into()
            }
            Self::NoColorSupport => "terminal didn't advertise color support; check `$TERM` (e.g., `export TERM=xterm-256color`).".into(),
            Self::LimitedPalette(tier) => format!(
                "terminal supports only {} colors; truecolor themes will downgrade.",
                match tier {
                    LimitedTier::Palette16 => "16",
                    LimitedTier::Palette256 => "256",
                }
            ),
            Self::VsCodeContrastShim => "VSCode terminal: `terminal.integrated.minimumContrastRatio` may distort theme colors. Set to 1 to disable.".into(),
            Self::TmuxNoTrueColorPassthrough => "tmux active with reduced color tier; if your outer terminal supports truecolor, add `set -ga terminal-overrides ',*:Tc'` to tmux.conf.".into(),
        }
    }
}

/// Pure compute: given the resolved color capability, the user's
/// configured color policy, and a snapshotted environment, produce
/// the list of warnings to surface. Order is stable: ladder variant
/// (at most one), then VSCode, then tmux.
///
/// Color-policy semantics:
/// - **Auto** — emit the full ladder (NoColorSet, NoColorSupport,
///   LimitedPalette) plus terminal-specific additions.
/// - **Always** — user explicitly forced colors via
///   `[layout_options].color = "always"`; suppress the ladder
///   (NO_COLOR / NoColorSupport / LimitedPalette would all
///   misattribute the cause of any reduced rendering). Terminal-
///   specific warnings (VsCodeContrastShim,
///   TmuxNoTrueColorPassthrough) still apply because the shim
///   and tmux still affect rendered colors.
/// - **Never** — user explicitly disabled colors; the panel is
///   silent because every warning in this module is about color
///   rendering, which the user opted out of.
pub(super) fn compute_environment_warnings(
    capability: Capability,
    color_policy: ColorPolicy,
    env: &EnvironmentSnapshot,
) -> Vec<EnvironmentWarning> {
    let mut warnings = Vec::new();

    if matches!(color_policy, ColorPolicy::Never) {
        return warnings;
    }

    if matches!(color_policy, ColorPolicy::Auto) {
        // Color-availability ladder: pick the most-specific
        // applicable variant. NO_COLOR wins because the user
        // explicitly disabled colors; NoColorSupport fires when
        // the terminal didn't advertise color support (TERM=dumb,
        // unset $TERM, etc.); LimitedPalette is the capability-
        // tier signal when colors ARE rendering but downgraded.
        if env.no_color {
            warnings.push(EnvironmentWarning::NoColorSet);
        } else if capability == Capability::None {
            warnings.push(EnvironmentWarning::NoColorSupport);
        } else if let Some(tier) = LimitedTier::from_capability(capability) {
            warnings.push(EnvironmentWarning::LimitedPalette(tier));
        }
    }

    if env
        .term_program
        .as_deref()
        .is_some_and(|p| p.eq_ignore_ascii_case("vscode"))
    {
        warnings.push(EnvironmentWarning::VsCodeContrastShim);
    }

    // Tmux passthrough only helps when colors are rendering at a
    // reduced tier. If capability is None (NO_COLOR / not-TTY /
    // color="never"), Tc passthrough wouldn't restore them; if
    // capability is at TrueColor, the user's setup is correct.
    // Gating on Palette16/Palette256 specifically avoids attributing
    // a tmux-inside-16-color-terminal to a passthrough misconfig.
    if env.tmux_active && LimitedTier::from_capability(capability).is_some() {
        warnings.push(EnvironmentWarning::TmuxNoTrueColorPassthrough);
    }

    warnings
}

/// Prepend environment warnings to a runtime-warnings list. Extracted
/// from `app::view` so the splice ordering can be tested without
/// standing up a `Frame` — a regression to `extend` (append) would
/// silently bury env warnings below transient render warnings.
pub(super) fn prepend_env_warnings(
    warnings: &mut Vec<String>,
    capability: Capability,
    color_policy: ColorPolicy,
    env: &EnvironmentSnapshot,
) {
    let env_warnings = compute_environment_warnings(capability, color_policy, env);
    if env_warnings.is_empty() {
        return;
    }
    let prefixed: Vec<String> = env_warnings
        .iter()
        .map(EnvironmentWarning::message)
        .collect();
    warnings.splice(0..0, prefixed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_env() -> EnvironmentSnapshot {
        EnvironmentSnapshot::default()
    }

    #[test]
    fn no_color_wins_over_palette_tier_under_auto_policy() {
        // NO_COLOR is the user's explicit declaration that they
        // don't want colors — a palette-mismatch warning on top
        // would be noise. Pin that NoColorSet suppresses
        // LimitedPalette under the default Auto policy.
        let env = EnvironmentSnapshot {
            no_color: true,
            ..empty_env()
        };
        let warnings = compute_environment_warnings(Capability::Palette16, ColorPolicy::Auto, &env);
        assert_eq!(warnings, vec![EnvironmentWarning::NoColorSet]);
    }

    #[test]
    fn no_color_support_fires_when_capability_is_none_under_auto_policy() {
        // `Capability::None` without NO_COLOR under Auto means the
        // terminal didn't advertise color support (TERM=dumb, unset
        // TERM, or stdout not a TTY in non-TUI contexts). Distinct
        // remediation from NoColorSet — point at $TERM rather than
        // assuming the user can fix it by toggling an env var.
        let warnings =
            compute_environment_warnings(Capability::None, ColorPolicy::Auto, &empty_env());
        assert_eq!(warnings, vec![EnvironmentWarning::NoColorSupport]);
    }

    #[test]
    fn limited_palette_fires_only_when_below_truecolor_under_auto() {
        // Pin the Palette tier ↔ ladder mapping so an accidental
        // `<=` in the ladder doesn't silently warn at TrueColor.
        for (cap, tier) in [
            (Capability::Palette16, LimitedTier::Palette16),
            (Capability::Palette256, LimitedTier::Palette256),
        ] {
            let warnings = compute_environment_warnings(cap, ColorPolicy::Auto, &empty_env());
            assert_eq!(
                warnings,
                vec![EnvironmentWarning::LimitedPalette(tier)],
                "expected LimitedPalette({tier:?}) for {cap:?}",
            );
        }
        let warnings =
            compute_environment_warnings(Capability::TrueColor, ColorPolicy::Auto, &empty_env());
        assert!(
            warnings.is_empty(),
            "TrueColor capability with empty env emits no warnings",
        );
    }

    #[test]
    fn always_policy_suppresses_ladder_but_keeps_terminal_specific() {
        // Codex P2 fix pin. Under `color = "always"`, the user has
        // overridden auto-detection; ladder warnings would
        // misattribute the cause of any rendering issue. VSCode
        // and tmux warnings still apply because the shim and tmux
        // affect rendered colors regardless of how they got
        // enabled.
        let env = EnvironmentSnapshot {
            no_color: true,
            term_program: Some("vscode".to_string()),
            tmux_active: true,
        };
        let warnings =
            compute_environment_warnings(Capability::Palette16, ColorPolicy::Always, &env);
        assert!(
            !warnings.contains(&EnvironmentWarning::NoColorSet),
            "Always policy must suppress NoColorSet (user override): {warnings:?}",
        );
        assert!(
            !warnings.contains(&EnvironmentWarning::LimitedPalette(LimitedTier::Palette16)),
            "Always policy must suppress LimitedPalette: {warnings:?}",
        );
        assert!(warnings.contains(&EnvironmentWarning::VsCodeContrastShim));
        assert!(warnings.contains(&EnvironmentWarning::TmuxNoTrueColorPassthrough));
    }

    #[test]
    fn never_policy_emits_no_warnings_at_all() {
        // Codex P2 fix pin. Under `color = "never"` every warning
        // in this module is about color rendering, which the user
        // opted out of. Pin the panel-is-silent contract so a
        // future variant addition that bypasses the policy guard
        // surfaces here.
        let env = EnvironmentSnapshot {
            no_color: true,
            term_program: Some("vscode".to_string()),
            tmux_active: true,
        };
        let warnings =
            compute_environment_warnings(Capability::Palette16, ColorPolicy::Never, &env);
        assert!(
            warnings.is_empty(),
            "Never policy must emit no warnings, got {warnings:?}",
        );
    }

    #[test]
    fn vscode_warning_is_additive_with_palette_warning() {
        // Both can fire — palette tells the user the renderer is
        // degraded; VSCode tells them the contrast-ratio shim is on
        // top. Distinct remediations.
        let env = EnvironmentSnapshot {
            term_program: Some("vscode".to_string()),
            ..empty_env()
        };
        let warnings =
            compute_environment_warnings(Capability::Palette256, ColorPolicy::Auto, &env);
        assert_eq!(
            warnings,
            vec![
                EnvironmentWarning::LimitedPalette(LimitedTier::Palette256),
                EnvironmentWarning::VsCodeContrastShim,
            ],
        );
    }

    #[test]
    fn vscode_check_is_case_insensitive() {
        // Pin case-insensitivity so a future tightening to exact-
        // match `vscode` doesn't silently break detection on shells
        // that uppercase `TERM_PROGRAM`.
        for variant in ["vscode", "VSCode", "VSCODE"] {
            let env = EnvironmentSnapshot {
                term_program: Some(variant.to_string()),
                ..empty_env()
            };
            let warnings =
                compute_environment_warnings(Capability::TrueColor, ColorPolicy::Auto, &env);
            assert_eq!(
                warnings,
                vec![EnvironmentWarning::VsCodeContrastShim],
                "term_program={variant:?}",
            );
        }
    }

    #[test]
    fn tmux_warning_only_fires_when_colors_render_at_reduced_tier() {
        // Codex P3 + code-reviewer fix pin. The Tc-passthrough
        // remediation only applies when colors ARE rendering at a
        // reduced tier — at Capability::None the colors are off
        // entirely and Tc passthrough wouldn't help; at TrueColor
        // the user's setup is correct.
        let tmux_env = EnvironmentSnapshot {
            tmux_active: true,
            ..empty_env()
        };
        // Reduced tier → warn.
        for cap in [Capability::Palette16, Capability::Palette256] {
            let warnings = compute_environment_warnings(cap, ColorPolicy::Auto, &tmux_env);
            assert!(
                warnings.contains(&EnvironmentWarning::TmuxNoTrueColorPassthrough),
                "expected tmux warn for {cap:?}",
            );
        }
        // Colors off (NoColorSupport path) → silent (Tc wouldn't help).
        let warnings = compute_environment_warnings(Capability::None, ColorPolicy::Auto, &tmux_env);
        assert!(
            !warnings.contains(&EnvironmentWarning::TmuxNoTrueColorPassthrough),
            "tmux warn must not fire when capability is None",
        );
        // TrueColor → silent (already correct).
        let warnings =
            compute_environment_warnings(Capability::TrueColor, ColorPolicy::Auto, &tmux_env);
        assert!(!warnings.contains(&EnvironmentWarning::TmuxNoTrueColorPassthrough));
    }

    #[test]
    fn three_way_additive_palette_vscode_tmux() {
        // The doc on `EnvironmentWarning` promises additive variants
        // can co-occur with the ladder. Pairwise coverage exists; pin
        // the full-stack case so a regression that accidentally makes
        // tmux exclusive with VSCode (or vice versa) via a stray
        // `else if` slips past every other test.
        let env = EnvironmentSnapshot {
            term_program: Some("vscode".to_string()),
            tmux_active: true,
            ..empty_env()
        };
        let warnings =
            compute_environment_warnings(Capability::Palette256, ColorPolicy::Auto, &env);
        assert_eq!(
            warnings,
            vec![
                EnvironmentWarning::LimitedPalette(LimitedTier::Palette256),
                EnvironmentWarning::VsCodeContrastShim,
                EnvironmentWarning::TmuxNoTrueColorPassthrough,
            ],
            "all three additives must co-occur in declared order",
        );
    }

    #[test]
    fn truecolor_with_clean_env_emits_no_warnings() {
        // The happy path: modern terminal, no env shenanigans, no
        // multiplexer. Pin so a regression that adds an always-on
        // warning surfaces here.
        let warnings =
            compute_environment_warnings(Capability::TrueColor, ColorPolicy::Auto, &empty_env());
        assert!(
            warnings.is_empty(),
            "truecolor + empty env must emit no warnings, got {warnings:?}",
        );
    }

    #[test]
    fn message_renders_remediation_imperative_for_each_variant() {
        // Pin both the symptom keyword AND the remediation imperative
        // so a regression that drops the actionable half (truncates
        // "VSCode terminal: contrast ratio may distort", say) leaves
        // the test red. The variant doc on `message()` explicitly
        // promises both halves.
        let cases = [
            (
                EnvironmentWarning::NoColorSet,
                vec!["NO_COLOR", "Unset NO_COLOR"],
            ),
            (
                EnvironmentWarning::NoColorSupport,
                vec!["color support", "$TERM"],
            ),
            (
                EnvironmentWarning::LimitedPalette(LimitedTier::Palette16),
                vec!["16 colors", "downgrade"],
            ),
            (
                EnvironmentWarning::LimitedPalette(LimitedTier::Palette256),
                vec!["256 colors", "downgrade"],
            ),
            (
                EnvironmentWarning::VsCodeContrastShim,
                vec!["minimumContrastRatio", "Set to 1"],
            ),
            (
                EnvironmentWarning::TmuxNoTrueColorPassthrough,
                vec!["terminal-overrides", "outer terminal"],
            ),
        ];
        for (warn, expected_phrases) in cases {
            let msg = warn.message();
            for phrase in expected_phrases {
                assert!(
                    msg.contains(phrase),
                    "{warn:?}'s message must mention {phrase:?}: {msg}",
                );
            }
        }
    }

    #[test]
    fn prepend_env_warnings_lands_env_warnings_at_index_zero() {
        // Pin the splice ordering — env warnings come BEFORE runtime
        // warnings so the user's terminal context reads first. A
        // regression to `extend` (append) would silently bury env
        // warnings below transient render warnings without failing
        // the compute-level tests.
        let env = EnvironmentSnapshot {
            no_color: true,
            ..empty_env()
        };
        let mut warnings = vec![
            "runtime warning A".to_string(),
            "runtime warning B".to_string(),
        ];
        prepend_env_warnings(
            &mut warnings,
            Capability::Palette16,
            ColorPolicy::Auto,
            &env,
        );
        assert_eq!(warnings.len(), 3);
        assert!(
            warnings[0].contains("NO_COLOR"),
            "env warning must come first, got: {warnings:?}",
        );
        assert_eq!(warnings[1], "runtime warning A");
        assert_eq!(warnings[2], "runtime warning B");
    }

    #[test]
    fn prepend_env_warnings_is_noop_when_compute_returns_empty() {
        // The happy path for `prepend_env_warnings`: clean env +
        // truecolor + auto policy → no env warnings to prepend, the
        // runtime warnings list survives untouched. Pin so a future
        // refactor that always pushes a sentinel warning doesn't
        // silently litter the panel.
        let mut warnings = vec!["runtime only".to_string()];
        prepend_env_warnings(
            &mut warnings,
            Capability::TrueColor,
            ColorPolicy::Auto,
            &empty_env(),
        );
        assert_eq!(warnings, vec!["runtime only".to_string()]);
    }
}
