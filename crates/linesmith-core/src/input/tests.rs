use super::*;

fn pct(v: f32) -> Percent {
    Percent::new(v).expect("in range")
}

#[test]
fn percent_new_rejects_out_of_range() {
    assert!(Percent::new(-0.1).is_none());
    assert!(Percent::new(100.1).is_none());
    assert!(Percent::new(f32::NAN).is_none());
}

#[test]
fn percent_from_f64_clamped_clamps_finite_values_and_rejects_nan() {
    assert_eq!(Percent::from_f64_clamped(150.0).unwrap().value(), 100.0);
    assert_eq!(Percent::from_f64_clamped(-5.0).unwrap().value(), 0.0);
    assert_eq!(Percent::from_f64_clamped(100.0).unwrap().value(), 100.0);
    assert_eq!(Percent::from_f64_clamped(0.0).unwrap().value(), 0.0);
    assert_eq!(Percent::from_f64_clamped(42.5).unwrap().value(), 42.5);
    // Tiny overshoot is the shape claude-code#37163 actually emits.
    // Strict `from_f64` rejects this because its range check runs
    // on the f64 before narrowing, so `100.0000001` is flagged
    // even though it would narrow to exactly `100.0` as f32. The
    // clamped variant accepts the out-of-range value and pins it
    // to 100.0.
    assert_eq!(
        Percent::from_f64_clamped(100.0000001).unwrap().value(),
        100.0
    );
    // NaN is the only input that still fails: `f64::clamp` treats
    // NaN as identity, which would poison downstream math. Reject
    // so the caller can surface InvalidValue.
    assert!(Percent::from_f64_clamped(f64::NAN).is_none());
    // Infinity clamps to the nearest bound under IEEE-754 ordering
    // — pinning explicitly in case a future stdlib change shifts
    // the semantics.
    assert_eq!(
        Percent::from_f64_clamped(f64::INFINITY).unwrap().value(),
        100.0
    );
    assert_eq!(
        Percent::from_f64_clamped(f64::NEG_INFINITY)
            .unwrap()
            .value(),
        0.0
    );
}

#[test]
fn percent_from_f64_rejects_values_that_would_narrow_into_range() {
    // 100.0000001 as f32 rounds to exactly 100.0. from_f64 validates
    // before narrowing so this is rejected rather than silently passing.
    assert!(Percent::from_f64(100.0000001).is_none());
    assert!(Percent::from_f64(-0.0000001).is_none());
    assert!(Percent::from_f64(f64::NAN).is_none());
    assert!(Percent::from_f64(100.0).is_some());
    assert!(Percent::from_f64(0.0).is_some());
}

#[test]
fn percent_complement_stays_in_range() {
    assert_eq!(pct(42.0).complement().value(), 58.0);
    assert_eq!(pct(0.0).complement().value(), 100.0);
    assert_eq!(pct(100.0).complement().value(), 0.0);
}

#[test]
fn parses_minimal_claude_payload() {
    let json = br#"{
        "model": { "id": "x", "display_name": "Claude Test" },
        "workspace": {
            "current_dir": ".",
            "project_dir": "/home/dev/linesmith",
            "added_dirs": [],
            "git_worktree": null
        }
    }"#;
    let ctx = parse(json).expect("parse ok");
    assert_eq!(ctx.tool, Tool::ClaudeCode);
    let model = ctx.model.expect("model");
    assert_eq!(model.display_name, "Claude Test");
    let workspace = ctx.workspace.expect("workspace");
    assert_eq!(workspace.project_dir.to_str(), Some("/home/dev/linesmith"));
    assert!(workspace.git_worktree.is_none());
    assert!(ctx.context_window.is_none());
}

#[test]
fn parses_payload_with_worktree() {
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": {
            "project_dir": "/repo",
            "git_worktree": { "name": "main", "path": "/wt/main" }
        }
    }"#;
    let ctx = parse(json).expect("parse ok");
    let wt = ctx
        .workspace
        .expect("workspace")
        .git_worktree
        .expect("worktree");
    assert_eq!(wt.name, "main");
    assert_eq!(wt.path, PathBuf::from("/wt/main"));
}

#[test]
fn git_worktree_absent_key_treated_as_none() {
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" }
    }"#;
    let ctx = parse(json).expect("parse ok");
    assert!(ctx.workspace.expect("workspace").git_worktree.is_none());
}

#[test]
fn parses_context_window() {
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 42.5,
            "remaining_percentage": 57.5,
            "context_window_size": 200000,
            "total_input_tokens": 12345,
            "total_output_tokens": 6789
        }
    }"#;
    let ctx = parse(json).expect("parse ok");
    let cw = ctx.context_window.expect("context_window");
    assert_eq!(cw.used.expect("used").value(), 42.5);
    assert_eq!(cw.remaining().expect("remaining").value(), 57.5);
    assert_eq!(cw.size, Some(200_000));
    assert_eq!(cw.total_input_tokens, Some(12_345));
    assert_eq!(cw.total_output_tokens, Some(6_789));
}

#[test]
fn used_percentage_above_100_clamps_instead_of_rejecting() {
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 150,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0
        }
    }"#;
    let ctx = parse(json).expect("clamp succeeds");
    let cw = ctx.context_window.expect("context_window present");
    assert_eq!(cw.used.expect("used").value(), 100.0);
}

#[test]
fn used_percentage_fractional_overshoot_clamps_to_100() {
    // Catch a regression that routes the raw f64 through `as i64`
    // or `.floor()` before clamping.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 101.7,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0
        }
    }"#;
    let ctx = parse(json).expect("clamp succeeds");
    let cw = ctx.context_window.expect("context_window present");
    assert_eq!(cw.used.expect("used").value(), 100.0);
}

#[test]
fn used_percentage_below_0_rejects_as_invalid_value() {
    // Negative percentages aren't a documented Claude Code state —
    // treat them as a corrupted payload and surface InvalidValue
    // so the failure is loud, instead of silently clamping to 0%.
    // The above-100 case is different (known upstream bug in
    // claude-code#37163) and clamps via the companion test.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": -5.0,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0
        }
    }"#;
    match parse(json).expect_err("should reject") {
        ParseError::InvalidValue { path, .. } => {
            assert_eq!(path, "context_window.used_percentage");
        }
        other => panic!("expected InvalidValue, got {other:?}"),
    }
}

#[test]
fn used_percentage_in_range_passes_through_unchanged() {
    // Clamp must not distort values that were already in range.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 42.5,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0
        }
    }"#;
    let ctx = parse(json).expect("in-range succeeds");
    let cw = ctx.context_window.expect("context_window present");
    assert_eq!(cw.used.expect("used").value(), 42.5);
}

#[test]
fn missing_used_percentage_degrades_leaf_to_none() {
    // ADR-0014: missing leaf no longer aborts the parse; only the
    // affected leaf goes None and peers like `size` survive.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0
        }
    }"#;
    let ctx = parse(json).expect("missing leaf must not fail the whole parse");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.used.is_none());
    assert_eq!(cw.size, Some(200_000));
}

#[test]
fn wrong_type_used_percentage_degrades_leaf_to_none() {
    // ADR-0014: type drift on a leaf no longer aborts the parse.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": "42",
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0
        }
    }"#;
    let ctx = parse(json).expect("type-drift leaf must not fail the whole parse");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.used.is_none());
    assert_eq!(cw.size, Some(200_000));
}

#[test]
fn pre_first_api_call_payload_renders_other_segments() {
    // Real captured CC 2.1.119 payload from the ~15s
    // pre-first-API-call window where `used_percentage` is null.
    // Asserts structurally (not value-pinned) so re-capturing the
    // fixture from a different account/session doesn't break the
    // test for reasons unrelated to the contract under check.
    let bytes = include_bytes!("../../tests/fixtures/claude_pre_first_api_call.json");
    let ctx = parse(bytes).expect("parse must succeed despite null context_window leaves");
    assert!(
        !ctx.model.expect("model").display_name.is_empty(),
        "model must parse"
    );
    assert!(
        !ctx.workspace
            .expect("workspace")
            .project_dir
            .as_os_str()
            .is_empty(),
        "workspace must parse"
    );
    // Per ADR-0014, leaves degrade individually: `used` is null
    // pre-first-call but `size` and `total_*_tokens` survive.
    // Segments with a hard dependency on `used` (the textual
    // `context_window` segment) hide; segments that only need
    // peer leaves keep rendering.
    let cw = ctx
        .context_window
        .expect("context_window present with partial leaves");
    assert!(cw.used.is_none(), "used_percentage was null in payload");
    assert!(cw.size.is_some(), "size populated in payload");
    assert!(ctx.cost.is_some(), "cost segment must still render");
    assert_eq!(ctx.effort, Some(EffortLevel::XHigh));
}

#[test]
fn null_used_percentage_degrades_leaf_only() {
    // ADR-0014: peers (`size`, `total_*_tokens`) survive a null
    // `used_percentage`, including the documented pre-first-API-
    // call shape. The textual segment hides; tokens / current_usage
    // peers remain consumable.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": null,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0
        }
    }"#;
    let ctx = parse(json).expect("null used_percentage must not fail the whole parse");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.used.is_none());
    assert_eq!(cw.size, Some(200_000));
    assert_eq!(cw.total_input_tokens, Some(0));
}

#[test]
fn null_context_window_size_degrades_leaf_only() {
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 12.5,
            "context_window_size": null,
            "total_input_tokens": 0,
            "total_output_tokens": 0
        }
    }"#;
    let ctx = parse(json).expect("null size must not fail the whole parse");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.size.is_none());
    assert!(cw.used.is_some(), "used survives even when size is null");
}

#[test]
fn null_total_input_tokens_degrades_leaf_only() {
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 12.5,
            "context_window_size": 200000,
            "total_input_tokens": null,
            "total_output_tokens": 0
        }
    }"#;
    let ctx = parse(json).expect("null total_input_tokens must not fail the whole parse");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.total_input_tokens.is_none());
    assert_eq!(
        cw.total_output_tokens,
        Some(0),
        "peer leaves survive isolated null"
    );
}

#[test]
fn null_total_output_tokens_degrades_leaf_only() {
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 12.5,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": null
        }
    }"#;
    let ctx = parse(json).expect("null total_output_tokens must not fail the whole parse");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.total_output_tokens.is_none());
    assert_eq!(cw.total_input_tokens, Some(0));
}

#[test]
fn current_usage_survives_when_peer_leaf_is_null() {
    // ADR-0014: per-leaf isolation — current_usage must not be
    // dropped just because a sibling totals counter is null.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 50.0,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": null,
            "current_usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            }
        }
    }"#;
    let ctx = parse(json).expect("partial null must not drop current_usage");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.total_output_tokens.is_none());
    let usage = cw.current_usage.expect("current_usage preserved");
    assert_eq!(usage.input_tokens, 100);
}

#[test]
fn context_window_size_above_u32_max_degrades_leaf_only() {
    // ADR-0014 narrows `size` to `u32`. A future hypothetical
    // CC payload exceeding u32::MAX must degrade the leaf to
    // None (not wrap via `as u32`) and let peers survive.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 12.5,
            "context_window_size": 4294967296,
            "total_input_tokens": 0,
            "total_output_tokens": 0
        }
    }"#;
    let ctx = parse(json).expect("u32 overflow must not fail the whole parse");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.size.is_none(), "size leaf degraded on overflow");
    assert!(cw.used.is_some(), "peer leaf survives");
}

#[test]
fn context_window_explicit_null_treated_as_none() {
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": null
    }"#;
    let ctx = parse(json).expect("parse ok");
    assert!(ctx.context_window.is_none());
}

#[test]
fn current_usage_absent_is_none() {
    // Schema doesn't guarantee the key is present inside a
    // context_window object; treat missing the same as explicit
    // `null` so a future schema variation parses cleanly.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 42.5,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0
        }
    }"#;
    let ctx = parse(json).expect("parse ok");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.current_usage.is_none());
}

#[test]
fn current_usage_null_is_none() {
    // Claude Code emits `current_usage: null` before the first API
    // call in a session; round-trip to Option::None.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 0,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "current_usage": null
        }
    }"#;
    let ctx = parse(json).expect("parse ok");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.current_usage.is_none());
}

#[test]
fn current_usage_present_parses_all_four_fields() {
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 12.4,
            "context_window_size": 200000,
            "total_input_tokens": 24800,
            "total_output_tokens": 3200,
            "current_usage": {
                "input_tokens": 2000,
                "output_tokens": 500,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 500
            }
        }
    }"#;
    let ctx = parse(json).expect("parse ok");
    let cw = ctx.context_window.expect("context_window present");
    let usage = cw.current_usage.expect("current_usage present");
    assert_eq!(usage.input_tokens, 2000);
    assert_eq!(usage.output_tokens, 500);
    assert_eq!(usage.cache_creation_input_tokens, 0);
    assert_eq!(usage.cache_read_input_tokens, 500);
}

#[test]
fn current_usage_non_object_degrades_to_none() {
    // ADR-0014: a non-object current_usage no longer aborts the
    // whole parse; the leaf goes None and peers survive.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 0,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "current_usage": "not an object"
        }
    }"#;
    let ctx = parse(json).expect("non-object current_usage must not fail the whole parse");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.current_usage.is_none());
    assert_eq!(cw.size, Some(200_000));
}

#[test]
fn current_usage_missing_inner_field_degrades_to_none() {
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 0,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "current_usage": {
                "input_tokens": 100,
                "output_tokens": 50
            }
        }
    }"#;
    let ctx = parse(json).expect("partial current_usage must not fail the whole parse");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.current_usage.is_none());
}

#[test]
fn current_usage_inner_wrong_type_degrades_to_none() {
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 0,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "current_usage": {
                "input_tokens": "200",
                "output_tokens": 50,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            }
        }
    }"#;
    let ctx = parse(json).expect("type-drift inner field must not fail the whole parse");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.current_usage.is_none());
}

#[test]
fn current_usage_inner_null_collapses_whole_turn_usage() {
    // Pin TurnUsage's all-or-nothing semantics: a single null
    // leaf collapses the whole `current_usage` to None (per
    // parse_current_usage's let-else cascade), but peers
    // outside `current_usage` (size, used) survive.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 50.0,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "current_usage": {
                "input_tokens": null,
                "output_tokens": 50,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            }
        }
    }"#;
    let ctx = parse(json).expect("partial null inside current_usage must not fail parse");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.current_usage.is_none());
    assert_eq!(cw.size, Some(200_000));
    assert!(cw.used.is_some());
}

#[test]
fn current_usage_inner_missing_collapses_whole_turn_usage() {
    // Symmetric to the null-leaf test, but pinned at the call
    // site (parse_current_usage's let-else cascade), not at the
    // helper. `try_u64_optional` collapses missing and null into
    // the same silent `None` branch today; if a future refactor
    // splits them (e.g. logging a warn on missing), this test
    // catches the call-site behavior change without forcing the
    // helper's internal control flow into the test surface.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/repo" },
        "context_window": {
            "used_percentage": 50.0,
            "context_window_size": 200000,
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "current_usage": {
                "output_tokens": 50,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            }
        }
    }"#;
    let ctx = parse(json).expect("missing leaf inside current_usage must not fail parse");
    let cw = ctx.context_window.expect("context_window present");
    assert!(cw.current_usage.is_none());
    assert_eq!(cw.size, Some(200_000));
}

#[test]
fn cost_total_cost_usd_accepts_zero_and_tiny_positive() {
    // Defends try_f64_required's success path against a future
    // over-tightening like `n != 0.0` or `n > 0.0`. Zero is valid
    // (no API calls yet); a tiny non-zero positive must also
    // round-trip. serde_json rejects literals at f64::MAX as
    // "number out of range" — see `out_of_range_number_rejected_
    // at_json_layer` for the upper bound.
    for &val in &[0.0_f64, 1e-300_f64] {
        let bytes = format!(
            r#"{{"model":{{"display_name":"X"}},"workspace":{{"project_dir":"/r"}},
                 "cost":{{"total_cost_usd":{val},"total_duration_ms":0,"total_api_duration_ms":0,
                          "total_lines_added":0,"total_lines_removed":0}}}}"#
        );
        let ctx = parse(bytes.as_bytes()).expect("finite f64 must round-trip");
        let cost = ctx.cost.expect("cost present");
        assert_eq!(cost.total_cost_usd, Some(val));
    }
}

#[test]
fn wrong_type_git_worktree_degrades_to_none() {
    // ADR-0014: malformed git_worktree no longer aborts the whole
    // parse; the worktree goes None and the project_dir survives.
    let json = br#"{
        "model": { "display_name": "X" },
        "workspace": {
            "project_dir": "/repo",
            "git_worktree": "main"
        }
    }"#;
    let ctx = parse(json).expect("malformed worktree must not fail the whole parse");
    let workspace = ctx.workspace.expect("workspace present");
    assert!(workspace.git_worktree.is_none());
    assert_eq!(workspace.project_dir.to_str(), Some("/repo"));
}

#[test]
fn missing_model_degrades_to_none() {
    // ADR-0014: a payload without `model` no longer aborts the
    // whole parse; the field goes None and segments hide.
    let json = br#"{
        "workspace": { "project_dir": "/repo" }
    }"#;
    let ctx = parse(json).expect("missing model must not fail the whole parse");
    assert!(ctx.model.is_none());
    assert!(ctx.workspace.is_some());
}

#[test]
fn rejects_malformed_json() {
    assert!(matches!(
        parse(b"{not json"),
        Err(ParseError::InvalidJson { .. })
    ));
}

#[test]
fn empty_object_payload_returns_all_none_top_level() {
    // ADR-0014 confirmation: parse(b"{}") must succeed with every
    // top-level Option field None, only `tool` and `raw` populated.
    // This is the post-isolation contract — no field is required
    // for parse to succeed on a syntactically valid object root.
    let ctx = parse(b"{}").expect("empty object must parse");
    assert_eq!(ctx.tool, Tool::ClaudeCode);
    assert!(ctx.model.is_none());
    assert!(ctx.workspace.is_none());
    assert!(ctx.context_window.is_none());
    assert!(ctx.cost.is_none());
    assert_eq!(ctx.effort, None);
    assert_eq!(ctx.vim, None);
    assert!(ctx.output_style.is_none());
    assert!(ctx.agent_name.is_none());
    assert!(ctx.version.is_none());
    // raw round-trips the empty object so plugins can still query
    // ctx.status.raw paths without a None guard.
    assert!(ctx.raw.is_object());
}

#[test]
fn malformed_json_carries_exact_source_position() {
    // The `}` at line 2, column 10 is where serde_json bails.
    let ParseError::InvalidJson { location, .. } = parse(b"{\n  \"bad\": }").unwrap_err() else {
        panic!("expected InvalidJson");
    };
    let pos = location.expect("position populated for positional errors");
    assert_eq!(pos.line, 2);
    assert_eq!(pos.column, 10);
}

#[test]
fn json_type_of_maps_each_variant() {
    use serde_json::Value;
    assert_eq!(
        JsonType::of(&Value::Object(Default::default())),
        JsonType::Object
    );
    assert_eq!(JsonType::of(&Value::Array(vec![])), JsonType::Array);
    assert_eq!(JsonType::of(&Value::String("x".into())), JsonType::String);
    assert_eq!(JsonType::of(&Value::from(42)), JsonType::Number);
    assert_eq!(JsonType::of(&Value::Bool(true)), JsonType::Bool);
    assert_eq!(JsonType::of(&Value::Null), JsonType::Null);
}

#[test]
fn parse_error_display_formats_root_path_readably() {
    // When the root JSON is not an object, the path is empty.
    let err = parse(b"[]").expect_err("array at root rejected");
    let display = err.to_string();
    assert!(display.contains("<root>"), "got {display:?}");
}

// --- cost error paths ---

#[test]
fn cost_absent_treated_as_none() {
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"}}"#;
    assert!(parse(bytes).expect("ok").cost.is_none());
}

#[test]
fn cost_explicit_null_treated_as_none() {
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"cost":null}"#;
    assert!(parse(bytes).expect("ok").cost.is_none());
}

#[test]
fn cost_wrong_type_degrades_to_none() {
    // ADR-0014: a non-object cost wrapper no longer aborts the
    // whole parse; the field goes None and peers survive.
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"cost":"nope"}"#;
    let ctx = parse(bytes).expect("non-object cost must not fail the whole parse");
    assert!(ctx.cost.is_none());
    assert!(ctx.model.is_some());
}

#[test]
fn cost_missing_sub_field_degrades_leaf_only() {
    // ADR-0014: missing cost leaf no longer aborts the parse;
    // the leaf goes None, peers populate from the payload.
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},
        "cost":{"total_cost_usd":1.0,"total_duration_ms":0,"total_api_duration_ms":0,"total_lines_added":0}}"#;
    let ctx = parse(bytes).expect("missing leaf must not fail the whole parse");
    let cost = ctx.cost.expect("cost present");
    assert!(cost.total_lines_removed.is_none());
    assert_eq!(cost.total_cost_usd, Some(1.0));
    assert_eq!(cost.total_lines_added, Some(0));
}

#[test]
fn cost_total_cost_usd_non_numeric_degrades_leaf_only() {
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},
        "cost":{"total_cost_usd":"oops","total_duration_ms":0,"total_api_duration_ms":0,
                 "total_lines_added":0,"total_lines_removed":0}}"#;
    let ctx = parse(bytes).expect("type-drift leaf must not fail the whole parse");
    let cost = ctx.cost.expect("cost present");
    assert!(cost.total_cost_usd.is_none());
    assert_eq!(cost.total_duration_ms, Some(0));
}

#[test]
fn cost_wrapper_with_all_leaves_drift_collapses_to_none() {
    // Plugin contract regression guard: ctx.status.cost != () must
    // always imply at least one readable leaf. A wrapper whose
    // every leaf is malformed collapses to None rather than
    // surfacing an all-`()` cost map to rhai consumers.
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},
        "cost":{"total_cost_usd":"a","total_duration_ms":"b","total_api_duration_ms":"c",
                 "total_lines_added":"d","total_lines_removed":"e"}}"#;
    let ctx = parse(bytes).expect("all-leaves-drift cost must not fail the whole parse");
    assert!(ctx.cost.is_none());
}

#[test]
fn context_window_wrapper_with_all_leaves_drift_collapses_to_none() {
    // Same plugin contract for context_window.
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},
        "context_window":{"used_percentage":"a","context_window_size":"b",
                           "total_input_tokens":"c","total_output_tokens":"d"}}"#;
    let ctx = parse(bytes).expect("all-leaves-drift context_window must not fail the parse");
    assert!(ctx.context_window.is_none());
}

#[test]
fn out_of_range_number_rejected_at_json_layer() {
    // Pins the JSON-syntax barrier: `1e500` overflows f64 and
    // serde_json rejects it as InvalidJson before our normalizer
    // sees it. Documents why try_f64_required has no `is_finite()`
    // guard — that branch would be unreachable through parse().
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},
        "cost":{"total_cost_usd":1e500,"total_duration_ms":0,"total_api_duration_ms":0,
                 "total_lines_added":0,"total_lines_removed":0}}"#;
    match parse(bytes).expect_err("serde_json rejects out-of-range numbers") {
        ParseError::InvalidJson { .. } => {}
        other => panic!("expected InvalidJson, got {other:?}"),
    }
}

#[test]
fn cost_lines_added_accepts_large_value_without_truncation() {
    // Regression guard for slice-3 review fix: fields were previously
    // narrowed to u32, silently truncating at 4.29B.
    let bytes = format!(
        r#"{{"model":{{"display_name":"X"}},"workspace":{{"project_dir":"/r"}},
           "cost":{{"total_cost_usd":0.0,"total_duration_ms":0,"total_api_duration_ms":0,
                    "total_lines_added":{n},"total_lines_removed":0}}}}"#,
        n = 5_000_000_000u64
    );
    let ctx = parse(bytes.as_bytes()).expect("parse ok");
    assert_eq!(
        ctx.cost.expect("cost").total_lines_added,
        Some(5_000_000_000u64)
    );
}

// --- effort error paths ---

#[test]
fn effort_object_form_parses() {
    // Canonical shape as of Claude Code 2.1.x.
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{"level":"xhigh"}}"#;
    let ctx = parse(bytes).expect("parse ok");
    assert_eq!(ctx.effort, Some(EffortLevel::XHigh));
}

#[test]
fn effort_bare_string_still_parses() {
    // Back-compat for tools that emit a bare-string form.
    let bytes =
        br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":"high"}"#;
    let ctx = parse(bytes).expect("parse ok");
    assert_eq!(ctx.effort, Some(EffortLevel::High));
}

#[test]
fn effort_object_missing_level_degrades_to_none() {
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{}}"#;
    let ctx = parse(bytes).expect("missing effort.level must not fail the whole parse");
    assert_eq!(ctx.effort, None);
    assert!(ctx.model.is_some());
}

#[test]
fn effort_object_non_string_level_degrades_to_none() {
    let bytes =
        br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{"level":42}}"#;
    let ctx = parse(bytes).expect("type-drift effort.level must not fail the whole parse");
    assert_eq!(ctx.effort, None);
}

#[test]
fn effort_object_null_level_maps_to_none() {
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{"level":null}}"#;
    let ctx = parse(bytes).expect("parse ok");
    assert_eq!(ctx.effort, None);
}

#[test]
fn effort_top_level_null_maps_to_none() {
    // Locks the outer early-return in parse_effort. A refactor that
    // dropped the explicit is_null() check and delegated to the
    // object/string match would regress this into a TypeMismatch.
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":null}"#;
    let ctx = parse(bytes).expect("parse ok");
    assert_eq!(ctx.effort, None);
}

#[test]
fn effort_non_object_non_string_degrades_to_none() {
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":42}"#;
    let ctx = parse(bytes).expect("non-object/non-string effort must not fail the whole parse");
    assert_eq!(ctx.effort, None);
}

#[test]
fn effort_object_unknown_level_degrades_to_none() {
    // ADR-0014: unknown effort variant warns + degrades, symmetric
    // with parse_vim. A future CC level shouldn't blank the line.
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{"level":"ultra"}}"#;
    let ctx = parse(bytes).expect("unknown effort variant must not fail the whole parse");
    assert_eq!(ctx.effort, None);
}

#[test]
fn effort_unknown_string_degrades_to_none() {
    let bytes =
        br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":"ultra"}"#;
    let ctx = parse(bytes).expect("unknown string effort must not fail the whole parse");
    assert_eq!(ctx.effort, None);
}

// --- vim / output_style / agent ---

#[test]
fn parses_vim_object_form() {
    let bytes = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/r" },
        "vim": { "mode": "insert" }
    }"#;
    let ctx = parse(bytes).expect("ok");
    assert_eq!(ctx.vim, Some(VimMode::Insert));
}

#[test]
fn parses_vim_string_form_for_compat() {
    let bytes = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/r" },
        "vim": "visual"
    }"#;
    let ctx = parse(bytes).expect("ok");
    assert_eq!(ctx.vim, Some(VimMode::Visual));
}

#[test]
fn vim_absent_or_null_yields_none() {
    let absent = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"}}"#;
    assert_eq!(parse(absent).unwrap().vim, None);
    let null = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":null}"#;
    assert_eq!(parse(null).unwrap().vim, None);
    let null_mode =
        br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":{"mode":null}}"#;
    assert_eq!(parse(null_mode).unwrap().vim, None);
}

#[test]
fn vim_unknown_mode_degrades_segment_not_whole_parse() {
    // An unknown vim mode (e.g. a future CC `select` or `terminal`)
    // must NOT abort the whole parse — the rest of the statusline
    // would render blank for an opt-in informational segment. Warn
    // and degrade to None instead. Lock the contract so a refactor
    // that re-introduces `InvalidValue` here regresses loudly.
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":{"mode":"surrogate"}}"#;
    let ctx = parse(bytes).expect("unknown vim mode must not fail parse");
    assert_eq!(ctx.vim, None);
}

#[test]
fn parses_output_style() {
    let bytes = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/r" },
        "output_style": { "name": "concise" }
    }"#;
    let ctx = parse(bytes).expect("ok");
    let style = ctx.output_style.expect("present");
    assert_eq!(style.name, "concise");
}

#[test]
fn output_style_absent_or_null_yields_none() {
    let absent = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"}}"#;
    assert!(parse(absent).unwrap().output_style.is_none());
    let null =
        br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":null}"#;
    assert!(parse(null).unwrap().output_style.is_none());
    let null_name = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":{"name":null}}"#;
    assert!(parse(null_name).unwrap().output_style.is_none());
    // Object without `name` is tolerated as None so the schema can grow.
    let no_name =
        br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":{}}"#;
    assert!(parse(no_name).unwrap().output_style.is_none());
    let empty = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":{"name":""}}"#;
    assert!(parse(empty).unwrap().output_style.is_none());
}

#[test]
fn output_style_name_typed_wrong_degrades_to_none() {
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":{"name":42}}"#;
    let ctx = parse(bytes).expect("type-drift output_style.name must not fail the whole parse");
    assert_eq!(ctx.output_style, None);
}

#[test]
fn parses_agent_name() {
    let bytes = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/r" },
        "agent": { "name": "research" }
    }"#;
    let ctx = parse(bytes).expect("ok");
    assert_eq!(ctx.agent_name.as_deref(), Some("research"));
}

#[test]
fn agent_absent_null_or_empty_yields_none() {
    let absent = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"}}"#;
    assert!(parse(absent).unwrap().agent_name.is_none());
    let null = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"agent":null}"#;
    assert!(parse(null).unwrap().agent_name.is_none());
    let empty =
        br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"agent":{"name":""}}"#;
    assert!(parse(empty).unwrap().agent_name.is_none());
    let no_name = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"agent":{}}"#;
    assert!(parse(no_name).unwrap().agent_name.is_none());
}

#[test]
fn vim_object_missing_mode_degrades_to_none() {
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":{}}"#;
    let ctx = parse(bytes).expect("missing vim.mode must not fail the whole parse");
    assert_eq!(ctx.vim, None);
}

#[test]
fn vim_object_non_string_mode_degrades_to_none() {
    let bytes =
        br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":{"mode":42}}"#;
    let ctx = parse(bytes).expect("type-drift vim.mode must not fail the whole parse");
    assert_eq!(ctx.vim, None);
}

#[test]
fn vim_non_object_non_string_degrades_to_none() {
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":42}"#;
    let ctx = parse(bytes).expect("non-object/non-string vim must not fail the whole parse");
    assert_eq!(ctx.vim, None);
}

#[test]
fn output_style_non_object_degrades_to_none() {
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":"concise"}"#;
    let ctx = parse(bytes).expect("non-object output_style must not fail the whole parse");
    assert_eq!(ctx.output_style, None);
}

#[test]
fn agent_non_object_degrades_to_none() {
    let bytes =
        br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"agent":"research"}"#;
    let ctx = parse(bytes).expect("non-object agent must not fail the whole parse");
    assert!(ctx.agent_name.is_none());
}

#[test]
fn agent_name_typed_wrong_degrades_to_none() {
    let bytes =
        br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"agent":{"name":42}}"#;
    let ctx = parse(bytes).expect("type-drift agent.name must not fail the whole parse");
    assert!(ctx.agent_name.is_none());
}

#[test]
fn parses_top_level_version_string() {
    // Pinned against the official CC docs at code.claude.com/docs/en/statusline
    // (verified 2026-04-28): `version` is a top-level string field
    // on the statusline payload.
    let bytes = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/r" },
        "version": "2.1.90"
    }"#;
    assert_eq!(parse(bytes).unwrap().version.as_deref(), Some("2.1.90"));
}

#[test]
fn version_absent_or_null_or_empty_yields_none() {
    let absent = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"}}"#;
    assert!(parse(absent).unwrap().version.is_none());
    let null = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"version":null}"#;
    assert!(parse(null).unwrap().version.is_none());
    let empty = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"version":""}"#;
    assert!(parse(empty).unwrap().version.is_none());
    // Whitespace-only is treated like empty: rendering "v   " is
    // worse than hiding the segment.
    let ws = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"version":"   "}"#;
    assert!(parse(ws).unwrap().version.is_none());
}

#[test]
fn version_surrounding_whitespace_is_trimmed() {
    // Defensive: if CC ever ships padded version strings, the
    // segment shouldn't render `v  2.1.90  `.
    let bytes = br#"{
        "model": { "display_name": "X" },
        "workspace": { "project_dir": "/r" },
        "version": "  2.1.90  "
    }"#;
    assert_eq!(parse(bytes).unwrap().version.as_deref(), Some("2.1.90"));
}

#[test]
fn version_typed_wrong_degrades_to_none() {
    let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"version":42}"#;
    let ctx = parse(bytes).expect("type-drift version must not fail the whole parse");
    assert!(ctx.version.is_none());
}
