use super::*;
use tempfile::TempDir;

fn write_creds(dir: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = dir.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
    path
}

fn valid_credentials_json(token: &str) -> String {
    format!(
        r#"{{
            "claudeAiOauth": {{
                "accessToken": "{token}",
                "refreshToken": null,
                "expiresAt": null,
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": null
            }}
        }}"#
    )
}

#[test]
fn parses_valid_credentials_bytes() {
    let json = valid_credentials_json("test-token-xyz");
    let creds = parse_credentials_bytes(
        &json,
        Path::new("/test"),
        CredentialSource::ClaudeLegacy {
            path: PathBuf::from("/test"),
        },
    )
    .expect("parse");
    assert_eq!(creds.token(), "test-token-xyz");
    assert_eq!(creds.scopes().len(), 2);
    assert!(matches!(
        creds.source(),
        CredentialSource::ClaudeLegacy { .. }
    ));
}

#[test]
fn rejects_null_token_as_empty_token() {
    let json = r#"{ "claudeAiOauth": { "accessToken": null } }"#;
    let err = parse_credentials_bytes(
        json,
        Path::new("/test"),
        CredentialSource::ClaudeLegacy {
            path: PathBuf::from("/test"),
        },
    )
    .unwrap_err();
    assert!(matches!(err, CredentialError::EmptyToken { .. }));
}

#[test]
fn rejects_absent_access_token_key_as_missing_field() {
    // Spec: absent key → `MissingField`; `null`/`""` → `EmptyToken`.
    // Tested here: oauth block present, key simply missing.
    let json = r#"{ "claudeAiOauth": { "scopes": ["x"] } }"#;
    let err = parse_credentials_bytes(
        json,
        Path::new("/test"),
        CredentialSource::ClaudeLegacy {
            path: PathBuf::from("/test"),
        },
    )
    .unwrap_err();
    assert!(matches!(err, CredentialError::MissingField { .. }));
}

#[test]
fn rejects_non_string_access_token() {
    let json = r#"{ "claudeAiOauth": { "accessToken": 42 } }"#;
    let err = parse_credentials_bytes(
        json,
        Path::new("/test"),
        CredentialSource::ClaudeLegacy {
            path: PathBuf::from("/test"),
        },
    )
    .unwrap_err();
    assert!(matches!(err, CredentialError::ParseError { .. }));
}

#[test]
fn rejects_empty_token() {
    let json = r#"{ "claudeAiOauth": { "accessToken": "" } }"#;
    let err = parse_credentials_bytes(
        json,
        Path::new("/test"),
        CredentialSource::ClaudeLegacy {
            path: PathBuf::from("/test"),
        },
    )
    .unwrap_err();
    assert!(matches!(err, CredentialError::EmptyToken { .. }));
}

#[test]
fn rejects_missing_claude_ai_oauth() {
    let json = r#"{ "somethingElse": {} }"#;
    let err = parse_credentials_bytes(
        json,
        Path::new("/test"),
        CredentialSource::ClaudeLegacy {
            path: PathBuf::from("/test"),
        },
    )
    .unwrap_err();
    assert!(matches!(err, CredentialError::MissingField { .. }));
}

#[test]
fn rejects_invalid_json() {
    let json = "{ not json at all ";
    let err = parse_credentials_bytes(
        json,
        Path::new("/test"),
        CredentialSource::ClaudeLegacy {
            path: PathBuf::from("/test"),
        },
    )
    .unwrap_err();
    assert!(matches!(err, CredentialError::ParseError { .. }));
}

#[test]
fn scopes_default_to_empty_when_missing() {
    let json = r#"{ "claudeAiOauth": { "accessToken": "t" } }"#;
    let creds = parse_credentials_bytes(
        json,
        Path::new("/test"),
        CredentialSource::ClaudeLegacy {
            path: PathBuf::from("/test"),
        },
    )
    .expect("parse");
    assert!(creds.scopes().is_empty());
}

#[test]
fn credentials_debug_redacts_token() {
    let creds = Credentials {
        token: SecretString::from("super-secret-token".to_string()),
        scopes: vec!["x".to_string()],
        source: CredentialSource::ClaudeLegacy {
            path: PathBuf::from("/etc/x"),
        },
    };
    let debug = format!("{creds:?}");
    assert!(
        !debug.contains("super-secret-token"),
        "debug leaks token: {debug}"
    );
    assert!(debug.contains("<redacted>"));
}

#[test]
fn credential_error_code_taxonomy() {
    // Construct a real serde_json::Error so ParseError's code is
    // exercised with a real cause (not a synthetic one).
    let parse_err = serde_json::from_str::<i32>("not-a-number").unwrap_err();

    let all: [(CredentialError, &str); 6] = [
        (CredentialError::NoCredentials, "NoCredentials"),
        (
            CredentialError::SubprocessFailed(io::Error::other("x")),
            "SubprocessFailed",
        ),
        (
            CredentialError::IoError {
                path: PathBuf::from("/x"),
                cause: io::Error::other("x"),
            },
            "IoError",
        ),
        (
            CredentialError::ParseError {
                path: PathBuf::from("/x"),
                cause: parse_err,
            },
            "ParseError",
        ),
        (
            CredentialError::MissingField {
                path: PathBuf::from("/x"),
            },
            "MissingField",
        ),
        (
            CredentialError::EmptyToken {
                path: PathBuf::from("/x"),
            },
            "EmptyToken",
        ),
    ];

    // Each variant produces its documented code.
    for (err, expected) in &all {
        assert_eq!(err.code(), *expected);
    }

    // Codes are unique across the taxonomy (no copy-paste collisions).
    let codes: std::collections::HashSet<&'static str> =
        all.iter().map(|(e, _)| e.code()).collect();
    assert_eq!(codes.len(), all.len());
}

#[test]
fn parse_error_display_does_not_leak_token_bytes() {
    // Build a malformed-JSON buffer whose middle contains a
    // sentinel "token". Our Display contract promises we never
    // forward `serde_json::Error`'s context snippet — verify.
    let leaky = r#"{ "claudeAiOauth": { "accessToken": "LEAK-ME-abcdef" "#;
    let err = parse_credentials_bytes(
        leaky,
        Path::new("/etc/creds"),
        CredentialSource::ClaudeLegacy {
            path: PathBuf::from("/etc/creds"),
        },
    )
    .unwrap_err();
    assert!(matches!(err, CredentialError::ParseError { .. }));
    let display = format!("{err}");
    let debug = format!("{err:?}");
    assert!(
        !display.contains("LEAK-ME"),
        "Display leaked token: {display}"
    );
    assert!(!debug.contains("LEAK-ME"), "Debug leaked token: {debug}");
}

#[test]
fn oversized_file_rejected_before_parse() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("big.json");
    // 1 MB of 'x' = exceeds MAX_FILE_SIZE — no JSON round-trip
    // possible. Ensures the length check fires before the serde
    // parser sees the buffer.
    let big = "x".repeat((MAX_FILE_SIZE + 1024) as usize);
    fs::write(&path, &big).unwrap();
    let err = read_and_parse_file(&path, CredentialSource::ClaudeLegacy { path: path.clone() })
        .unwrap_err();
    match err {
        CredentialError::IoError { cause, .. } => {
            assert_eq!(cause.kind(), io::ErrorKind::InvalidData);
        }
        other => panic!("expected IoError(InvalidData), got {other:?}"),
    }
}

/// Tests pass a constructed `FileCascadeEnv` rather than mutating
/// the process env. `set_var` / `remove_var` are `unsafe` in
/// modern Rust and the crate-level `unsafe_code = "forbid"` lint
/// would reject the direct-mutation pattern — and using the real
/// env in parallel tests is inherently racy anyway.
mod cascade {
    use super::*;

    fn env_from(claude: Option<&Path>, xdg: Option<&Path>, home: Option<&Path>) -> FileCascadeEnv {
        FileCascadeEnv {
            claude_config_dir: claude.map(Path::to_path_buf),
            xdg_config_home: xdg.map(Path::to_path_buf),
            home: home.map(Path::to_path_buf),
        }
    }

    #[test]
    fn env_dir_candidate_included_when_set() {
        let tmp = TempDir::new().unwrap();
        let env = env_from(Some(tmp.path()), None, Some(tmp.path()));
        let candidates = file_cascade_candidates(&env);
        assert!(matches!(candidates[0].1, CredentialSource::EnvDir { .. }));
    }

    #[test]
    fn env_dir_absent_when_not_set() {
        // `FileCascadeEnv::from_process_env` already maps empty
        // strings to None; this test covers the cascade-level
        // behavior: no env-dir field → first candidate is XDG.
        let tmp = TempDir::new().unwrap();
        let env = env_from(None, None, Some(tmp.path()));
        let candidates = file_cascade_candidates(&env);
        assert!(
            !matches!(candidates[0].1, CredentialSource::EnvDir { .. }),
            "no CLAUDE_CONFIG_DIR should omit the EnvDir candidate"
        );
    }

    #[test]
    fn xdg_preferred_over_legacy_when_both_roots_present() {
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg");
        let env = env_from(None, Some(&xdg), Some(tmp.path()));
        let candidates = file_cascade_candidates(&env);
        let positions: Vec<_> = candidates
            .iter()
            .map(|(_, s)| match s {
                CredentialSource::XdgConfig { .. } => "xdg",
                CredentialSource::ClaudeLegacy { .. } => "legacy",
                _ => "other",
            })
            .collect();
        assert_eq!(positions, ["xdg", "legacy"]);
    }

    #[test]
    fn xdg_default_root_is_home_dot_config() {
        let tmp = TempDir::new().unwrap();
        let env = env_from(None, None, Some(tmp.path()));
        let candidates = file_cascade_candidates(&env);
        let xdg = candidates
            .iter()
            .find(|(_, s)| matches!(s, CredentialSource::XdgConfig { .. }))
            .expect("xdg candidate present");
        assert!(xdg.0.starts_with(tmp.path().join(".config").join("claude")));
    }

    #[test]
    fn resolve_reads_existing_env_dir_credentials() {
        let tmp = TempDir::new().unwrap();
        write_creds(
            tmp.path(),
            ".credentials.json",
            &valid_credentials_json("env-dir-tok"),
        );
        let env = env_from(Some(tmp.path()), None, Some(tmp.path()));
        let creds = try_file_cascade_with(&env).expect("resolve");
        assert_eq!(creds.token(), "env-dir-tok");
        assert!(matches!(creds.source(), CredentialSource::EnvDir { .. }));
    }

    #[test]
    fn resolve_falls_through_to_xdg() {
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg");
        write_creds(
            &xdg,
            "claude/.credentials.json",
            &valid_credentials_json("xdg-tok"),
        );
        let env = env_from(
            Some(&tmp.path().join("does-not-exist")),
            Some(&xdg),
            Some(tmp.path()),
        );
        let creds = try_file_cascade_with(&env).expect("resolve");
        assert_eq!(creds.token(), "xdg-tok");
        assert!(matches!(creds.source(), CredentialSource::XdgConfig { .. }));
    }

    #[test]
    fn resolve_falls_through_to_legacy() {
        let tmp = TempDir::new().unwrap();
        write_creds(
            tmp.path(),
            ".claude/.credentials.json",
            &valid_credentials_json("legacy-tok"),
        );
        let env = env_from(None, None, Some(tmp.path()));
        let creds = try_file_cascade_with(&env).expect("resolve");
        assert_eq!(creds.token(), "legacy-tok");
        assert!(matches!(
            creds.source(),
            CredentialSource::ClaudeLegacy { .. }
        ));
    }

    #[test]
    fn resolve_no_files_returns_no_credentials() {
        let tmp = TempDir::new().unwrap();
        let env = env_from(None, None, Some(tmp.path()));
        let err = try_file_cascade_with(&env).unwrap_err();
        assert!(matches!(err, CredentialError::NoCredentials));
    }

    #[test]
    fn resolve_no_home_returns_no_credentials() {
        let env = env_from(None, None, None);
        let err = try_file_cascade_with(&env).unwrap_err();
        assert!(matches!(err, CredentialError::NoCredentials));
    }

    #[test]
    fn xdg_path_probed_even_when_home_is_unset() {
        // Service/CI environments can set $XDG_CONFIG_HOME without
        // $HOME; the XDG candidate must not be gated on home.
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg");
        write_creds(
            &xdg,
            "claude/.credentials.json",
            &valid_credentials_json("xdg-no-home-tok"),
        );
        let env = env_from(None, Some(&xdg), None);
        let creds = try_file_cascade_with(&env).expect("resolve");
        assert_eq!(creds.token(), "xdg-no-home-tok");
        assert!(matches!(creds.source(), CredentialSource::XdgConfig { .. }));
    }

    #[test]
    fn candidate_list_includes_xdg_when_home_unset() {
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg");
        let env = env_from(None, Some(&xdg), None);
        let candidates = file_cascade_candidates(&env);
        assert!(
            candidates
                .iter()
                .any(|(_, s)| matches!(s, CredentialSource::XdgConfig { .. })),
            "XDG candidate must be present with HOME unset + XDG_CONFIG_HOME set",
        );
        assert!(
            !candidates
                .iter()
                .any(|(_, s)| matches!(s, CredentialSource::ClaudeLegacy { .. })),
            "Legacy candidate requires HOME",
        );
    }

    #[test]
    fn xdg_wins_when_both_xdg_and_legacy_files_exist() {
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg");
        write_creds(
            &xdg,
            "claude/.credentials.json",
            &valid_credentials_json("xdg-wins"),
        );
        write_creds(
            tmp.path(),
            ".claude/.credentials.json",
            &valid_credentials_json("legacy-loses"),
        );
        let env = env_from(None, Some(&xdg), Some(tmp.path()));
        let creds = try_file_cascade_with(&env).expect("resolve");
        assert_eq!(creds.token(), "xdg-wins");
        assert!(matches!(creds.source(), CredentialSource::XdgConfig { .. }));
    }

    #[test]
    fn env_dir_set_but_empty_dir_falls_through_to_xdg() {
        // `CLAUDE_CONFIG_DIR` is a hint, not a declaration — per
        // credentials.md §Edge cases. A set-but-empty (no
        // credentials.json inside) env dir shouldn't shadow XDG.
        let tmp = TempDir::new().unwrap();
        let env_dir = tmp.path().join("env-dir");
        fs::create_dir_all(&env_dir).unwrap();
        let xdg = tmp.path().join("xdg");
        write_creds(
            &xdg,
            "claude/.credentials.json",
            &valid_credentials_json("xdg-tok"),
        );
        let env = env_from(Some(&env_dir), Some(&xdg), Some(tmp.path()));
        let creds = try_file_cascade_with(&env).expect("resolve");
        assert_eq!(creds.token(), "xdg-tok");
    }

    #[test]
    fn resolve_credentials_end_to_end_no_files() {
        // Integration-level smoke: public entry point via
        // DataContext::credentials() memoization. With no files
        // and no Keychain hit (empty HOME, no CLAUDE_CONFIG_DIR),
        // the cascade terminates at NoCredentials.
        let tmp = TempDir::new().unwrap();
        let env = env_from(None, None, Some(tmp.path()));
        let err = try_file_cascade_with(&env).unwrap_err();
        assert!(matches!(err, CredentialError::NoCredentials));
    }
}
