use super::*;
use chrono::TimeZone;
use tempfile::TempDir;

fn env_from(claude: Option<&Path>, xdg: Option<&Path>, home: Option<&Path>) -> DiscoveryEnv {
    DiscoveryEnv {
        claude_config_dir: claude.map(Path::to_path_buf),
        xdg_config_home: xdg.map(Path::to_path_buf),
        home: home.map(Path::to_path_buf),
    }
}

fn write_jsonl(dir: &Path, workspace: &str, session: &str, lines: &[&str]) -> PathBuf {
    let target = dir.join(workspace);
    fs::create_dir_all(&target).unwrap();
    let path = target.join(session);
    fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

fn record(ts: &str, input: u64, output: u64, id: Option<&str>) -> String {
    let id_part = id.map_or(String::new(), |i| format!(r#","id":"{i}""#));
    format!(
        r#"{{"timestamp":"{ts}","message":{{"usage":{{"input_tokens":{input},"output_tokens":{output}}},"model":"claude-opus-4-7"{id_part}}}}}"#
    )
}

// --- project_roots cascade ----------------------------------------

#[test]
fn project_roots_includes_env_dir_when_set() {
    let tmp = TempDir::new().unwrap();
    let env = env_from(Some(tmp.path()), None, Some(tmp.path()));
    let roots = project_roots(&env);
    assert!(roots[0].ends_with("projects"));
    assert!(roots[0].starts_with(tmp.path()));
}

#[test]
fn project_roots_omits_env_dir_when_unset() {
    let tmp = TempDir::new().unwrap();
    let env = env_from(None, None, Some(tmp.path()));
    let roots = project_roots(&env);
    for r in &roots {
        assert!(!r
            .parent()
            .unwrap()
            .ends_with(tmp.path().file_name().unwrap()));
    }
}

#[test]
fn project_roots_falls_back_to_home_when_xdg_unset() {
    let tmp = TempDir::new().unwrap();
    let env = env_from(None, None, Some(tmp.path()));
    let roots = project_roots(&env);
    assert!(roots
        .iter()
        .any(|r| r.starts_with(tmp.path().join(".config"))));
    assert!(roots
        .iter()
        .any(|r| r.starts_with(tmp.path().join(".claude"))));
}

// --- aggregate_jsonl_with: dir-level outcomes ---------------------

#[test]
fn aggregate_returns_directory_missing_when_no_roots_exist() {
    let tmp = TempDir::new().unwrap();
    let env = env_from(None, None, Some(&tmp.path().join("nonexistent")));
    let err = aggregate_jsonl_with(&env).unwrap_err();
    assert!(matches!(err, JsonlError::DirectoryMissing));
}

#[test]
fn aggregate_returns_no_entries_when_roots_empty() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".claude").join("projects")).unwrap();
    let env = env_from(None, None, Some(tmp.path()));
    let err = aggregate_jsonl_with(&env).unwrap_err();
    assert!(matches!(err, JsonlError::NoEntries));
}

// --- 5h block math ------------------------------------------------

#[test]
fn active_block_computed_from_recent_entries() {
    // Two entries within 5h of now.
    let now = Utc::now();
    let e1 = UsageEntry {
        timestamp: now - Duration::hours(1),
        message: MessageFields {
            usage: Some(UsageCounts {
                input_tokens: 100,
                output_tokens: 50,
                cache_creation: 0,
                cache_read: 0,
            }),
            model: Some("claude-opus-4-7".into()),
            id: Some("msg_1".into()),
        },
        usage_limit_reset_time: None,
    };
    let block = compute_active_block(&[e1], now).expect("active");
    assert_eq!(block.token_counts.input, 100);
    assert_eq!(block.models, vec!["claude-opus-4-7"]);
}

#[test]
fn no_active_block_when_last_entry_is_older_than_window() {
    let now = Utc::now();
    let e1 = UsageEntry {
        timestamp: now - Duration::hours(10),
        message: MessageFields::default(),
        usage_limit_reset_time: None,
    };
    assert!(compute_active_block(&[e1], now).is_none());
}

#[test]
fn new_block_starts_on_gap_exceeding_window() {
    // e1 at T-8h, e2 at T-1h. Gap is 7h > 5h, so a new block
    // starts at e2. Only the newer block is kept.
    let now = Utc::now();
    let e1 = UsageEntry {
        timestamp: now - Duration::hours(8),
        message: MessageFields {
            usage: Some(UsageCounts {
                input_tokens: 999,
                ..UsageCounts::default()
            }),
            ..MessageFields::default()
        },
        usage_limit_reset_time: None,
    };
    let e2 = UsageEntry {
        timestamp: now - Duration::hours(1),
        message: MessageFields {
            usage: Some(UsageCounts {
                input_tokens: 10,
                ..UsageCounts::default()
            }),
            ..MessageFields::default()
        },
        usage_limit_reset_time: None,
    };
    let block = compute_active_block(&[e1, e2], now).expect("active");
    // Only e2's tokens count — e1 was rolled into a closed block.
    assert_eq!(block.token_counts.input, 10);
}

#[test]
fn usage_limit_reset_picks_most_recent() {
    let now = Utc::now();
    let earlier_reset = now + Duration::hours(1);
    let later_reset = now + Duration::hours(2);
    let e1 = UsageEntry {
        timestamp: now - Duration::minutes(90),
        message: MessageFields::default(),
        usage_limit_reset_time: Some(earlier_reset),
    };
    let e2 = UsageEntry {
        timestamp: now - Duration::minutes(30),
        message: MessageFields::default(),
        usage_limit_reset_time: Some(later_reset),
    };
    let block = compute_active_block(&[e1, e2], now).expect("active");
    assert_eq!(block.usage_limit_reset, Some(later_reset));
}

// --- Per-line record parsing --------------------------------------

#[test]
fn parses_full_record_shape() {
    let line = r#"{"timestamp":"2026-04-20T14:23:47Z","message":{"id":"msg_1","model":"claude-opus-4-7","usage":{"input_tokens":1842,"output_tokens":631,"cache_creation_input_tokens":0,"cache_read_input_tokens":48122}},"costUSD":0.0421,"version":"1.0.85","usageLimitResetTime":"2026-04-20T19:00:00Z"}"#;
    let entry: UsageEntry = serde_json::from_str(line).expect("parse");
    let u = entry.message.usage.unwrap();
    assert_eq!(u.input_tokens, 1842);
    assert_eq!(u.cache_read, 48122);
    assert_eq!(entry.message.id.as_deref(), Some("msg_1"));
    assert!(entry.usage_limit_reset_time.is_some());
}

#[test]
fn parses_sparse_record_shape() {
    // Missing model, id, cost, reset — just timestamp + usage.
    let line = r#"{"timestamp":"2026-04-20T14:23:47Z","message":{"usage":{"input_tokens":100,"output_tokens":50}}}"#;
    let entry: UsageEntry = serde_json::from_str(line).expect("parse");
    assert_eq!(entry.message.usage.unwrap().input_tokens, 100);
    assert!(entry.message.id.is_none());
}

#[test]
fn unknown_fields_are_dropped() {
    // Future schema extension shouldn't break parsing.
    let line = r#"{"timestamp":"2026-04-20T14:23:47Z","message":{},"futureField":"ignored","anotherThing":{"nested":true}}"#;
    serde_json::from_str::<UsageEntry>(line).expect("parse");
}

// --- JsonlTailer -------------------------------------------------

#[test]
fn tailer_reads_all_lines_on_first_call() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("t.jsonl");
    let lines = [
        record("2026-04-20T00:00:00Z", 1, 1, Some("a")),
        record("2026-04-20T00:01:00Z", 2, 2, Some("b")),
        record("2026-04-20T00:02:00Z", 3, 3, Some("c")),
    ];
    fs::write(&path, lines.join("\n") + "\n").unwrap();
    let mut tailer = JsonlTailer::new(path);
    let entries = tailer.read_new().expect("ok");
    assert_eq!(entries.len(), 3);
}

#[test]
fn tailer_only_reads_new_lines_on_second_call() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("t.jsonl");
    fs::write(
        &path,
        record("2026-04-20T00:00:00Z", 1, 1, Some("a")) + "\n",
    )
    .unwrap();
    let mut tailer = JsonlTailer::new(path.clone());
    let first = tailer.read_new().expect("ok");
    assert_eq!(first.len(), 1);

    let existing = fs::read_to_string(&path).unwrap();
    let new_line = record("2026-04-20T00:01:00Z", 2, 2, Some("b"));
    fs::write(&path, format!("{existing}{new_line}\n")).unwrap();
    let second = tailer.read_new().expect("ok");
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].message.id.as_deref(), Some("b"));
}

#[test]
fn tailer_returns_empty_for_missing_file() {
    let tmp = TempDir::new().unwrap();
    let mut tailer = JsonlTailer::new(tmp.path().join("nonexistent.jsonl"));
    let entries = tailer.read_new().expect("ok");
    assert!(entries.is_empty());
}

#[test]
fn tailer_resets_on_truncation() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("t.jsonl");
    let initial = [
        record("2026-04-20T00:00:00Z", 1, 1, Some("a")),
        record("2026-04-20T00:01:00Z", 2, 2, Some("b")),
    ];
    fs::write(&path, initial.join("\n") + "\n").unwrap();
    let mut tailer = JsonlTailer::new(path.clone());
    tailer.read_new().expect("first");

    let new_line = record("2026-04-20T00:02:00Z", 3, 3, Some("c"));
    fs::write(&path, new_line + "\n").unwrap();
    let after = tailer.read_new().expect("after truncate");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].message.id.as_deref(), Some("c"));
}

#[test]
fn tailer_skips_partial_trailing_line() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("t.jsonl");
    let complete = record("2026-04-20T00:00:00Z", 1, 1, Some("a"));
    fs::write(
        &path,
        format!("{complete}\n{}", r#"{"timestamp":"2026-04-20T00:01:00Z""#),
    )
    .unwrap();
    let mut tailer = JsonlTailer::new(path);
    let entries = tailer.read_new().expect("ok");
    // Partial line must not advance the offset — it would be
    // re-read on the next poll once the tail completes.
    assert_eq!(entries.len(), 1);
}

#[test]
fn tailer_skips_non_utf8_line_and_keeps_later_valid_lines() {
    // Per spec §Edge cases, non-UTF-8 is a malformed-line class
    // — the tailer must skip just the bad line, NOT abandon the
    // rest of the file. Pre-fix, `read_line(&mut String)` raised
    // `InvalidData` and the file was dropped entirely.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("t.jsonl");
    let good_before = record("2026-04-20T00:00:00Z", 1, 1, Some("before"));
    let good_after = record("2026-04-20T00:02:00Z", 3, 3, Some("after"));
    let mut bytes = Vec::new();
    bytes.extend_from_slice(good_before.as_bytes());
    bytes.push(b'\n');
    bytes.extend_from_slice(&[0xFF, 0xFE, 0xFD, b'\n']);
    bytes.extend_from_slice(good_after.as_bytes());
    bytes.push(b'\n');
    fs::write(&path, &bytes).unwrap();
    let mut tailer = JsonlTailer::new(path);
    let entries = tailer.read_new().expect("ok");
    assert_eq!(entries.len(), 2);
}

#[test]
fn tailer_skips_malformed_lines_and_advances_past_them() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("t.jsonl");
    let good = record("2026-04-20T00:00:00Z", 1, 1, Some("a"));
    let bad = "{ this is not json }";
    let good2 = record("2026-04-20T00:01:00Z", 2, 2, Some("b"));
    fs::write(&path, format!("{good}\n{bad}\n{good2}\n")).unwrap();
    let mut tailer = JsonlTailer::new(path);
    let entries = tailer.read_new().expect("ok");
    assert_eq!(entries.len(), 2);
}

// --- Dedup semantics ----------------------------------------------

#[test]
fn aggregate_dedupes_on_message_id() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    let now = Utc::now();
    let ts = now
        .duration_trunc(Duration::minutes(1))
        .unwrap()
        .to_rfc3339();
    let line = record(&ts, 100, 50, Some("dup-1"));
    write_jsonl(&projects, "-proj-a", "sess1.jsonl", &[&line]);
    write_jsonl(&projects, "-proj-a", "sess2.jsonl", &[&line]);

    let env = env_from(None, None, Some(home));
    let agg = aggregate_jsonl_with(&env).expect("aggregate");
    // Dedupe merges the shared id across sessions → 100, not 200.
    assert_eq!(agg.seven_day.token_counts.input, 100);
}

#[test]
fn aggregate_keeps_missing_id_entries_individually() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    let now = Utc::now();
    let ts = now
        .duration_trunc(Duration::minutes(1))
        .unwrap()
        .to_rfc3339();
    let line = record(&ts, 100, 50, None);
    write_jsonl(&projects, "-proj-a", "sess1.jsonl", &[&line, &line]);

    let env = env_from(None, None, Some(home));
    let agg = aggregate_jsonl_with(&env).expect("aggregate");
    assert_eq!(agg.seven_day.token_counts.input, 200);
}

// --- Full aggregate integration -----------------------------------

#[test]
fn aggregate_happy_path_produces_active_block_and_7d_window() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    let now = Utc::now();
    let recent_ts = now
        .duration_trunc(Duration::minutes(1))
        .unwrap()
        .to_rfc3339();
    let old_ts = (now - Duration::days(3))
        .duration_trunc(Duration::minutes(1))
        .unwrap()
        .to_rfc3339();
    let old_line = record(&old_ts, 500, 100, Some("old-1"));
    let recent_line = record(&recent_ts, 250, 50, Some("new-1"));
    write_jsonl(
        &projects,
        "-Users-alice-code-myrepo",
        "session.jsonl",
        &[&old_line, &recent_line],
    );

    let env = env_from(None, None, Some(home));
    let agg = aggregate_jsonl_with(&env).expect("aggregate");

    // 7d window picks up both entries.
    assert_eq!(agg.seven_day.token_counts.input, 750);
    // Active block is anchored to the recent entry.
    let block = agg.five_hour.expect("active block");
    assert_eq!(block.token_counts.input, 250);
}

#[test]
fn aggregate_old_only_transcript_has_no_active_block() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    let old_ts = (Utc::now() - Duration::days(10))
        .duration_trunc(Duration::minutes(1))
        .unwrap()
        .to_rfc3339();
    let line = record(&old_ts, 100, 50, Some("old-1"));
    write_jsonl(&projects, "-proj-a", "session.jsonl", &[&line]);

    let env = env_from(None, None, Some(home));
    let agg = aggregate_jsonl_with(&env).expect("aggregate");
    assert!(agg.five_hour.is_none());
    // 7d window also excludes the >7d entry.
    assert_eq!(agg.seven_day.token_counts.input, 0);
}

// --- TokenCounts saturating arithmetic ---------------------------

#[test]
fn token_counts_total_saturates_on_overflow() {
    let counts = TokenCounts::from_parts(u64::MAX - 5, 10, 0, 0);
    assert_eq!(counts.total(), u64::MAX);
}

#[test]
fn token_counts_from_parts_pins_positional_argument_order() {
    // Regression guard: every in-crate test calls this constructor
    // with specific values in specific positions (e.g. the cascade
    // `jsonl_ok` fixture uses 1_000_000 input + 200_000 output).
    // If the argument order is ever reshuffled, the `total()`
    // assertions downstream still pass (sums are order-invariant)
    // so a typed-field check here is the only structural guard.
    let t = TokenCounts::from_parts(1, 2, 3, 4);
    assert_eq!(t.input(), 1);
    assert_eq!(t.output(), 2);
    assert_eq!(t.cache_creation(), 3);
    assert_eq!(t.cache_read(), 4);
    assert_eq!(t.total(), 10);
}

// --- Error taxonomy -----------------------------------------------

#[test]
fn jsonl_error_code_taxonomy_is_unique() {
    let all: [(JsonlError, &str); 4] = [
        (JsonlError::DirectoryMissing, "DirectoryMissing"),
        (JsonlError::NoEntries, "NoEntries"),
        (
            JsonlError::IoError {
                path: PathBuf::from("/x"),
                cause: io::Error::other("x"),
            },
            "IoError",
        ),
        (
            JsonlError::ParseError {
                path: PathBuf::from("/x"),
                line: 1,
                cause: serde_json::from_str::<i32>("x").unwrap_err(),
            },
            "ParseError",
        ),
    ];
    let codes: std::collections::HashSet<&'static str> =
        all.iter().map(|(e, _)| e.code()).collect();
    assert_eq!(codes.len(), 4);
    for (err, expected) in &all {
        assert_eq!(err.code(), *expected);
    }
}

// --- floor_to_hour edge case --------------------------------------

#[test]
fn floor_to_hour_truncates_subhour_components() {
    let ts = Utc.with_ymd_and_hms(2026, 4, 20, 14, 37, 52).unwrap();
    let floored = floor_to_hour(ts);
    assert_eq!(
        floored,
        Utc.with_ymd_and_hms(2026, 4, 20, 14, 0, 0).unwrap()
    );
}

// --- FiveHourBlock::end derivation --------------------------------

#[test]
fn five_hour_block_end_derives_from_start() {
    let now = Utc::now();
    let e = UsageEntry {
        timestamp: now - Duration::minutes(30),
        message: MessageFields::default(),
        usage_limit_reset_time: None,
    };
    let block = compute_active_block(&[e], now).expect("active");
    assert_eq!(
        block.end(),
        block.start + Duration::hours(BLOCK_DURATION_HOURS)
    );
}

// --- Block-boundary math (off-by-one guard) -----------------------

#[test]
fn entries_exactly_5h_apart_stay_in_same_block() {
    // Spec: gap > 5h opens a new block. A gap of exactly 5h
    // (not strictly greater) must keep both entries in the
    // single rolling block. Guards against a `>=` refactor.
    let now = Utc::now();
    let e1 = UsageEntry {
        timestamp: now - Duration::hours(5),
        message: MessageFields {
            usage: Some(UsageCounts {
                input_tokens: 100,
                ..UsageCounts::default()
            }),
            ..MessageFields::default()
        },
        usage_limit_reset_time: None,
    };
    let e2 = UsageEntry {
        timestamp: now,
        message: MessageFields {
            usage: Some(UsageCounts {
                input_tokens: 50,
                ..UsageCounts::default()
            }),
            ..MessageFields::default()
        },
        usage_limit_reset_time: None,
    };
    let block = compute_active_block(&[e1, e2], now).expect("active");
    // Both entries rolled into one block → 150 total input.
    assert_eq!(block.token_counts.input, 150);
}

#[test]
fn gap_of_5h_plus_one_ns_opens_new_block() {
    // Strict `>` — one nanosecond past the boundary flips.
    let now = Utc::now();
    let e1 = UsageEntry {
        timestamp: now - Duration::hours(5) - Duration::nanoseconds(1),
        message: MessageFields {
            usage: Some(UsageCounts {
                input_tokens: 999,
                ..UsageCounts::default()
            }),
            ..MessageFields::default()
        },
        usage_limit_reset_time: None,
    };
    let e2 = UsageEntry {
        timestamp: now,
        message: MessageFields {
            usage: Some(UsageCounts {
                input_tokens: 7,
                ..UsageCounts::default()
            }),
            ..MessageFields::default()
        },
        usage_limit_reset_time: None,
    };
    let block = compute_active_block(&[e1, e2], now).expect("active");
    // Only the newer block survives; 999 was rolled into a
    // closed block and discarded in v0.1.
    assert_eq!(block.token_counts.input, 7);
}

// --- 7-day window boundary ---------------------------------------

#[test]
fn entry_at_exactly_7d_boundary_is_included() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    // Write timestamp that `build_aggregate`'s `window_start`
    // check (`e.timestamp >= window_start`) will include.
    // Use `now - 7d + 10s` so the spec's `>=` boundary is
    // pinned without depending on instant-perfect math.
    let near_boundary = (Utc::now() - Duration::days(7) + Duration::seconds(10))
        .duration_trunc(Duration::seconds(1))
        .unwrap()
        .to_rfc3339();
    let line = record(&near_boundary, 42, 0, Some("boundary"));
    write_jsonl(&projects, "-proj", "sess.jsonl", &[&line]);
    let env = env_from(None, None, Some(home));
    let agg = aggregate_jsonl_with(&env).expect("aggregate");
    assert_eq!(agg.seven_day.token_counts.input, 42);
}

#[test]
fn entry_older_than_7d_excluded_from_window() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    // `now - 8d` is definitively outside the window.
    let old = (Utc::now() - Duration::days(8))
        .duration_trunc(Duration::seconds(1))
        .unwrap()
        .to_rfc3339();
    let line = record(&old, 1000, 0, Some("way-old"));
    write_jsonl(&projects, "-proj", "sess.jsonl", &[&line]);
    let env = env_from(None, None, Some(home));
    let agg = aggregate_jsonl_with(&env).expect("aggregate");
    assert_eq!(agg.seven_day.token_counts.input, 0);
}

// --- Cross-root dedup --------------------------------------------

#[test]
fn aggregate_dedupes_across_cascade_roots() {
    // Spec: the same message.id written to two different roots
    // (one after a user migrates ~/.claude → CLAUDE_CONFIG_DIR,
    // say) collapses to a single entry. Regression guard for
    // a per-root HashMap without a global merge step.
    let tmp = TempDir::new().unwrap();
    let env_dir = tmp.path().join("env-dir");
    let home = tmp.path().join("home");
    let env_projects = env_dir.join("projects");
    let legacy_projects = home.join(".claude").join("projects");
    let ts = Utc::now()
        .duration_trunc(Duration::minutes(1))
        .unwrap()
        .to_rfc3339();
    let line = record(&ts, 100, 50, Some("shared-msg"));
    write_jsonl(&env_projects, "-proj", "sess-env.jsonl", &[&line]);
    write_jsonl(&legacy_projects, "-proj", "sess-legacy.jsonl", &[&line]);
    let env = env_from(Some(&env_dir), None, Some(&home));
    let agg = aggregate_jsonl_with(&env).expect("aggregate");
    // If dedup is scoped per-root, this would show 200 tokens.
    assert_eq!(agg.seven_day.token_counts.input, 100);
}

// --- Offset monotonicity invariant -------------------------------

#[test]
fn tailer_offset_monotonically_advances_on_repeat_reads() {
    // Invariant from spec §Testing strategy: new_offset >=
    // old_offset and new_offset <= file_size. Exercise several
    // append cycles.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("t.jsonl");
    fs::write(
        &path,
        record("2026-04-20T00:00:00Z", 1, 1, Some("a")) + "\n",
    )
    .unwrap();
    let mut tailer = JsonlTailer::new(path.clone());
    tailer.read_new().expect("first");
    let after_first = tailer.last_offset;
    assert_eq!(after_first, tailer.last_size);

    let existing = fs::read_to_string(&path).unwrap();
    let new_line = record("2026-04-20T00:01:00Z", 2, 2, Some("b"));
    fs::write(&path, format!("{existing}{new_line}\n")).unwrap();
    tailer.read_new().expect("second");
    let after_second = tailer.last_offset;
    assert!(after_second > after_first, "offset must advance");
    assert_eq!(after_second, tailer.last_size);

    tailer.read_new().expect("third");
    assert_eq!(tailer.last_offset, after_second);
}

// --- Models dedup within a block ---------------------------------

#[test]
fn block_models_dedupes_within_block() {
    let now = Utc::now();
    fn mk(ts: DateTime<Utc>, model: &str) -> UsageEntry {
        UsageEntry {
            timestamp: ts,
            message: MessageFields {
                model: Some(model.to_string()),
                ..MessageFields::default()
            },
            usage_limit_reset_time: None,
        }
    }
    let entries = [
        mk(now - Duration::minutes(30), "claude-opus-4-7"),
        mk(now - Duration::minutes(20), "claude-sonnet-4-6"),
        mk(now - Duration::minutes(10), "claude-opus-4-7"),
    ];
    let block = compute_active_block(&entries, now).expect("active");
    assert_eq!(block.models.len(), 2);
}

// --- usage_limit_reset merge -------------------------------------

// --- XDG-without-HOME cascade ------------------------------------

#[test]
fn project_roots_includes_xdg_when_home_unset() {
    // Service/CI environments can set $XDG_CONFIG_HOME without
    // $HOME; the XDG projects root must not be gated on home.
    // Matches the credentials::file_cascade_candidates pattern.
    let tmp = TempDir::new().unwrap();
    let xdg = tmp.path().join("xdg");
    let env = env_from(None, Some(&xdg), None);
    let roots = project_roots(&env);
    assert!(
        roots
            .iter()
            .any(|r| r == &xdg.join("claude").join("projects")),
        "XDG candidate must be present with HOME unset + XDG set",
    );
    assert!(
        !roots.iter().any(|r| r.ends_with(".claude/projects")),
        "Legacy ~/.claude requires HOME",
    );
}

#[test]
fn aggregate_reads_xdg_projects_when_home_unset() {
    let tmp = TempDir::new().unwrap();
    let xdg = tmp.path().join("xdg");
    let ts = Utc::now()
        .duration_trunc(Duration::minutes(1))
        .unwrap()
        .to_rfc3339();
    let line = record(&ts, 77, 33, Some("xdg-only"));
    write_jsonl(
        &xdg.join("claude").join("projects"),
        "-proj",
        "sess.jsonl",
        &[&line],
    );
    let env = env_from(None, Some(&xdg), None);
    let agg = aggregate_jsonl_with(&env).expect("aggregate");
    assert_eq!(agg.seven_day.token_counts.input, 77);
}

// --- Future-dated entry excluded from 7d window ------------------

#[test]
fn seven_day_window_excludes_future_timestamps() {
    // Clock skew (backup restore, NTP glitch, VM resume) can
    // stamp entries in the future. They must NOT inflate the
    // 7d totals — spec says window is `[now - 7d, now]`.
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    let future = (Utc::now() + Duration::hours(2))
        .duration_trunc(Duration::seconds(1))
        .unwrap()
        .to_rfc3339();
    let future_line = record(&future, 500, 0, Some("future-1"));
    let past = Utc::now()
        .duration_trunc(Duration::seconds(1))
        .unwrap()
        .to_rfc3339();
    let past_line = record(&past, 10, 0, Some("past-1"));
    write_jsonl(
        &projects,
        "-proj",
        "sess.jsonl",
        &[&future_line, &past_line],
    );
    let env = env_from(None, None, Some(home));
    let agg = aggregate_jsonl_with(&env).expect("aggregate");
    // Only the present entry contributes; future one excluded.
    // Assert across categories so a half-applied fix that only
    // guards `input_tokens` (leaving `output_tokens` to bleed
    // through) trips this test.
    assert_eq!(agg.seven_day.token_counts.input, 10);
    assert_eq!(agg.seven_day.token_counts.output, 0);
    assert_eq!(agg.seven_day.token_counts.cache_creation, 0);
    assert_eq!(agg.seven_day.token_counts.cache_read, 0);
}

#[test]
fn claude_config_dir_only_no_home_no_xdg() {
    // Pure env-dir cascade: CLAUDE_CONFIG_DIR set, both HOME and
    // XDG_CONFIG_HOME unset. Must produce exactly one candidate.
    let tmp = TempDir::new().unwrap();
    let env = env_from(Some(tmp.path()), None, None);
    let roots = project_roots(&env);
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0], tmp.path().join("projects"));
}

#[test]
fn future_timestamp_inside_5h_block_is_counted_as_mild_skew() {
    // Pins the intended behavior for mild clock skew: an entry
    // stamped in the near future still anchors the active block
    // (the `now - actual_last_activity > 5h` liveness check is
    // false for negative durations). Users with slightly-fast
    // clocks shouldn't have their current session's tokens
    // disappear. Pathological far-future skew is a separate
    // hardening question, out of scope here.
    let now = Utc::now();
    let future_entry = UsageEntry {
        timestamp: now + Duration::minutes(10),
        message: MessageFields {
            usage: Some(UsageCounts {
                input_tokens: 42,
                ..UsageCounts::default()
            }),
            ..MessageFields::default()
        },
        usage_limit_reset_time: None,
    };
    let block = compute_active_block(&[future_entry], now).expect("active");
    assert_eq!(block.token_counts.input, 42);
}

#[test]
fn usage_limit_reset_keeps_some_over_later_none() {
    // `extend_block` only overwrites when the incoming entry
    // carries `Some(_)`. A later entry with `None` must not
    // clear the earlier reset hint.
    let now = Utc::now();
    let reset = now + Duration::hours(1);
    let e1 = UsageEntry {
        timestamp: now - Duration::minutes(30),
        message: MessageFields::default(),
        usage_limit_reset_time: Some(reset),
    };
    let e2 = UsageEntry {
        timestamp: now - Duration::minutes(10),
        message: MessageFields::default(),
        usage_limit_reset_time: None,
    };
    let block = compute_active_block(&[e1, e2], now).expect("active");
    assert_eq!(block.usage_limit_reset, Some(reset));
}
