//! Runtime theme registry construction. Built-ins are always loaded;
//! user themes under `$XDG_CONFIG_HOME/linesmith/themes/` are
//! best-effort.

use std::path::{Path, PathBuf};

use crate::data_context::xdg::{resolve_subdir, XdgEnv, XdgScope};
use crate::theme::ThemeRegistry;

/// `$XDG_CONFIG_HOME/linesmith/themes/` (with `$HOME` fallback) per
/// the cascade in [`crate::data_context::xdg::resolve_subdir`].
/// `None` when neither env var is populated.
#[must_use]
pub(crate) fn user_themes_dir(env: &XdgEnv) -> Option<PathBuf> {
    resolve_subdir(env, XdgScope::Config, "themes")
}

/// Build a [`ThemeRegistry`] from built-ins plus user themes
/// discovered under `user_themes_dir`. Pass `None` to skip user-
/// theme loading entirely (test harnesses, no XDG/HOME env). The
/// loader is best-effort — `on_warn` is called for theme-loader
/// diagnostics: malformed files, name collisions (built-in
/// override, duplicate user theme), and unreadable directory
/// entries. Pass `|_| {}` to discard.
pub(crate) fn build_theme_registry(
    user_themes_dir: Option<&Path>,
    on_warn: impl FnMut(&str),
) -> ThemeRegistry {
    let mut registry = ThemeRegistry::with_built_ins();
    if let Some(dir) = user_themes_dir {
        registry = registry.with_user_themes(dir, on_warn);
    }
    registry
}
