//! Stub error types for lazy [`DataContext`](super::DataContext) sources.
//!
//! Each source's real error variants land with its owning epic (e.g.
//! `UsageError` gains real variants in lsm-y6m; `GitError` in lsm-8jl).
//! For now every accessor returns the `NotImplemented` variant so the
//! plugin runtime can expose a uniform error surface to scripts.
//!
//! **`NotImplemented` is temporary.** When an epic lands real variants
//! for a given error enum, the `NotImplemented` variant is removed in
//! the same commit. Because each enum is `#[non_exhaustive]`, adding
//! new variants is non-breaking; *removing* `NotImplemented` is a
//! breaking change for any external code that matches on it. v0.1
//! treats that window as acceptable — no external consumers exist
//! yet, and the stub's whole purpose is to signal "not wired up."

macro_rules! stub_error {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq)]
        #[non_exhaustive]
        pub enum $name {
            /// Source not yet implemented. Real variants land with the
            /// epic that owns this source.
            NotImplemented,
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    Self::NotImplemented => f.write_str("NotImplemented"),
                }
            }
        }

        impl std::error::Error for $name {}
    };
}

stub_error!(
    SettingsError,
    "Errors from reading `~/.claude/settings.json` + overlays."
);
stub_error!(ClaudeJsonError, "Errors from reading `~/.claude.json`.");
stub_error!(JsonlError, "Errors from aggregating JSONL transcripts.");
stub_error!(
    UsageError,
    "Errors from the OAuth usage endpoint + cache stack."
);
stub_error!(
    CredentialError,
    "Errors from macOS Keychain / `.credentials.json` reads."
);
stub_error!(SessionError, "Errors from the live sessions directory.");
stub_error!(GitError, "Errors from `gix` repo inspection.");
