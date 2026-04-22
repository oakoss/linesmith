use std::io::Cursor;
use std::str::FromStr;

const CLAUDE_MINIMAL: &str = include_str!("fixtures/claude_minimal.json");
const CLAUDE_WORKTREE: &str = include_str!("fixtures/claude_worktree.json");

#[test]
fn renders_model_and_workspace_when_outside_worktree() {
    let mut out = Vec::new();
    linesmith::run(Cursor::new(CLAUDE_MINIMAL), &mut out).expect("run ok");
    assert_eq!(
        String::from_utf8(out).expect("utf8"),
        "Claude Sonnet 4.6 linesmith\n"
    );
}

#[test]
fn renders_full_payload_with_cost_effort_and_worktree() {
    // The stdin `rate_limits` field is no longer consumed; rate-limit
    // segments are opt-in so a first-run user doesn't trigger a
    // Keychain prompt from the default line.
    let mut out = Vec::new();
    linesmith::run(Cursor::new(CLAUDE_WORKTREE), &mut out).expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");

    for substring in [
        "Claude Sonnet 4.6",
        "42% · 200k",
        "$1.23",
        "high",
        "linesmith/feat-segments",
    ] {
        assert!(
            rendered.contains(substring),
            "expected {substring:?} in {rendered:?}"
        );
    }
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
    linesmith::run(Cursor::new(b"{not json"), &mut out).expect("run should not error");
    assert_eq!(String::from_utf8(out).expect("utf8"), "?\n");
}

#[test]
fn narrow_terminal_drops_cost_and_effort_first() {
    // Full line is ~95 cells. 50 cells must drop cost and effort (highest
    // priorities) before it touches context_window or workspace.
    let mut out = Vec::new();
    linesmith::run_with_width(Cursor::new(CLAUDE_WORKTREE), &mut out, 50).expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");
    assert!(!rendered.contains("$1.23"), "cost should drop at 50 cells");
    assert!(!rendered.contains("high"), "effort should drop at 50 cells");
    assert!(rendered.contains("42% · 200k"));
    assert!(rendered.contains("linesmith/feat-segments"));
}

#[test]
fn extreme_narrow_keeps_only_lowest_priority_segments() {
    // 30 cells: everything above workspace's priority-16 must drop; the
    // workspace segment ("linesmith/feat-segments") is 23 cells and fits.
    let mut out = Vec::new();
    linesmith::run_with_width(Cursor::new(CLAUDE_WORKTREE), &mut out, 30).expect("run ok");
    assert_eq!(
        String::from_utf8(out).expect("utf8"),
        "linesmith/feat-segments\n"
    );
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
    env.xdg_config_home = Some(xdg.path().to_string_lossy().into_owned());

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
fn config_reorders_and_filters_segments() {
    // Config picks only model + workspace, in that custom order.
    let cfg = linesmith::config::Config::from_str(
        r#"
            [line]
            segments = ["workspace", "model"]
        "#,
    )
    .expect("parse");
    let segments = linesmith::build_segments(Some(&cfg), None, |_| {});
    let mut out = Vec::new();
    linesmith::run_with_segments_and_width(Cursor::new(CLAUDE_WORKTREE), &mut out, &segments, 200)
        .expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");
    assert_eq!(rendered, "linesmith/feat-segments Claude Sonnet 4.6\n");
}

#[test]
fn config_style_override_emits_sgr_bytes_end_to_end() {
    // TOML → SegmentOverride → parse_style → with_user_style → render_with_warn
    // pipeline: the model segment's rendered text should be wrapped in a
    // TrueColor-red + bold SGR prefix followed by a reset.
    let cfg = linesmith::config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
            [segments.model]
            style = "fg:rgb(255, 0, 0) bold"
        "#,
    )
    .expect("parse");
    let segments = linesmith::build_segments(Some(&cfg), None, |_| {});
    let status_ctx =
        linesmith::input::parse(include_bytes!("fixtures/claude_minimal.json")).expect("parse");
    let ctx = linesmith::data_context::DataContext::new(status_ctx);
    let line = linesmith::layout::render_with_warn(
        &segments,
        &ctx,
        200,
        &mut |_| {},
        linesmith::theme::default_theme(),
        linesmith::theme::Capability::TrueColor,
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
    let cfg = linesmith::config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
            [segments.model]
            style = "role:mauve"
        "#,
    )
    .expect("parse");
    let mut warnings = Vec::new();
    let segments = linesmith::build_segments(Some(&cfg), None, |m| warnings.push(m.to_string()));
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("segments.model.style"));
    assert!(warnings[0].contains("mauve"));
    let mut out = Vec::new();
    linesmith::run_with_segments_and_width(Cursor::new(CLAUDE_MINIMAL), &mut out, &segments, 200)
        .expect("run ok");
    // Render still succeeds; the bad override is skipped.
    assert!(String::from_utf8(out)
        .expect("utf8")
        .contains("Claude Sonnet 4.6"));
}

#[test]
fn config_priority_override_flips_drop_order_under_pressure() {
    // With default priorities, a narrow terminal drops cost (192)
    // before model (64). Override model's priority to 250 and it drops
    // first instead.
    let cfg = linesmith::config::Config::from_str(
        r#"
            [line]
            segments = ["model", "cost"]
            [segments.model]
            priority = 250
        "#,
    )
    .expect("parse");
    let segments = linesmith::build_segments(Some(&cfg), None, |_| {});
    let mut out = Vec::new();
    // Budget tight enough to force one drop but fit the other.
    linesmith::run_with_segments_and_width(Cursor::new(CLAUDE_WORKTREE), &mut out, &segments, 10)
        .expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");
    // Model dropped; cost survived.
    assert!(!rendered.contains("Claude"));
    assert!(rendered.contains("$1.23"));
}
