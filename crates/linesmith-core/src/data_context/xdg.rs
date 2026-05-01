//! Single source of truth for the linesmith XDG cascade.
//!
//! All `linesmith` paths derived from `$XDG_*_HOME` / `$HOME` flow
//! through [`resolve_subdir`]. Three runtime callers used to
//! re-implement the same cascade independently (cache root, segment
//! plugin dir, user theme dir), with the doctor mirroring all three —
//! every consumer was a drift opportunity. Collapsing them here
//! gives one cascade definition + one piece of test surface.
//!
//! The function is pure: it takes [`XdgEnv`] (a snapshot of the
//! relevant env vars) and a [`XdgScope`] tag, returns a `PathBuf` or
//! `None`. Callers build [`XdgEnv`] at the boundary — `driver.rs`
//! from `CliEnv`, `doctor` from its [`crate::doctor::EnvVarState`]
//! snapshot — so the cascade itself never touches `std::env`.

use std::ffi::OsString;
use std::path::PathBuf;

/// Snapshot of the env vars the XDG cascade reads. Only `xdg` and
/// `home` matter; the cascade has no other inputs.
///
/// Fields are `OsString` (not `String`) because Unix paths are
/// byte-strings — a user with `XDG_CACHE_HOME=/srv/café-bin` in a
/// non-UTF-8 locale must not silently lose their setting. `None` and
/// empty-string both count as "not set" — the cascade treats them
/// the same. The [`XdgEnv::from_process_env`] factory normalizes at
/// the boundary so each call site doesn't have to.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct XdgEnv {
    pub(crate) xdg_cache_home: Option<OsString>,
    pub(crate) xdg_config_home: Option<OsString>,
    pub(crate) home: Option<OsString>,
}

impl XdgEnv {
    /// Snapshot the three env vars from the running process, treating
    /// empty strings as unset per the XDG spec. Non-UTF-8 values are
    /// preserved; `Option<OsString>` carries them through to the
    /// cascade unchanged.
    #[must_use]
    pub fn from_process_env() -> Self {
        Self::from_os_options(
            std::env::var_os("XDG_CACHE_HOME"),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }

    /// Build an [`XdgEnv`] from raw `OsString` reads (the shape
    /// `std::env::var_os` returns). Filters empty values to `None`
    /// at the single normalization point.
    #[must_use]
    pub fn from_os_options(
        xdg_cache_home: Option<OsString>,
        xdg_config_home: Option<OsString>,
        home: Option<OsString>,
    ) -> Self {
        fn nonempty(v: Option<OsString>) -> Option<OsString> {
            v.filter(|s| !s.is_empty())
        }
        Self {
            xdg_cache_home: nonempty(xdg_cache_home),
            xdg_config_home: nonempty(xdg_config_home),
            home: nonempty(home),
        }
    }
}

/// Which XDG base spec directory the caller wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum XdgScope {
    /// `$XDG_CACHE_HOME` → `$HOME/.cache`
    Cache,
    /// `$XDG_CONFIG_HOME` → `$HOME/.config`
    Config,
}

/// Resolve `$XDG_<scope>_HOME/linesmith/<sub>` falling back to
/// `$HOME/.<cache|config>/linesmith/<sub>`. `None` when neither
/// source is populated.
///
/// Pass `sub = ""` for the linesmith root (used by
/// [`crate::data_context::cache::default_root`]); pass a sub-name
/// like `"segments"` or `"themes"` for the runtime user-content
/// dirs. Empty `sub` short-circuits the inner helper, which omits
/// the join rather than relying on `PathBuf` to suppress an empty
/// component.
#[must_use]
pub fn resolve_subdir(env: &XdgEnv, scope: XdgScope, sub: &str) -> Option<PathBuf> {
    let (xdg, home_sub) = match scope {
        XdgScope::Cache => (env.xdg_cache_home.as_deref(), ".cache"),
        XdgScope::Config => (env.xdg_config_home.as_deref(), ".config"),
    };
    if let Some(x) = xdg {
        return Some(linesmith_subdir(PathBuf::from(x), sub));
    }
    env.home
        .as_deref()
        .map(|h| linesmith_subdir(PathBuf::from(h).join(home_sub), sub))
}

fn linesmith_subdir(base: PathBuf, sub: &str) -> PathBuf {
    let with_app = base.join("linesmith");
    if sub.is_empty() {
        with_app
    } else {
        with_app.join(sub)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn from_os_options_filters_empty_strings_to_none() {
        let env = XdgEnv::from_os_options(Some(OsString::new()), os("/x"), Some(OsString::new()));
        assert_eq!(env.xdg_cache_home, None);
        assert_eq!(env.xdg_config_home, os("/x"));
        assert_eq!(env.home, None);
    }

    #[test]
    fn cache_scope_prefers_xdg_cache_home_over_home() {
        let env = XdgEnv::from_os_options(os("/xdg"), None, os("/home"));
        assert_eq!(
            resolve_subdir(&env, XdgScope::Cache, ""),
            Some(PathBuf::from("/xdg/linesmith"))
        );
    }

    #[test]
    fn cache_scope_falls_back_to_home_dot_cache() {
        let env = XdgEnv::from_os_options(None, None, os("/home/user"));
        assert_eq!(
            resolve_subdir(&env, XdgScope::Cache, ""),
            Some(PathBuf::from("/home/user/.cache/linesmith"))
        );
    }

    #[test]
    fn config_scope_uses_xdg_config_home() {
        let env = XdgEnv::from_os_options(None, os("/conf"), None);
        assert_eq!(
            resolve_subdir(&env, XdgScope::Config, "segments"),
            Some(PathBuf::from("/conf/linesmith/segments"))
        );
    }

    #[test]
    fn config_scope_falls_back_to_home_dot_config() {
        let env = XdgEnv::from_os_options(None, None, os("/home/user"));
        assert_eq!(
            resolve_subdir(&env, XdgScope::Config, "themes"),
            Some(PathBuf::from("/home/user/.config/linesmith/themes"))
        );
    }

    #[test]
    fn config_scope_does_not_borrow_xdg_cache_home() {
        // A user with $XDG_CACHE_HOME set but $XDG_CONFIG_HOME unset
        // (and $HOME unset) gets `None` for Config — the two scopes
        // do not share env vars.
        let env = XdgEnv::from_os_options(os("/xdg-cache"), None, None);
        assert_eq!(resolve_subdir(&env, XdgScope::Config, ""), None);
    }

    #[test]
    fn returns_none_when_neither_xdg_nor_home_is_set() {
        let env = XdgEnv::default();
        assert_eq!(resolve_subdir(&env, XdgScope::Cache, "x"), None);
        assert_eq!(resolve_subdir(&env, XdgScope::Config, "y"), None);
    }

    #[test]
    fn empty_sub_does_not_append_trailing_slash() {
        let env = XdgEnv::from_os_options(os("/xdg"), None, None);
        let path = resolve_subdir(&env, XdgScope::Cache, "").unwrap();
        assert_eq!(path, PathBuf::from("/xdg/linesmith"));
        assert_eq!(path.components().count(), 3); // "/", "xdg", "linesmith"
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_xdg_cache_home() {
        // Critical regression guard: Unix paths are byte-strings.
        // A user with `XDG_CACHE_HOME=/srv/<latin1-bytes>` must NOT
        // silently fall through to $HOME. The OsString-typed env
        // fields preserve these bytes through the cascade; an
        // earlier `String`-typed implementation would have dropped
        // them via the `var().ok()` collapse to `None`.
        use std::os::unix::ffi::OsStringExt;
        // Latin-1 "café" (\xe9 is non-UTF-8 as a standalone byte).
        let bytes = b"/srv/caf\xe9-bin".to_vec();
        let xdg = OsString::from_vec(bytes.clone());
        let env = XdgEnv::from_os_options(Some(xdg), None, os("/home/user"));
        let resolved = resolve_subdir(&env, XdgScope::Cache, "").unwrap();
        // Result preserves the non-UTF-8 ancestor; uses XDG (not the
        // $HOME fallback) because the XDG var was set + non-empty.
        assert!(
            resolved
                .as_os_str()
                .as_encoded_bytes()
                .starts_with(b"/srv/caf\xe9-bin/linesmith"),
            "expected non-UTF-8 XDG to be preserved: got {:?}",
            resolved
        );
    }
}
