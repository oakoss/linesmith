use std::io::Cursor;

const CLAUDE_MINIMAL: &str = include_str!("fixtures/claude_minimal.json");
const CLAUDE_WORKTREE: &str = include_str!("fixtures/claude_worktree.json");

#[test]
fn renders_workspace_name_when_outside_worktree() {
    let mut out = Vec::new();
    linesmith::run(Cursor::new(CLAUDE_MINIMAL), &mut out).expect("run ok");
    assert_eq!(String::from_utf8(out).expect("utf8"), "linesmith\n");
}

#[test]
fn renders_repo_worktree_hybrid_when_inside_worktree() {
    let mut out = Vec::new();
    linesmith::run(Cursor::new(CLAUDE_WORKTREE), &mut out).expect("run ok");
    assert_eq!(
        String::from_utf8(out).expect("utf8"),
        "linesmith/feat-segments\n"
    );
}

#[test]
fn malformed_json_exits_zero_with_marker_line() {
    let mut out = Vec::new();
    linesmith::run(Cursor::new(b"{not json"), &mut out).expect("run should not error");
    assert_eq!(String::from_utf8(out).expect("utf8"), "?\n");
}
