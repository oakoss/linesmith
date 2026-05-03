use std::io::Cursor;
use std::process::Command;
use std::str::FromStr;

const CLAUDE_MINIMAL: &str = include_str!("fixtures/claude_minimal.json");
const CLAUDE_WORKTREE: &str = include_str!("fixtures/claude_worktree.json");

/// Run `git <args>` inside `cwd` with an isolated config (global /
/// system configs bypassed, hooks disabled, signing off, default
/// branch pinned). Panics with the child's stderr + exit code on
/// failure so CI logs carry enough context to diagnose without
/// re-running locally.
fn run_git(cwd: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "commit.gpgsign=false"])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(["-c", "init.defaultBranch=main"])
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} in {cwd:?} exited {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn renders_model_and_workspace_when_outside_worktree() {
    let mut out = Vec::new();
    linesmith_core::run(Cursor::new(CLAUDE_MINIMAL), &mut out).expect("run ok");
    assert_eq!(
        String::from_utf8(out).expect("utf8"),
        "Claude Sonnet 4.6 linesmith\n"
    );
}

#[test]
fn renders_full_payload_with_cost_effort_and_workspace() {
    // Rate-limit segments are opt-in, so a first-run user doesn't
    // trigger a Keychain prompt from the default line.
    let mut out = Vec::new();
    linesmith_core::run(Cursor::new(CLAUDE_WORKTREE), &mut out).expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");

    for substring in [
        "Claude Sonnet 4.6",
        "42% · 200k",
        "$1.23",
        "high",
        "linesmith",
    ] {
        assert!(
            rendered.contains(substring),
            "expected {substring:?} in {rendered:?}"
        );
    }
    // Guard: workspace sources worktree-name from gix discovery now,
    // not stdin. `run()` leaves cwd unset, so the hybrid form must
    // NOT appear here — even though the fixture carries a
    // `git_worktree` field.
    assert!(
        !rendered.contains("linesmith/"),
        "workspace must not emit hybrid form without a real linked-worktree cwd: {rendered:?}"
    );
    for absent in ["5h", "7d", "rate_limit"] {
        assert!(
            !rendered.contains(absent),
            "{absent:?} should not appear without explicit opt-in ({rendered:?})",
        );
    }
    assert!(rendered.ends_with('\n'));
}

#[test]
fn malformed_json_exits_zero_with_marker_line() {
    let mut out = Vec::new();
    linesmith_core::run(Cursor::new(b"{not json"), &mut out).expect("run should not error");
    assert_eq!(String::from_utf8(out).expect("utf8"), "?\n");
}

#[test]
fn narrow_terminal_drops_cost_and_effort_first() {
    // Budget chosen so the two highest drop-priorities (cost, effort)
    // drop before context_window or workspace get touched.
    let mut out = Vec::new();
    linesmith_core::run_with_width(Cursor::new(CLAUDE_WORKTREE), &mut out, 40).expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");
    assert!(!rendered.contains("$1.23"), "cost should drop first");
    assert!(!rendered.contains("high"), "effort should drop second");
    assert!(rendered.contains("42% · 200k"));
    assert!(rendered.contains("linesmith"));
    assert!(
        !rendered.contains("linesmith/"),
        "no worktree cwd here: {rendered:?}"
    );
}

#[test]
fn extreme_narrow_keeps_only_lowest_priority_segments() {
    // Budget tight enough that only workspace (lowest drop-priority)
    // survives, even though context_window would fit alone.
    let mut out = Vec::new();
    linesmith_core::run_with_width(Cursor::new(CLAUDE_WORKTREE), &mut out, 10).expect("run ok");
    assert_eq!(String::from_utf8(out).expect("utf8"), "linesmith\n");
}

#[test]
fn xdg_plugin_renders_via_full_driver_path() {
    // Pins the `cli_main → load_plugins → build_segments → RhaiSegment::render`
    // chain end-to-end with a real .rhai file under XDG.
    use std::fs;
    use tempfile::TempDir;

    let xdg = TempDir::new().expect("tempdir");
    let segments_dir = xdg.path().join("linesmith").join("segments");
    fs::create_dir_all(&segments_dir).expect("mkdir");

    fs::write(
        segments_dir.join("echo.rhai"),
        r#"
        const ID = "echo";
        fn render(ctx) {
            #{ runs: [#{ text: ctx.config.text }] }
        }
        "#,
    )
    .expect("write plugin");

    let config_dir = xdg.path().join("linesmith");
    fs::write(
        config_dir.join("config.toml"),
        r#"
            [line]
            segments = ["echo"]
            [segments.echo]
            text = "hi-from-plugin"
        "#,
    )
    .expect("write config");

    let mut env = linesmith::CliEnv::for_tests();
    env.xdg_config_home = Some(xdg.path().as_os_str().to_owned());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = linesmith::cli_main(
        std::iter::empty::<&str>(),
        Cursor::new(CLAUDE_MINIMAL),
        &mut stdout,
        &mut stderr,
        &env,
    );
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert_eq!(String::from_utf8(stdout).expect("utf8"), "hi-from-plugin\n");
}

#[test]
fn git_branch_renders_unborn_head_via_full_driver_path() {
    // End-to-end through cli_main: a freshly init'd repo fixture has
    // no commits, so HEAD is unborn and the segment renders the
    // symbolic-ref target (whatever init.defaultBranch resolves to).
    use tempfile::TempDir;

    let repo_dir = TempDir::new().expect("tempdir");
    gix::init(repo_dir.path()).expect("gix::init");

    let mut env = linesmith::CliEnv::for_tests();
    env.cwd = Some(repo_dir.path().to_path_buf());

    // Scope the segment list to just `git_branch` so the assertion
    // doesn't couple to the rest of the default line.
    let xdg = TempDir::new().expect("tempdir");
    let config_dir = xdg.path().join("linesmith");
    std::fs::create_dir_all(&config_dir).expect("mkdir");
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
            [line]
            segments = ["git_branch"]
        "#,
    )
    .expect("write config");
    env.xdg_config_home = Some(xdg.path().as_os_str().to_owned());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = linesmith::cli_main(
        std::iter::empty::<&str>(),
        Cursor::new(CLAUDE_MINIMAL),
        &mut stdout,
        &mut stderr,
        &env,
    );
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    let rendered = String::from_utf8(stdout).expect("utf8");
    // gix's default branch is `main` unless init.defaultBranch is
    // configured; accept either.
    assert!(
        rendered.starts_with("main") || rendered.starts_with("master"),
        "expected branch name at start, got {rendered:?}"
    );
}

/// Set up a primary checkout + one linked worktree via `git worktree
/// add`, returning `(primary_tempdir, worktree_parent_tempdir,
/// worktree_path)`. Tempdirs are held by the caller so the worktree
/// stays live for the duration of the test.
fn linked_worktree_fixture(
    name: &str,
) -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
    use tempfile::TempDir;

    let primary = TempDir::new().expect("primary tempdir");
    let wt_parent = TempDir::new().expect("worktree tempdir");
    run_git(primary.path(), &["init", "--quiet"]);
    run_git(
        primary.path(),
        &["commit", "--allow-empty", "-m", "seed", "--quiet"],
    );
    let worktree_dir = wt_parent.path().join(name);
    run_git(
        primary.path(),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            name,
            worktree_dir.to_str().expect("utf8 path"),
        ],
    );
    (primary, wt_parent, worktree_dir)
}

fn cli_env_with_config(
    worktree_dir: std::path::PathBuf,
    config_toml: &str,
) -> (linesmith::CliEnv, tempfile::TempDir) {
    use tempfile::TempDir;

    let xdg = TempDir::new().expect("xdg tempdir");
    let config_dir = xdg.path().join("linesmith");
    std::fs::create_dir_all(&config_dir).expect("mkdir");
    std::fs::write(config_dir.join("config.toml"), config_toml).expect("write config");

    let mut env = linesmith::CliEnv::for_tests();
    env.cwd = Some(worktree_dir);
    env.xdg_config_home = Some(xdg.path().as_os_str().to_owned());
    (env, xdg)
}

#[test]
fn renders_worktree_hybrid_with_real_linked_worktree() {
    let (_primary, _wt_parent, worktree_dir) = linked_worktree_fixture("feat-segments");
    let (env, _xdg) = cli_env_with_config(
        worktree_dir,
        r#"
            [line]
            segments = ["workspace"]
        "#,
    );

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = linesmith::cli_main(
        std::iter::empty::<&str>(),
        Cursor::new(CLAUDE_MINIMAL),
        &mut stdout,
        &mut stderr,
        &env,
    );
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert_eq!(
        String::from_utf8(stdout).expect("utf8"),
        "linesmith/feat-segments\n"
    );
}

#[test]
fn workspace_and_git_branch_coexist_on_linked_worktree() {
    // Spec §Coordination: both segments render distinct payloads on a
    // linked worktree. `workspace` shows the worktree name, `git_branch`
    // shows the branch. No suppression.
    let (_primary, _wt_parent, worktree_dir) = linked_worktree_fixture("feat-segments");
    let (env, _xdg) = cli_env_with_config(
        worktree_dir,
        r#"
            [line]
            segments = ["workspace", "git_branch"]
        "#,
    );

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = linesmith::cli_main(
        std::iter::empty::<&str>(),
        Cursor::new(CLAUDE_MINIMAL),
        &mut stdout,
        &mut stderr,
        &env,
    );
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    let rendered = String::from_utf8(stdout).expect("utf8");
    assert!(
        rendered.contains("linesmith/feat-segments"),
        "workspace hybrid missing: {rendered:?}"
    );
    assert!(
        rendered.contains("feat-segments") && rendered.matches("feat-segments").count() >= 2,
        "git_branch should render the branch name alongside workspace: {rendered:?}"
    );
}

#[test]
fn git_branch_hides_outside_repo() {
    use tempfile::TempDir;

    let cwd = TempDir::new().expect("tempdir");

    let mut env = linesmith::CliEnv::for_tests();
    env.cwd = Some(cwd.path().to_path_buf());

    let xdg = TempDir::new().expect("tempdir");
    let config_dir = xdg.path().join("linesmith");
    std::fs::create_dir_all(&config_dir).expect("mkdir");
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
            [line]
            segments = ["git_branch", "model"]
        "#,
    )
    .expect("write config");
    env.xdg_config_home = Some(xdg.path().as_os_str().to_owned());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = linesmith::cli_main(
        std::iter::empty::<&str>(),
        Cursor::new(CLAUDE_MINIMAL),
        &mut stdout,
        &mut stderr,
        &env,
    );
    assert_eq!(code, 0);
    let rendered = String::from_utf8(stdout).expect("utf8");
    // Only the model segment rendered; git_branch hid silently.
    assert_eq!(rendered, "Claude Sonnet 4.6\n");
}

#[test]
fn config_reorders_and_filters_segments() {
    // Config picks only model + workspace, in that custom order.
    let cfg = linesmith_core::config::Config::from_str(
        r#"
            [line]
            segments = ["workspace", "model"]
        "#,
    )
    .expect("parse");
    let segments = linesmith_core::build_segments(Some(&cfg), None, |_| {});
    let mut out = Vec::new();
    linesmith_core::run_with_segments_and_width(
        Cursor::new(CLAUDE_WORKTREE),
        &mut out,
        &segments,
        200,
    )
    .expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");
    assert_eq!(rendered, "linesmith Claude Sonnet 4.6\n");
}

#[test]
fn config_style_override_emits_sgr_bytes_end_to_end() {
    // TOML → SegmentOverride → parse_style → with_user_style → render_with_warn
    // pipeline: the model segment's rendered text should be wrapped in a
    // TrueColor-red + bold SGR prefix followed by a reset.
    let cfg = linesmith_core::config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
            [segments.model]
            style = "fg:rgb(255, 0, 0) bold"
        "#,
    )
    .expect("parse");
    let segments = linesmith_core::build_segments(Some(&cfg), None, |_| {});
    let status_ctx = linesmith_core::input::parse(include_bytes!("fixtures/claude_minimal.json"))
        .expect("parse");
    let ctx = linesmith_core::data_context::DataContext::new(status_ctx);
    let line = linesmith_core::layout::render_with_warn(
        &segments,
        &ctx,
        200,
        &mut |_| {},
        linesmith_core::theme::default_theme(),
        linesmith_core::theme::Capability::TrueColor,
        false,
    );
    assert!(
        line.contains("\x1b[1;38;2;255;0;0m"),
        "expected bold + truecolor-red SGR prefix, got {line:?}"
    );
    assert!(line.contains("Claude Sonnet 4.6"));
    assert!(line.contains("\x1b[0m"), "expected SGR reset");
}

#[test]
fn config_style_override_invalid_warns_and_render_still_succeeds() {
    let cfg = linesmith_core::config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
            [segments.model]
            style = "role:mauve"
        "#,
    )
    .expect("parse");
    let mut warnings = Vec::new();
    let segments =
        linesmith_core::build_segments(Some(&cfg), None, |m| warnings.push(m.to_string()));
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("segments.model.style"));
    assert!(warnings[0].contains("mauve"));
    let mut out = Vec::new();
    linesmith_core::run_with_segments_and_width(
        Cursor::new(CLAUDE_MINIMAL),
        &mut out,
        &segments,
        200,
    )
    .expect("run ok");
    // Render still succeeds; the bad override is skipped.
    assert!(String::from_utf8(out)
        .expect("utf8")
        .contains("Claude Sonnet 4.6"));
}

#[test]
fn model_format_compact_strips_context_word_end_to_end() {
    // Default `format = "compact"` strips the trailing word "context"
    // from `(X context)` parentheticals. Pins the from_extras wiring
    // (segments/mod.rs::built_in_by_id → ModelSegment::from_extras)
    // through the full Config → build_segments → render path.
    let cfg = linesmith_core::config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
        "#,
    )
    .expect("parse");
    let segments = linesmith_core::build_segments(Some(&cfg), None, |_| {});
    let payload = br#"{
        "model": { "id": "claude-opus-4-7", "display_name": "Opus 4.7 (1M context)" },
        "session_id": "test-session",
        "cwd": "/home/dev/linesmith",
        "workspace": {
            "current_dir": ".",
            "project_dir": "/home/dev/linesmith",
            "added_dirs": [],
            "git_worktree": null
        }
    }"#;
    let mut out = Vec::new();
    linesmith_core::run_with_segments_and_width(
        Cursor::new(&payload[..]),
        &mut out,
        &segments,
        200,
    )
    .expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");
    assert!(rendered.contains("Opus 4.7 (1M)"), "got {rendered:?}");
    assert!(
        !rendered.contains("(1M context)"),
        "compact must drop the word: {rendered:?}"
    );
}

#[test]
fn model_format_full_preserves_anthropics_verbatim_string_end_to_end() {
    let cfg = linesmith_core::config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
            [segments.model]
            format = "full"
        "#,
    )
    .expect("parse");
    let segments = linesmith_core::build_segments(Some(&cfg), None, |_| {});
    let payload = br#"{
        "model": { "id": "claude-opus-4-7", "display_name": "Opus 4.7 (1M context)" },
        "session_id": "test-session",
        "cwd": "/home/dev/linesmith",
        "workspace": {
            "current_dir": ".",
            "project_dir": "/home/dev/linesmith",
            "added_dirs": [],
            "git_worktree": null
        }
    }"#;
    let mut out = Vec::new();
    linesmith_core::run_with_segments_and_width(
        Cursor::new(&payload[..]),
        &mut out,
        &segments,
        200,
    )
    .expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");
    assert!(
        rendered.contains("Opus 4.7 (1M context)"),
        "got {rendered:?}"
    );
}

#[test]
fn config_priority_override_flips_drop_order_under_pressure() {
    // With default priorities, a narrow terminal drops cost (192)
    // before model (64). Override model's priority to 250 and it drops
    // first instead.
    let cfg = linesmith_core::config::Config::from_str(
        r#"
            [line]
            segments = ["model", "cost"]
            [segments.model]
            priority = 250
        "#,
    )
    .expect("parse");
    let segments = linesmith_core::build_segments(Some(&cfg), None, |_| {});
    let mut out = Vec::new();
    // Budget tight enough to force one drop but fit the other.
    linesmith_core::run_with_segments_and_width(
        Cursor::new(CLAUDE_WORKTREE),
        &mut out,
        &segments,
        10,
    )
    .expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");
    // Model dropped; cost survived.
    assert!(!rendered.contains("Claude"));
    assert!(rendered.contains("$1.23"));
}
