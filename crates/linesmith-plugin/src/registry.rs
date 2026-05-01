//! Plugin registry: the single source of truth for compiled `.rhai`
//! scripts after discovery. Owns the parsed ASTs + resolved header
//! data. Wrapping a [`CompiledPlugin`] in a `Segment` adapter is the
//! consumer's job (see linesmith-core's `RhaiSegment`), not this
//! module's.
//!
//! [`PluginRegistry::load`] walks the discovery order from
//! [`super::discovery::scan_plugin_dirs`], compiles each script,
//! resolves its `@data_deps` header, and extracts the required
//! `const ID` declaration. Non-fatal errors (compile failure, unknown
//! dep, id collision) are returned alongside the registry so
//! `linesmith doctor` can surface them; a single bad plugin does not
//! abort the whole load.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rhai::{Engine, AST};

use super::discovery::{scan_dirs, scan_plugin_dirs};
use super::errors::{CollisionWinner, PluginError};
use super::header::{parse_data_deps_header, HeaderError};

/// A single compiled plugin ready to be wrapped by a consumer-side
/// `Segment` adapter.
///
/// Field visibility is `pub(crate)` — the registry is the only
/// factory (`compile_plugin` is the sole construction site), and
/// the only mutator. This keeps the non-empty-id, status-first-dep,
/// non-reserved-dep invariants the factory enforces from being
/// silently violated by a third-party caller that constructs the
/// struct directly. Field accessors are `pub` for consumers.
///
/// `declared_deps` is a raw `Vec<String>` of the header-declared dep
/// tokens (always with `"status"` first). The consumer maps these
/// back to its own dep enum at registration time and is responsible
/// for any `&'static` promotion required by its `Segment` trait.
///
/// Construction runs the script's top-level statements once to
/// extract `const ID`; plugin authors with side effects at module
/// scope pay that cost at registry build, not at first render.
#[derive(Debug)]
pub struct CompiledPlugin {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) ast: AST,
    pub(crate) declared_deps: Vec<String>,
}

impl CompiledPlugin {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn declared_deps(&self) -> &[String] {
        &self.declared_deps
    }

    /// Consume the plugin, yielding its constituent fields as a
    /// named-field [`CompiledPluginParts`]. Used by consumer-side
    /// `Segment` adapters that need to take ownership of the `AST`
    /// and the dep list. Named fields keep the call site readable
    /// and let new fields be added without breaking destructures.
    #[must_use]
    pub fn into_parts(self) -> CompiledPluginParts {
        CompiledPluginParts {
            id: self.id,
            path: self.path,
            ast: self.ast,
            declared_deps: self.declared_deps,
        }
    }
}

/// Owned-by-value view of a [`CompiledPlugin`]'s fields, returned by
/// [`CompiledPlugin::into_parts`]. Pure transport DTO — the
/// non-empty-id and status-first-dep invariants `compile_plugin`
/// enforces are implicit on the values, but this struct doesn't
/// re-check them since callers can only obtain it by consuming a
/// registry-built [`CompiledPlugin`].
#[derive(Debug)]
pub struct CompiledPluginParts {
    pub id: String,
    pub path: PathBuf,
    pub ast: AST,
    pub declared_deps: Vec<String>,
}

/// Keyed collection of compiled plugins. Lookup is by `id`; iteration
/// preserves discovery order. Non-fatal load errors (compile failure,
/// unknown dep, id collision) live alongside the compiled plugins so
/// post-load consumers (e.g. `linesmith doctor`) can query them at
/// any point without re-running discovery.
pub struct PluginRegistry {
    plugins: Vec<CompiledPlugin>,
    errors: Vec<PluginError>,
}

impl PluginRegistry {
    /// Discover, compile, and register every plugin across
    /// `config_dirs` plus the default XDG segments directory.
    /// `built_in_ids` is the set of reserved ids that plugins cannot
    /// shadow (plugins attempting to register one of these names are
    /// rejected as `IdCollision`).
    ///
    /// Non-fatal load errors are collected on the returned registry;
    /// query them via [`Self::load_errors`]. A missing or unreadable
    /// directory is not an error — the discovery layer silently
    /// skips it.
    #[must_use]
    pub fn load(config_dirs: &[PathBuf], engine: &Engine, built_in_ids: &[&str]) -> Self {
        Self::load_from_paths(&scan_plugin_dirs(config_dirs), engine, built_in_ids)
    }

    /// Explicit-XDG variant of [`Self::load`]. Passes `xdg_dir`
    /// through to the discovery scan rather than reading
    /// `XDG_CONFIG_HOME` from the process env. Use `None` to skip the
    /// XDG fallback entirely — driver paths pass an env-derived
    /// [`PathBuf`] so test harnesses with a hermetic env snapshot
    /// don't pick up the developer's real `~/.config/linesmith/segments/`.
    #[must_use]
    pub fn load_with_xdg(
        config_dirs: &[PathBuf],
        xdg_dir: Option<&Path>,
        engine: &Engine,
        built_in_ids: &[&str],
    ) -> Self {
        Self::load_from_paths(&scan_dirs(config_dirs, xdg_dir), engine, built_in_ids)
    }

    /// Core load logic: given an already-discovered list of plugin
    /// file paths (in discovery order), compile each one, detect id
    /// collisions, and build the registry.
    fn load_from_paths(paths: &[PathBuf], engine: &Engine, built_in_ids: &[&str]) -> Self {
        let mut plugins = Vec::new();
        let mut errors = Vec::new();
        // Track plugin ids we've already registered → path of the
        // winning (first-discovered) occurrence.
        let mut seen_ids: HashMap<String, PathBuf> = HashMap::new();

        for path in paths {
            match compile_plugin(path, engine) {
                Ok(plugin) => {
                    if built_in_ids.iter().any(|b| *b == plugin.id) {
                        errors.push(PluginError::IdCollision {
                            id: plugin.id,
                            winner: CollisionWinner::BuiltIn,
                            loser_path: path.clone(),
                        });
                        continue;
                    }
                    if let Some(first_path) = seen_ids.get(&plugin.id) {
                        errors.push(PluginError::IdCollision {
                            id: plugin.id.clone(),
                            winner: CollisionWinner::Plugin(first_path.clone()),
                            loser_path: path.clone(),
                        });
                        continue;
                    }
                    seen_ids.insert(plugin.id.clone(), path.clone());
                    plugins.push(plugin);
                }
                Err(err) => errors.push(err),
            }
        }

        Self { plugins, errors }
    }

    /// Non-fatal errors from the most recent load. Includes compile
    /// failures, malformed `@data_deps` headers, unknown dep names,
    /// and id collisions (with built-ins or other plugins). Returns
    /// an empty slice when every plugin loaded cleanly.
    #[must_use]
    pub fn load_errors(&self) -> &[PluginError] {
        &self.errors
    }

    /// Look up a compiled plugin by its `const ID` value.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CompiledPlugin> {
        self.plugins.iter().find(|p| p.id == id)
    }

    /// Iterate every compiled plugin in discovery order.
    pub fn iter(&self) -> impl Iterator<Item = &CompiledPlugin> {
        self.plugins.iter()
    }

    /// Total number of compiled plugins.
    #[must_use]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// `true` when no plugins were discovered or compiled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Consume the registry, yielding every compiled plugin by value.
    /// The segment builder pulls plugins out by id this way to move
    /// each [`CompiledPlugin`] into a consumer-side adapter.
    #[must_use]
    pub fn into_plugins(self) -> Vec<CompiledPlugin> {
        self.plugins
    }
}

/// Compile one plugin file. Reads the source, parses the `@data_deps`
/// header, compiles the AST, and extracts the required `const ID`.
fn compile_plugin(path: &Path, engine: &Engine) -> Result<CompiledPlugin, PluginError> {
    let src = std::fs::read_to_string(path).map_err(|e| PluginError::Compile {
        path: path.to_path_buf(),
        message: format!("read: {e}"),
    })?;

    // Header parse first — surfaces malformed / unknown-dep errors
    // without paying the AST compile cost. `const ID` isn't known
    // yet (the AST hasn't compiled), so these variants carry `path`
    // rather than a plugin `id` field.
    let deps = match parse_data_deps_header(&src) {
        Ok(d) => d,
        Err(HeaderError::Malformed(m)) => {
            return Err(PluginError::MalformedDataDeps {
                path: path.to_path_buf(),
                message: m,
            });
        }
        Err(HeaderError::UnknownDep(name)) => {
            return Err(PluginError::UnknownDataDep {
                path: path.to_path_buf(),
                name,
            });
        }
    };

    let ast = engine.compile(&src).map_err(|e| PluginError::Compile {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;

    // Extract `const ID = "..."` by running the top-level statements.
    // Rhai's `fn` declarations are not executed (they register for
    // later calls), so only `const` / `let` at module level run.
    let mut scope = rhai::Scope::new();
    engine
        .run_ast_with_scope(&mut scope, &ast)
        .map_err(|e| PluginError::Compile {
            path: path.to_path_buf(),
            message: format!("top-level exec: {e}"),
        })?;

    // Distinguish "ID binding absent" from "ID bound to the wrong
    // type" so the error message tells the author what to fix.
    // `Scope::get_value::<String>` collapses both into `None`; use
    // `get` + type inspection instead.
    let id = match scope.get("ID") {
        None => {
            return Err(PluginError::Compile {
                path: path.to_path_buf(),
                message: "missing required `const ID = \"...\"`".into(),
            });
        }
        Some(v) => match v.clone().into_string() {
            Ok(s) => s,
            Err(actual_type) => {
                return Err(PluginError::Compile {
                    path: path.to_path_buf(),
                    message: format!("`const ID` must be a string, found `{actual_type}`"),
                });
            }
        },
    };

    if id.is_empty() {
        return Err(PluginError::Compile {
            path: path.to_path_buf(),
            message: "`const ID` must not be empty".into(),
        });
    }

    Ok(CompiledPlugin {
        id,
        path: path.to_path_buf(),
        ast,
        declared_deps: deps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::build_engine;
    use std::fs;
    use tempfile::TempDir;

    const BUILTINS: &[&str] = &["model", "workspace", "cost"];

    fn write_plugin(dir: &Path, name: &str, src: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, src).expect("write plugin");
        path
    }

    fn deps(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn empty_config_dirs_produces_empty_registry() {
        let engine = build_engine();
        // No dirs configured AND no XDG scan (unit-tested sibling
        // pieces handle XDG); registry loads zero plugins.
        let tmp = TempDir::new().expect("tempdir");
        let reg =
            PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, BUILTINS);
        let errors = reg.load_errors();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(errors.is_empty());
    }

    #[test]
    fn valid_plugin_compiles_and_registers() {
        let engine = build_engine();
        let tmp = TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "foo.rhai",
            r#"
            const ID = "foo";
            fn render(ctx) { () }
            "#,
        );
        let reg =
            PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, BUILTINS);
        let errors = reg.load_errors();
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(reg.len(), 1);
        let plugin = reg.get("foo").expect("registered by id");
        assert_eq!(plugin.id, "foo");
        assert_eq!(plugin.declared_deps, deps(&["status"]));
    }

    #[test]
    fn plugin_with_data_deps_header_resolves_correctly() {
        let engine = build_engine();
        let tmp = TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "u.rhai",
            r#"// @data_deps = ["usage", "git"]
            const ID = "u";
            fn render(ctx) { () }
            "#,
        );
        let reg =
            PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, BUILTINS);
        let errors = reg.load_errors();
        assert!(errors.is_empty());
        let plugin = reg.get("u").expect("registered");
        assert_eq!(plugin.declared_deps, deps(&["status", "usage", "git"]));
    }

    #[test]
    fn missing_id_const_surfaces_compile_error() {
        let engine = build_engine();
        let tmp = TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "noid.rhai",
            r#"
            fn render(ctx) { () }
            "#,
        );
        let reg =
            PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, BUILTINS);
        let errors = reg.load_errors();
        assert!(reg.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], PluginError::Compile { .. }));
        let msg = format!("{}", errors[0]);
        assert!(msg.contains("ID"), "expected ID reference in error: {msg}");
    }

    #[test]
    fn empty_id_string_rejected() {
        let engine = build_engine();
        let tmp = TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "empty_id.rhai",
            r#"
            const ID = "";
            fn render(ctx) { () }
            "#,
        );
        let reg =
            PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, BUILTINS);
        let errors = reg.load_errors();
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], PluginError::Compile { .. }));
    }

    #[test]
    fn syntax_error_surfaces_compile_error() {
        let engine = build_engine();
        let tmp = TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "bad.rhai",
            r#"
            const ID = "bad
            fn render(ctx) { () }
            "#,
        );
        let reg =
            PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, BUILTINS);
        let errors = reg.load_errors();
        assert!(reg.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], PluginError::Compile { .. }));
    }

    #[test]
    fn unknown_data_dep_surfaces_unknown_dep_error() {
        let engine = build_engine();
        let tmp = TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "mystery.rhai",
            r#"// @data_deps = ["mystery"]
            const ID = "mystery";
            fn render(ctx) { () }
            "#,
        );
        let reg =
            PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, BUILTINS);
        let errors = reg.load_errors();
        assert!(reg.is_empty());
        assert_eq!(errors.len(), 1);
        let PluginError::UnknownDataDep { name, .. } = &errors[0] else {
            panic!("expected UnknownDataDep, got {:?}", errors[0]);
        };
        assert_eq!(name, "mystery");
    }

    #[test]
    fn reserved_credentials_dep_surfaces_unknown_dep_error() {
        // `credentials` is plugin-reserved per spec §@data_deps
        // header syntax; must fail as UnknownDataDep at load time.
        let engine = build_engine();
        let tmp = TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "cr.rhai",
            r#"// @data_deps = ["credentials"]
            const ID = "cr";
            fn render(ctx) { () }
            "#,
        );
        let reg =
            PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, BUILTINS);
        let errors = reg.load_errors();
        assert!(reg.is_empty());
        assert!(matches!(errors[0], PluginError::UnknownDataDep { .. }));
    }

    #[test]
    fn malformed_data_deps_surfaces_malformed_error() {
        let engine = build_engine();
        let tmp = TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "mal.rhai",
            r#"// @data_deps = ["usage"
            const ID = "mal";
            fn render(ctx) { () }
            "#,
        );
        let reg =
            PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, BUILTINS);
        let errors = reg.load_errors();
        assert!(reg.is_empty());
        assert!(matches!(errors[0], PluginError::MalformedDataDeps { .. }));
    }

    #[test]
    fn plugin_id_colliding_with_built_in_rejected() {
        let engine = build_engine();
        let tmp = TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "model.rhai",
            r#"
            const ID = "model";
            fn render(ctx) { () }
            "#,
        );
        let reg =
            PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, BUILTINS);
        let errors = reg.load_errors();
        assert!(reg.is_empty());
        let PluginError::IdCollision { winner, .. } = &errors[0] else {
            panic!("expected IdCollision, got {:?}", errors[0]);
        };
        assert_eq!(*winner, CollisionWinner::BuiltIn);
    }

    #[test]
    fn non_string_id_const_surfaces_typed_error() {
        let engine = build_engine();
        let tmp = TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "num_id.rhai",
            r#"
            const ID = 42;
            fn render(ctx) { () }
            "#,
        );
        let reg =
            PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, BUILTINS);
        let errors = reg.load_errors();
        assert!(reg.is_empty());
        let PluginError::Compile { message, .. } = &errors[0] else {
            panic!("expected Compile, got {:?}", errors[0]);
        };
        assert!(
            message.contains("must be a string"),
            "error must distinguish wrong-type from missing: {message}"
        );
    }

    #[test]
    fn duplicate_plugin_id_first_wins_second_rejected() {
        let engine = build_engine();
        let tmp_a = TempDir::new().expect("tempdir");
        let tmp_b = TempDir::new().expect("tempdir");
        let winner = write_plugin(
            tmp_a.path(),
            "x.rhai",
            r#"
            const ID = "dup";
            fn render(ctx) { () }
            "#,
        );
        let loser = write_plugin(
            tmp_b.path(),
            "y.rhai",
            r#"
            const ID = "dup";
            fn render(ctx) { () }
            "#,
        );
        let reg = PluginRegistry::load_with_xdg(
            &[tmp_a.path().to_path_buf(), tmp_b.path().to_path_buf()],
            None,
            &engine,
            BUILTINS,
        );
        let errors = reg.load_errors();
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("dup").expect("first wins").path, winner);
        assert_eq!(errors.len(), 1);
        let PluginError::IdCollision {
            id,
            winner: collision_winner,
            loser_path,
        } = &errors[0]
        else {
            panic!("expected IdCollision, got {:?}", errors[0]);
        };
        assert_eq!(id, "dup");
        assert_eq!(*collision_winner, CollisionWinner::Plugin(winner.clone()));
        assert_eq!(loser_path, &loser);
    }

    #[test]
    fn mix_of_good_and_bad_plugins_registers_good_and_reports_bad() {
        // A bad plugin doesn't block the registry from picking up
        // the good ones — important for a multi-plugin user install
        // where one broken script shouldn't silently drop the rest.
        let engine = build_engine();
        let tmp = TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "a_good.rhai",
            r#"
            const ID = "good";
            fn render(ctx) { () }
            "#,
        );
        write_plugin(
            tmp.path(),
            "b_bad.rhai",
            r#"
            fn render(ctx) { () }
            "#,
        );
        let reg =
            PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, BUILTINS);
        let errors = reg.load_errors();
        assert_eq!(reg.len(), 1);
        assert!(reg.get("good").is_some());
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], PluginError::Compile { .. }));
    }
}
