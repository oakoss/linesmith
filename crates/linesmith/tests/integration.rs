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
fn renders_full_payload_with_rate_limits_cost_effort_and_worktree() {
    // Rate-limit countdowns depend on wall-clock `now`, so we match the
    // substrings that are stable rather than the full line.
    let mut out = Vec::new();
    linesmith::run(Cursor::new(CLAUDE_WORKTREE), &mut out).expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");

    for substring in [
        "Claude Sonnet 4.6",
        "42% · 200k",
        "5h 35%",
        "7d 12%",
        "$1.23",
        "high",
        "linesmith/feat-segments",
    ] {
        assert!(
            rendered.contains(substring),
            "expected {substring:?} in {rendered:?}"
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
fn config_reorders_and_filters_segments() {
    // Config picks only model + workspace, in that custom order.
    let cfg = linesmith::config::Config::from_str(
        r#"
            [line]
            segments = ["workspace", "model"]
        "#,
    )
    .expect("parse");
    let segments = linesmith::build_segments(Some(&cfg), |_| {});
    let mut out = Vec::new();
    linesmith::run_with_segments_and_width(Cursor::new(CLAUDE_WORKTREE), &mut out, &segments, 200)
        .expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");
    assert_eq!(rendered, "linesmith/feat-segments Claude Sonnet 4.6\n");
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
    let segments = linesmith::build_segments(Some(&cfg), |_| {});
    let mut out = Vec::new();
    // Budget tight enough to force one drop but fit the other.
    linesmith::run_with_segments_and_width(Cursor::new(CLAUDE_WORKTREE), &mut out, &segments, 10)
        .expect("run ok");
    let rendered = String::from_utf8(out).expect("utf8");
    // Model dropped; cost survived.
    assert!(!rendered.contains("Claude"));
    assert!(rendered.contains("$1.23"));
}
