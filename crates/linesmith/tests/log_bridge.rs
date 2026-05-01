//! End-to-end check that plugin `log()` output flows through the
//! `LINESMITH_LOG`-gated host emitter installed by
//! `linesmith-core::plugins::build_engine`.
//!
//! `linesmith-plugin`'s `emit_warn` falls back to a raw `eprintln!`
//! when no host emitter is installed. That's fine for unit tests
//! and direct embedders, but a consumer that builds an engine
//! without the warn-emitter wrapper would put plugin diagnostics on
//! stderr regardless of `LINESMITH_LOG=off` — a silent regression.
//! The wrapper at `linesmith-core::plugins::build_engine` is the
//! chokepoint that closes that hole; this test pins the wiring at
//! integration level so a future entry point that bypasses the
//! wrapper surfaces here rather than in production.
//!
//! Subprocess shape: `linesmith_plugin::engine::WARN_EMITTER` is a
//! `OnceLock` — one shot per process. Running each variant in a
//! fresh subprocess (via `CARGO_BIN_EXE_linesmith`) sidesteps the
//! isolation problem in-process tests would have to solve. No
//! extra dev-dep — cargo populates the env var automatically for
//! integration tests.

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

use tempfile::TempDir;

const STATUSLINE_PAYLOAD: &[u8] = include_bytes!("fixtures/claude_minimal.json");

/// Plugin that calls `log("hello")` once, then renders a literal
/// string so the segment is visible. One render per subprocess, so
/// the plugin-host's per-process `LOG_LINES_PER_PLUGIN` rate-limit
/// can't suppress it.
const LOG_PLUGIN: &str = r#"
    const ID = "echo";
    fn render(ctx) {
        log("hello");
        #{ runs: [#{ text: "echo" }] }
    }
"#;

/// Minimal config that activates only the `echo` plugin so the
/// segment's `log()` call dominates stderr — no other built-in
/// segments running. `[line]` per `docs/specs/config.md`.
const CONFIG: &str = r#"
    [line]
    segments = ["echo"]
"#;

struct Fixture {
    _xdg: TempDir,
    xdg_path: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let xdg = TempDir::new().expect("tempdir");
        let segments_dir = xdg.path().join("linesmith").join("segments");
        fs::create_dir_all(&segments_dir).expect("mkdir segments/");
        fs::write(segments_dir.join("echo.rhai"), LOG_PLUGIN).expect("write plugin");
        let config_dir = xdg.path().join("linesmith");
        fs::write(config_dir.join("config.toml"), CONFIG).expect("write config");
        let xdg_path = xdg.path().to_path_buf();
        Self {
            _xdg: xdg,
            xdg_path,
        }
    }
}

/// Run the linesmith binary with the fixture's XDG dir + an explicit
/// `LINESMITH_LOG` value. Returns captured stderr (UTF-8). Panics on
/// non-zero exit so a config / plugin breakage surfaces directly.
fn run_with_log_level(fixture: &Fixture, level: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_linesmith"))
        // Inherit the test runner's environment so platform-required
        // vars (Windows: SystemRoot, TEMP, USERPROFILE; Unix: HOME,
        // TERM) reach the spawned binary. Override only the inputs
        // this test cares about. PATH leaks in via inheritance and
        // matters because credential resolution on macOS shells out
        // to `security` (data_context::credentials::run_security)
        // and future git shell-outs need it; not testing that path
        // here, but the binary's bootstrap doesn't fail before
        // reaching the render path.
        .env("XDG_CONFIG_HOME", &fixture.xdg_path)
        .env("LINESMITH_LOG", level)
        // LINESMITH_CONFIG would override the XDG_CONFIG_HOME
        // discovery and break the fixture's config-loading. Strip
        // it from the inherited env so a developer with a personal
        // override doesn't make this test resolve elsewhere.
        .env_remove("LINESMITH_CONFIG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn linesmith binary");
    child
        .stdin
        .as_mut()
        .expect("stdin pipe")
        .write_all(STATUSLINE_PAYLOAD)
        .expect("write stdin payload");
    let out = child.wait_with_output().expect("wait_with_output");
    assert!(
        out.status.success(),
        "linesmith exited {:?}\nstderr: {}\nstdout: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    // Sanity: the plugin actually rendered (which means the engine
    // ran and `log()` fired). If the segment didn't render, the test
    // is asserting against an emitter path that didn't execute.
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.contains("echo"),
        "expected `echo` segment in stdout, got {stdout:?}"
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn plugin_log_emits_warn_line_when_level_warn() {
    // LINESMITH_LOG=warn — the host emitter routes the plugin's
    // `log("hello")` through `linesmith_core::logging::emit(Level::Warn, ...)`,
    // which writes `linesmith [warn]: plugin echo: hello` to stderr.
    let fixture = Fixture::new();
    let stderr = run_with_log_level(&fixture, "warn");
    assert!(
        stderr.contains("linesmith [warn]: plugin echo: hello"),
        "expected the plugin log line in stderr, got {stderr:?}"
    );
}

#[test]
fn plugin_log_suppressed_when_level_off() {
    // LINESMITH_LOG=off — the level gate inside
    // `linesmith_core::logging::emit` short-circuits before any
    // write happens, so the plugin's `log("hello")` never reaches
    // stderr. Any future entry point that builds a plugin engine
    // without going through `linesmith-core::plugins::build_engine`
    // (the wrapper that installs the host bridge) would fall back
    // to the plugin crate's raw `eprintln!`, re-opening the
    // suppression hole this assertion guards against.
    let fixture = Fixture::new();
    let stderr = run_with_log_level(&fixture, "off");
    assert!(
        !stderr.contains("plugin echo: hello"),
        "expected plugin log line suppressed, got {stderr:?}"
    );
    // Defense-in-depth: also confirm no `linesmith [warn]` prefix
    // anywhere in the captured stderr. Notably no colon — the
    // host-emitter format is `linesmith [warn]:` (with colon) but
    // the plugin crate's `eprintln!` fallback writes `linesmith
    // [warn] {msg}` (without colon). Matching the prefix catches
    // both paths so a regression that triggers the colonless
    // fallback can't slip past this assertion.
    assert!(
        !stderr.contains("linesmith [warn]"),
        "no warn-level lines should appear under LINESMITH_LOG=off, got {stderr:?}"
    );
}
