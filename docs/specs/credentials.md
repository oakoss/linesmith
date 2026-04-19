# Credentials

- Status: draft
- Version: 0.1
- Last updated: 2026-04-19
- Driving ADRs: [ADR-0010](../adrs/0010-data-fetching-architecture.md), [ADR-0011](../adrs/0011-rate-limit-data-source.md)

## Overview

Reading Claude Code's OAuth access token is the gatekeeper for the rate-limit segments. The token isn't stored in a single canonical location: on macOS it lives in the login Keychain, on Linux/Windows it lives in a JSON file whose path depends on environment variables and install layout. Multi-account users have more than one token on the same machine.

This spec defines the credential-resolution cascade, the memoization contract with `DataContext`, the token validation rules, and the failure modes that surface to segments.

It does NOT cover: how segments render credential errors ([rate-limit-segments.md](rate-limit-segments.md)), where tokens come from originally (they come from Claude Code's OAuth flow — out of scope), or Windows Credential Manager native API (deferred to v0.2).

## Requirements

### Functional

- Resolve the OAuth access token on every supported platform with minimum one subprocess or file read
- macOS primary path: `security find-generic-password -s "Claude Code-credentials"` with account = `$USER`
- macOS multi-account fallback: full Keychain dump filtered for `Claude Code-credentials*`, sorted by `mdat` modification time (newest wins), used when the primary call returns no token
- Linux / Windows path: check in order: `$CLAUDE_CONFIG_DIR/.credentials.json` → `~/.config/claude/.credentials.json` (XDG) → `~/.claude/.credentials.json` (Claude Code legacy). First file wins.
- Parse the file as JSON; extract `claudeAiOauth.accessToken` as the token value
- Validate: token must be a non-empty string. Null/empty → `NoCredentials`.
- Memoize the parsed `Credentials` value (or error) for the entire process lifetime
- Expose diagnostic info (without exposing the token itself) via `linesmith doctor`
- Never log the token value, even at trace level

### Non-functional

- Single credential-resolution pass per invocation, regardless of how many segments call `ctx.credentials()`
- Keychain subprocess budget: one `security find-generic-password` call; the multi-account fallback (full Keychain dump) only runs if the primary returns nothing
- Filesystem budget: at most three `stat` calls on Linux/Windows (one per cascade step); only the first existing file is read
- Memory: parsed `Credentials` is cheap; `Arc::clone` for sharing across segments
- Platform coverage: macOS (primary), Linux (file-based), Windows (file-based — Credential Manager is v0.2+)
- Token bytes stay in memory only; never written to our own cache files, logs, or telemetry

## Interface / Contract

### Types

```rust
#[derive(Debug, Clone)]
pub struct Credentials {
    /// OAuth Bearer token. Treated as sensitive — do not include in
    /// Debug output, logs, or cache files.
    token: SecretString,
    /// OAuth scopes granted to this token, parsed from the credentials
    /// file. Informational; not gated on.
    scopes: Vec<String>,
    /// Source path that yielded the token (for diagnostics via
    /// `linesmith doctor`). For macOS Keychain: "keychain:Claude Code-credentials[:mdat]".
    source: CredentialSource,
}

pub enum CredentialSource {
    MacosKeychainPrimary,
    MacosKeychainMultiAccount { service: String, mdat: Option<String> },
    EnvDir { path: PathBuf },        // $CLAUDE_CONFIG_DIR
    XdgConfig { path: PathBuf },     // ~/.config/claude/
    ClaudeLegacy { path: PathBuf },  // ~/.claude/
}

impl Credentials {
    pub fn token(&self) -> &str;   // returns the inner string
    pub fn scopes(&self) -> &[String];
    pub fn source(&self) -> &CredentialSource;
}

#[derive(Debug)]
pub enum CredentialError {
    /// No token found in any cascade location.
    NoCredentials,
    /// The `security` subprocess failed to launch, exit cleanly, or
    /// produce output (binary not found on PATH, Keychain locked,
    /// non-zero exit from keychain access denial). Used for Keychain
    /// interaction failures specifically, distinct from file-based
    /// credential reads.
    SubprocessFailed(io::Error),
    /// Credentials file found but could not be opened or read
    /// (permission denied, truncated read, filesystem error).
    /// Used for file-based credential reads; distinct from
    /// SubprocessFailed (Keychain) and ParseError (valid JSON shape check).
    IoError { path: PathBuf, cause: io::Error },
    /// Credentials file found but not valid JSON.
    ParseError { path: PathBuf, cause: serde_json::Error },
    /// Credentials file valid JSON but shape doesn't match
    /// (missing `claudeAiOauth.accessToken`).
    MissingField { path: PathBuf },
    /// Token field present but empty string or null.
    EmptyToken { path: PathBuf },
}
```

`SecretString` is `secrecy::SecretString` or equivalent; its `Debug` impl prints `<redacted>`. The `token()` accessor returns `&str` deliberately — consuming code owns the responsibility of not logging it.

### Resolution cascade

The single public entry point:

```rust
pub fn resolve_credentials() -> Result<Credentials, CredentialError>;
```

Implementation flow:

1. **macOS only** (`cfg(target_os = "macos")`):
   - Run `security find-generic-password -a $USER -w -s "Claude Code-credentials"`.
   - Stdout is a JSON string. Parse it with the shape below; extract `accessToken`.
   - On success, return `Credentials { source: MacosKeychainPrimary, ... }`.
   - On empty stdout or missing `accessToken`: fall through to multi-account fallback.
   - On non-zero exit due to "item not found": fall through to multi-account fallback.
   - On non-zero exit for other reasons (Keychain locked, permissions denied): record as `SubprocessFailed` but still fall through to multi-account fallback. Only surface `SubprocessFailed` if the fallback also yields nothing.

2. **macOS multi-account fallback**:
   - Run `security dump-keychain` (no args; dumps the login Keychain).
   - Parse output looking for blocks where `svce = "Claude Code-credentials<suffix>"`.
   - For each matching service, parse the `mdat` modification-time blob (hex-encoded ASCII timestamp in the `security` output).
   - Sort candidates: entries with `mdat` sort newer-first; entries without `mdat` sort last by dump order.
   - For each candidate in order, run `security find-generic-password -s <service> -w` and try to parse the token.
   - First successful parse returns `Credentials { source: MacosKeychainMultiAccount { service, mdat }, ... }`.
   - If no candidate yields a token, fall through to file-based cascade (Linux/Windows path — Keychain might be empty and user has a credentials file).

3. **File-based cascade** (all platforms, after macOS paths fail):
   - In order: `$CLAUDE_CONFIG_DIR/.credentials.json` (if env var set) → `~/.config/claude/.credentials.json` → `~/.claude/.credentials.json`.
   - For each path: `stat` it. If NotFound, advance. If exists, read and parse.
   - First readable+parseable file wins. Return `Credentials` with the appropriate `CredentialSource` variant.

4. **No token found**: return `CredentialError::NoCredentials`.

### Credentials file shape

```json
{
  "claudeAiOauth": {
    "accessToken": "<token>",
    "refreshToken": null,
    "expiresAt": null,
    "scopes": ["user:inference", "user:sessions:claude_code", ...],
    "subscriptionType": null
  }
}
```

Rust partial struct (only fields we consume):

```rust
#[derive(serde::Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeAiOauth>,
}

#[derive(serde::Deserialize)]
struct ClaudeAiOauth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(default)]
    scopes: Vec<String>,
}
```

`refreshToken`, `expiresAt`, `subscriptionType` are NOT consumed:

- `refreshToken` — refresh flow is Claude Code's responsibility; we re-read on next invocation
- `expiresAt` — we don't pre-validate; we trust the endpoint to return 401 if the token is expired
- `subscriptionType` — confirmed null for Max users; tier detection is out of scope per [ADR-0011](../adrs/0011-rate-limit-data-source.md)

### Integration with `DataContext`

Per [data-fetching.md](data-fetching.md) §DataContext:

```rust
impl DataContext {
    pub fn credentials(&self) -> Arc<Result<Credentials, CredentialError>> {
        self.credentials.get_or_init(|| Arc::new(resolve_credentials())).clone()
    }
}
```

First call runs the full cascade; subsequent calls return the same `Arc`. Errors are cached identically to successes.

## Behavior

### Token transfer to HTTP client

The rate-limit fetcher constructs the `Authorization: Bearer <token>` header directly from `credentials.token()`. The `SecretString` is exposed briefly during header construction; the HTTP client library should not log the header value.

```rust
let auth = format!("Bearer {}", creds.token());
// `auth` is a plain String at this point; do NOT clone it into logs.
```

v0.1 trusts `ureq` not to log headers at `info!` or below. If later versions add trace-level logging that could leak tokens, we redact at construction.

### macOS Keychain permission prompt

First time linesmith runs, macOS may prompt the user: "linesmith wants to use your confidential information stored in "Claude Code-credentials" in your keychain." This is expected. Users can grant "Always Allow" to suppress future prompts for the same binary.

linesmith does not try to work around this prompt — it's the OS's security model, and honoring it is correct behavior.

### Error-vs-fallback distinction

- `NoCredentials` (final result, no token anywhere) → segment renders `[No credentials]` and linesmith falls through to JSONL aggregation per [ADR-0011](../adrs/0011-rate-limit-data-source.md)
- `SubprocessFailed` (Keychain subprocess broken, no file fallback succeeded) → segment renders `[Keychain error]`; JSONL fallback still applies
- `ParseError` / `MissingField` / `EmptyToken` → segment renders `[Credentials corrupt]`; JSONL fallback still applies

All error cases fall through to the JSONL aggregation path before surfacing an error to the user. The error messages render only when JSONL also yields nothing.

### Token rotation

OAuth tokens rotate: Claude Code refreshes them when it hits the Anthropic auth endpoints. Our model:

- Token rotation is invisible to us — we re-read on the next linesmith invocation and get the new token.
- Within a single invocation, the token is frozen: we read it once, use it for the OAuth endpoint call, and forget it when the process exits.
- If the endpoint returns 401 mid-invocation (e.g., token expired literally during the call), we surface `UsageError::Unauthorized` and let JSONL fallback take over.

## Edge cases

- **Keychain locked on macOS** (user has screen lock active): `security` exits non-zero with a specific error code. Treated as `SubprocessFailed`; JSONL fallback still applies.
- **`security` binary missing** (should never happen on macOS, but nerd-sniped by PATH manipulation): `SubprocessFailed` with `ErrorKind::NotFound`.
- **Multi-account: two `Claude Code-credentials<suffix>` entries with identical `mdat`**: fall back to dump order (stable but arbitrary); document in the diagnostic message.
- **`CLAUDE_CONFIG_DIR` set but file doesn't exist**: continue to XDG path. Env var is a hint, not a declaration.
- **`CLAUDE_CONFIG_DIR` set to empty string**: treat as unset (honor the user's probable intent).
- **File readable but `claudeAiOauth` is missing entirely** (e.g., a stale Claude Code version wrote a different shape): `MissingField`. Don't try to recover — the file isn't what we think it is.
- **`accessToken` is a non-string value** (e.g., accidentally a number): serde parse error → `ParseError`.
- **Multiple credentials files across the cascade**: only the first found is read. We don't try to merge or pick "best."
- **File permissions restrict read**: file exists but `fs::read_to_string` returns `PermissionDenied` or similar. Surface as `IoError { path, cause }`, preserving the OS error so `linesmith doctor` can report the actual permission failure.
- **Huge credentials file** (>1MB): unlikely, but cap the read at 1MB to avoid pathological input; surface as `ParseError` if truncated.
- **Symlink pointing at `/dev/urandom` or other non-text**: parse fails on invalid JSON; `ParseError`. We don't try to detect malicious symlinks.

## Testing strategy

- **Unit tests:**
  - Parse valid credentials JSON → success
  - Parse with `accessToken: null` → `EmptyToken`
  - Parse with missing `claudeAiOauth` → `MissingField`
  - Parse invalid JSON → `ParseError`
  - `CLAUDE_CONFIG_DIR` set to existing dir with file → file is read
  - `CLAUDE_CONFIG_DIR` set to non-existing dir → cascade continues
  - XDG path preferred over legacy when both exist

- **Integration tests (macOS only, gated behind `cfg(target_os = "macos")`):**
  - Inject a fake `security` binary on PATH that returns controlled output; assert primary path works
  - Assert multi-account fallback triggers when primary returns empty
  - Assert `mdat` sort order picks the newest entry

- **Fuzz / property tests (nice-to-have):**
  - Feed random bytes to the parser; assert no panics, always an `Err`
  - Feed valid-JSON-but-wrong-shape; assert graceful error

- **Manual test plan (for release):**
  - macOS with Claude Code single-account installed
  - macOS with Claude Code multi-account installed
  - Linux with XDG layout
  - Linux with legacy `~/.claude/` layout
  - Windows (once supported)
  - All of the above with Claude Code not installed → assert `NoCredentials`

## Open questions

- **Windows Credential Manager integration.** v0.1 uses file-based credential reading on Windows. CC stores credentials elsewhere on Windows (DPAPI-encrypted?) — confirmed only when we have a Windows tester. Defer to v0.2 with its own ADR.
- **Token refresh.** Claude Code's OAuth flow may rotate `accessToken` while linesmith is running. v0.1 reads once per process and trusts the endpoint to return 401 if the token is stale. If this produces annoying error flicker for users, revisit in a follow-up.
- **Keychain prompt suppression.** Users running linesmith from a fresh install see a macOS permission prompt. If we later distribute a signed binary, the signature lets users grant access once via "Always Allow." Until then, users must click through. Document in README.
- **Redaction strategy.** `SecretString` + `Debug` redacting is the first line of defense, but runtime logs (tracing spans) could still leak via `format!` outside our control. Audit all debug logging paths when the fetcher is implemented.
- **File permission hardening.** Should we refuse to read `.credentials.json` if its mode is world-readable (Unix)? Claude Code itself writes mode 600, so the file should already be private. Hardening this check is low-priority until we see a case where it matters.

## Change log

- 2026-04-19: initial draft (v0.1). Defines resolution cascade (macOS Keychain + multi-account fallback + Linux/Windows file cascade with XDG support), `Credentials` and `CredentialError` types with `SecretString` redaction, integration contract with `DataContext`, and failure-mode taxonomy that differentiates `NoCredentials` / `SubprocessFailed` / `IoError` / `ParseError` / `MissingField` / `EmptyToken`. Driven by ADR-0010 + ADR-0011.
