//! [`DataDep`] enumerates the data sources a segment may declare via
//! [`Segment::data_deps`](crate::segments::Segment::data_deps).
//!
//! Canonical definition: `docs/specs/data-fetching.md` §Segment
//! dependency declaration.

/// Which data source on [`DataContext`](super::DataContext) a segment reads.
///
/// Segments return `&'static [DataDep]` from `data_deps()`. The runtime
/// unions the declared deps across all enabled segments and only
/// lazy-fetches sources that some segment needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DataDep {
    /// Parsed stdin payload. Always populated eagerly; listing is optional.
    Status,
    /// `~/.claude/settings.json` + overlays.
    Settings,
    /// `~/.claude.json` per-user state.
    ClaudeJson,
    /// JSONL transcripts under `~/.claude/projects/**`.
    Jsonl,
    /// OAuth `/api/oauth/usage` endpoint + cache.
    Usage,
    /// macOS Keychain / `.credentials.json` — not plugin-accessible
    /// (reserved in `plugin-api.md` §@data_deps header syntax).
    Credentials,
    /// `~/.claude/sessions/{pid}.json` live process directory.
    Sessions,
    /// Git repo state via `gix`.
    Git,
}

impl DataDep {
    /// The lowercase-token name used in the plugin-api `@data_deps`
    /// header (`plugin-api.md` §@data_deps header syntax) and in log /
    /// doctor output. Single source of truth for the wire name.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Settings => "settings",
            Self::ClaudeJson => "claude_json",
            Self::Jsonl => "jsonl",
            Self::Usage => "usage",
            Self::Credentials => "credentials",
            Self::Sessions => "sessions",
            Self::Git => "git",
        }
    }
}

impl std::fmt::Display for DataDep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
