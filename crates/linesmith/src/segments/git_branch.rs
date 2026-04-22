//! `git_branch` segment: branch name + dirty indicator.
//!
//! Canonical definition: `docs/specs/git-segments.md`.
//!
//! Hidden when cwd is outside a git repo, when the repo is bare, or
//! when gix rejects the repo. Detached HEAD renders a short SHA;
//! unborn HEAD renders the symbolic-ref target (e.g. `main`).

use std::collections::BTreeMap;

use super::rate_limit_format::parse_bool;
use super::{RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::{DataContext, DataDep, GitContext, Head, RepoKind};
use crate::theme::Role;

#[derive(Default)]
pub struct GitBranchSegment {
    cfg: Config,
}

/// Between workspace (16) and model (64): branch identity is more
/// valuable than cost/effort under width pressure but less than
/// model identity when both are set.
const PRIORITY: u8 = 48;

const ID: &str = "git_branch";
const DEFAULT_DIRTY_INDICATOR: &str = "*";
const DEFAULT_TRUNCATION_MARKER: &str = "…";
const DEFAULT_SHORT_SHA_LEN: u8 = 7;
const DEFAULT_MAX_BRANCH_LEN: u16 = 40;
const DEFAULT_AHEAD_FORMAT: &str = "↑{n}";
const DEFAULT_BEHIND_FORMAT: &str = "↓{n}";
const NO_UPSTREAM_MARKER: &str = "?";

/// Resolved runtime config. Defaults match `git-segments.md`
/// §Config schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    pub(crate) icon: String,
    pub(crate) label: String,
    pub(crate) max_length: u16,
    pub(crate) truncation_marker: String,
    pub(crate) short_sha_length: u8,
    pub(crate) dirty_enabled: bool,
    pub(crate) dirty_indicator: String,
    pub(crate) clean_indicator: String,
    pub(crate) ahead_behind: AheadBehindConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) struct AheadBehindConfig {
    pub(crate) enabled: bool,
    pub(crate) ahead_format: FormatTemplate,
    pub(crate) behind_format: FormatTemplate,
    pub(crate) hide_when_zero: bool,
    pub(crate) hide_when_no_upstream: bool,
}

impl Default for AheadBehindConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ahead_format: FormatTemplate::parse(DEFAULT_AHEAD_FORMAT)
                .expect("DEFAULT_AHEAD_FORMAT must contain FormatTemplate::PLACEHOLDER"),
            behind_format: FormatTemplate::parse(DEFAULT_BEHIND_FORMAT)
                .expect("DEFAULT_BEHIND_FORMAT must contain FormatTemplate::PLACEHOLDER"),
            hide_when_zero: true,
            hide_when_no_upstream: true,
        }
    }
}

/// Template string for ahead/behind rendering. Constructor guarantees
/// [`Self::PLACEHOLDER`] is present, so a typo like `↑{count}`
/// surfaces at config-parse time rather than silently rendering with
/// no count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FormatTemplate(String);

impl FormatTemplate {
    /// The count placeholder every template must contain.
    pub(crate) const PLACEHOLDER: &'static str = "{n}";

    /// Parse a user-supplied template. Returns `None` when the
    /// placeholder is missing.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        if s.contains(Self::PLACEHOLDER) {
            Some(Self(s.to_string()))
        } else {
            None
        }
    }

    pub(crate) fn render(&self, n: u32) -> String {
        self.0.replace(Self::PLACEHOLDER, &n.to_string())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            icon: String::new(),
            label: String::new(),
            max_length: DEFAULT_MAX_BRANCH_LEN,
            truncation_marker: DEFAULT_TRUNCATION_MARKER.into(),
            short_sha_length: DEFAULT_SHORT_SHA_LEN,
            dirty_enabled: true,
            dirty_indicator: DEFAULT_DIRTY_INDICATOR.into(),
            clean_indicator: String::new(),
            ahead_behind: AheadBehindConfig::default(),
        }
    }
}

impl GitBranchSegment {
    /// Parse the `[segments.git_branch]` extras bag. Unknown values
    /// warn and fall back to defaults. `dirty.format = "counts"`
    /// warns and falls back to "indicator".
    pub fn from_extras(
        extras: &BTreeMap<String, toml::Value>,
        warn: &mut impl FnMut(&str),
    ) -> Self {
        let mut cfg = Config::default();

        if let Some(v) = extras.get("icon").and_then(|v| v.as_str()) {
            cfg.icon = v.to_string();
        }
        if let Some(v) = extras.get("label").and_then(|v| v.as_str()) {
            cfg.label = v.to_string();
        }
        if let Some(v) = extras.get("max_length") {
            match v.as_integer().and_then(|n| u16::try_from(n).ok()) {
                // Spec min is 1; 0 would render nothing useful.
                Some(n) if n >= 1 => cfg.max_length = n,
                _ => warn(&format!(
                    "segments.{ID}.max_length: expected 1..=65535; ignoring"
                )),
            }
        }
        if let Some(v) = extras.get("truncation_marker").and_then(|v| v.as_str()) {
            cfg.truncation_marker = v.to_string();
        }
        if let Some(v) = extras.get("short_sha_length").and_then(|v| v.as_integer()) {
            match u8::try_from(v) {
                // Spec allows 1..=40; clamp to u8 and cap at 40.
                Ok(n) if (1..=40).contains(&n) => cfg.short_sha_length = n,
                _ => warn(&format!(
                    "segments.{ID}.short_sha_length: expected 1..=40; ignoring"
                )),
            }
        }

        if let Some(dirty) = extras.get("dirty").and_then(|v| v.as_table()) {
            let dirty_map: BTreeMap<String, toml::Value> =
                dirty.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            if let Some(v) = parse_bool(&dirty_map, "enabled", "git_branch.dirty", warn) {
                cfg.dirty_enabled = v;
            }
            if let Some(fmt) = dirty_map.get("format").and_then(|v| v.as_str()) {
                match fmt {
                    "indicator" | "hidden" => {
                        if fmt == "hidden" {
                            cfg.dirty_enabled = false;
                        }
                    }
                    "counts" => {
                        warn("segments.git_branch.dirty.format=\"counts\" is not yet implemented; falling back to \"indicator\" (follow-up: lsm-kjj counts mode)");
                    }
                    _ => warn(
                        "segments.git_branch.dirty.format: expected \"indicator\"|\"counts\"|\"hidden\"; ignoring",
                    ),
                }
            }
            if let Some(v) = dirty_map.get("indicator").and_then(|v| v.as_str()) {
                cfg.dirty_indicator = v.to_string();
            }
            if let Some(v) = dirty_map.get("clean_indicator").and_then(|v| v.as_str()) {
                cfg.clean_indicator = v.to_string();
            }
        }

        if let Some(ab) = extras.get("ahead_behind").and_then(|v| v.as_table()) {
            let ab_map: BTreeMap<String, toml::Value> =
                ab.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            if let Some(v) = parse_bool(&ab_map, "enabled", "git_branch.ahead_behind", warn) {
                cfg.ahead_behind.enabled = v;
            }
            let placeholder = FormatTemplate::PLACEHOLDER;
            if let Some(v) = ab_map.get("ahead_format").and_then(|v| v.as_str()) {
                match FormatTemplate::parse(v) {
                    Some(tpl) => cfg.ahead_behind.ahead_format = tpl,
                    None => warn(&format!(
                        "segments.{ID}.ahead_behind.ahead_format: missing `{placeholder}` placeholder in {v:?}; ignoring"
                    )),
                }
            }
            if let Some(v) = ab_map.get("behind_format").and_then(|v| v.as_str()) {
                match FormatTemplate::parse(v) {
                    Some(tpl) => cfg.ahead_behind.behind_format = tpl,
                    None => warn(&format!(
                        "segments.{ID}.ahead_behind.behind_format: missing `{placeholder}` placeholder in {v:?}; ignoring"
                    )),
                }
            }
            if let Some(v) = parse_bool(&ab_map, "hide_when_zero", "git_branch.ahead_behind", warn)
            {
                cfg.ahead_behind.hide_when_zero = v;
            }
            if let Some(v) = parse_bool(
                &ab_map,
                "hide_when_no_upstream",
                "git_branch.ahead_behind",
                warn,
            ) {
                cfg.ahead_behind.hide_when_no_upstream = v;
            }
        }

        Self { cfg }
    }
}

impl Segment for GitBranchSegment {
    fn data_deps(&self) -> &'static [DataDep] {
        &[DataDep::Git]
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }

    fn render(&self, ctx: &DataContext) -> RenderResult {
        let arc = ctx.git();
        match &*arc {
            Err(_) | Ok(None) => Ok(None),
            // Bare repos have no working tree, so branch / dirty
            // state is meaningless. Submodules, linked worktrees, and
            // main checkouts all render normally.
            Ok(Some(gc)) if matches!(gc.repo_kind, RepoKind::Bare) => Ok(None),
            Ok(Some(gc)) => {
                let text = self.assemble(gc);
                if text.is_empty() {
                    return Ok(None);
                }
                Ok(Some(RenderedSegment::new(text).with_role(Role::Accent)))
            }
        }
    }
}

impl GitBranchSegment {
    fn assemble(&self, gc: &GitContext) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.cfg.icon.is_empty() {
            parts.push(self.cfg.icon.clone());
        }
        if !self.cfg.label.is_empty() {
            parts.push(self.cfg.label.clone());
        }

        let head = self.render_head(&gc.head);
        if !head.is_empty() {
            parts.push(head);
        }

        if self.cfg.dirty_enabled {
            if let Some(marker) = self.render_dirty(gc) {
                parts.push(marker);
            }
        }

        if self.cfg.ahead_behind.enabled {
            if let Some(marker) = self.render_ahead_behind(gc) {
                parts.push(marker);
            }
        }

        parts.join(" ")
    }

    fn render_ahead_behind(&self, gc: &GitContext) -> Option<String> {
        // Ahead/behind only applies to local branches. Detached /
        // Unborn / OtherRef skip the marker entirely — the `?` that
        // `hide_when_no_upstream = false` emits is reserved for a
        // branch whose tracking remote is unconfigured.
        if !matches!(gc.head, Head::Branch(_)) {
            return None;
        }
        match &*gc.upstream() {
            None => {
                if self.cfg.ahead_behind.hide_when_no_upstream {
                    None
                } else {
                    Some(NO_UPSTREAM_MARKER.to_string())
                }
            }
            Some(state) => {
                if state.ahead == 0 && state.behind == 0 && self.cfg.ahead_behind.hide_when_zero {
                    return None;
                }
                let mut out = String::new();
                if state.ahead > 0 || !self.cfg.ahead_behind.hide_when_zero {
                    out.push_str(&self.cfg.ahead_behind.ahead_format.render(state.ahead));
                }
                if state.behind > 0 || !self.cfg.ahead_behind.hide_when_zero {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(&self.cfg.ahead_behind.behind_format.render(state.behind));
                }
                if out.is_empty() {
                    None
                } else {
                    Some(out)
                }
            }
        }
    }

    fn render_head(&self, head: &Head) -> String {
        match head {
            Head::Branch(name) => {
                truncate_middle(name, self.cfg.max_length, &self.cfg.truncation_marker)
            }
            Head::Detached(oid) => {
                let s = oid.to_string();
                let n = usize::from(self.cfg.short_sha_length).min(s.len());
                format!("({})", &s[..n])
            }
            Head::Unborn { symbolic_ref } => truncate_middle(
                symbolic_ref,
                self.cfg.max_length,
                &self.cfg.truncation_marker,
            ),
            Head::OtherRef { full_name } => {
                truncate_middle(full_name, self.cfg.max_length, &self.cfg.truncation_marker)
            }
        }
    }

    fn render_dirty(&self, gc: &GitContext) -> Option<String> {
        if gc.dirty().is_dirty() {
            if self.cfg.dirty_indicator.is_empty() {
                None
            } else {
                Some(self.cfg.dirty_indicator.clone())
            }
        } else if self.cfg.clean_indicator.is_empty() {
            None
        } else {
            Some(self.cfg.clean_indicator.clone())
        }
    }
}

/// Middle-truncate `s` so its cell width fits `max`, inserting
/// `marker` between the kept prefix and suffix. Grapheme-aware per
/// segment-system.md §Layout intent. Falls through for short inputs.
fn truncate_middle(s: &str, max: u16, marker: &str) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    let max_usize = usize::from(max);
    let cur_width = UnicodeWidthStr::width(s);
    if max == 0 || cur_width <= max_usize {
        return s.to_string();
    }
    let marker_width = UnicodeWidthStr::width(marker);
    if marker_width >= max_usize {
        // Pathological: marker alone exceeds the budget. Keep the
        // first `max` graphemes of the source — degraded but stable.
        let mut out = String::new();
        let mut w = 0usize;
        for g in s.graphemes(true) {
            let gw = UnicodeWidthStr::width(g);
            if w + gw > max_usize {
                break;
            }
            out.push_str(g);
            w += gw;
        }
        return out;
    }
    let budget = max_usize - marker_width;
    let head_budget = budget.div_ceil(2);
    let tail_budget = budget - head_budget;

    let mut head = String::new();
    let mut head_w = 0usize;
    for g in s.graphemes(true) {
        let gw = UnicodeWidthStr::width(g);
        if head_w + gw > head_budget {
            break;
        }
        head.push_str(g);
        head_w += gw;
    }
    let mut tail_graphemes: Vec<&str> = Vec::new();
    let mut tail_w = 0usize;
    for g in s.graphemes(true).rev() {
        let gw = UnicodeWidthStr::width(g);
        if tail_w + gw > tail_budget {
            break;
        }
        tail_graphemes.push(g);
        tail_w += gw;
    }
    tail_graphemes.reverse();
    let mut out = head;
    out.push_str(marker);
    for g in tail_graphemes {
        out.push_str(g);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_context::{GitContext, Head, RepoKind, UpstreamState};
    use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn minimal_status() -> StatusContext {
        StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "Claude".into(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            },
            context_window: None,
            cost: None,
            effort: None,
            raw: Arc::new(serde_json::Value::Null),
        }
    }

    fn ctx_with_git(
        result: Result<Option<GitContext>, crate::data_context::GitError>,
    ) -> DataContext {
        let dc = DataContext::with_cwd(minimal_status(), None);
        dc.preseed_git(result).expect("seed");
        dc
    }

    #[test]
    fn hides_when_not_in_repo() {
        assert!(GitBranchSegment::default()
            .render(&ctx_with_git(Ok(None)))
            .unwrap()
            .is_none());
    }

    #[test]
    fn hides_on_gix_error() {
        let err = crate::data_context::GitError::CorruptRepo {
            path: PathBuf::from("/x"),
            message: "synthetic".into(),
        };
        assert!(GitBranchSegment::default()
            .render(&ctx_with_git(Err(err)))
            .unwrap()
            .is_none());
    }

    #[test]
    fn hides_on_bare_repo() {
        let gc = GitContext::new(
            RepoKind::Bare,
            PathBuf::from("/tmp/bare.git"),
            Head::Unborn {
                symbolic_ref: "main".into(),
            },
        );
        assert!(GitBranchSegment::default()
            .render(&ctx_with_git(Ok(Some(gc))))
            .unwrap()
            .is_none());
    }

    #[test]
    fn renders_branch_name() {
        let gc = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/repo/.git"),
            Head::Branch("main".into()),
        );
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_git(Ok(Some(gc))))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main");
        assert_eq!(rendered.style().role, Some(Role::Accent));
    }

    #[test]
    fn renders_detached_as_short_sha_in_parens() {
        let gc = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/repo/.git"),
            Head::Detached(gix::ObjectId::empty_tree(gix::hash::Kind::Sha1)),
        );
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_git(Ok(Some(gc))))
            .unwrap()
            .expect("rendered");
        // gix's canonical empty-tree SHA starts with "4b825dc6" — we
        // assert the shape (parens + configured length) rather than
        // the exact bytes so the test survives gix's hash changes.
        assert!(rendered.text().starts_with('('));
        assert!(rendered.text().ends_with(')'));
        // `(` + 7 hex + `)` = 9 cells.
        assert_eq!(rendered.text().chars().count(), 9);
    }

    #[test]
    fn renders_other_ref_full_name() {
        let gc = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/repo/.git"),
            Head::OtherRef {
                full_name: "refs/remotes/origin/feature".into(),
            },
        );
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_git(Ok(Some(gc))))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "refs/remotes/origin/feature");
    }

    // --- Ahead/behind rendering ---

    fn ctx_with_upstream(head: Head, upstream: Option<UpstreamState>) -> DataContext {
        let gc = GitContext::new(RepoKind::Main, PathBuf::from("/repo/.git"), head);
        gc.preseed_upstream(upstream).expect("fresh onceCell");
        let dc = DataContext::with_cwd(minimal_status(), None);
        dc.preseed_git(Ok(Some(gc))).expect("seed");
        dc
    }

    #[test]
    fn renders_ahead_when_local_leads() {
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_upstream(
                Head::Branch("main".into()),
                Some(UpstreamState {
                    ahead: 2,
                    behind: 0,
                    upstream_branch: "origin/main".into(),
                }),
            ))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main ↑2");
    }

    #[test]
    fn renders_behind_when_remote_leads() {
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_upstream(
                Head::Branch("main".into()),
                Some(UpstreamState {
                    ahead: 0,
                    behind: 3,
                    upstream_branch: "origin/main".into(),
                }),
            ))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main ↓3");
    }

    #[test]
    fn renders_both_when_diverged() {
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_upstream(
                Head::Branch("main".into()),
                Some(UpstreamState {
                    ahead: 2,
                    behind: 3,
                    upstream_branch: "origin/main".into(),
                }),
            ))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main ↑2 ↓3");
    }

    #[test]
    fn hides_ahead_behind_when_zero_by_default() {
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_upstream(
                Head::Branch("main".into()),
                Some(UpstreamState {
                    ahead: 0,
                    behind: 0,
                    upstream_branch: "origin/main".into(),
                }),
            ))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main");
    }

    #[test]
    fn shows_zeros_when_configured() {
        let mut seg = GitBranchSegment::default();
        seg.cfg.ahead_behind.hide_when_zero = false;
        let rendered = seg
            .render(&ctx_with_upstream(
                Head::Branch("main".into()),
                Some(UpstreamState {
                    ahead: 0,
                    behind: 0,
                    upstream_branch: "origin/main".into(),
                }),
            ))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main ↑0 ↓0");
    }

    #[test]
    fn hides_ahead_behind_when_no_upstream_by_default() {
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_upstream(Head::Branch("main".into()), None))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main");
    }

    #[test]
    fn renders_question_mark_when_no_upstream_opted_in() {
        let mut seg = GitBranchSegment::default();
        seg.cfg.ahead_behind.hide_when_no_upstream = false;
        let rendered = seg
            .render(&ctx_with_upstream(Head::Branch("main".into()), None))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main ?");
    }

    #[test]
    fn skips_ahead_behind_on_detached_head() {
        let gc = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/repo/.git"),
            Head::Detached(gix::ObjectId::empty_tree(gix::hash::Kind::Sha1)),
        );
        let dc = DataContext::with_cwd(minimal_status(), None);
        dc.preseed_git(Ok(Some(gc))).expect("seed");
        let rendered = GitBranchSegment::default()
            .render(&dc)
            .unwrap()
            .expect("rendered");
        assert!(
            !rendered.text().contains('↑') && !rendered.text().contains('↓'),
            "expected no ahead/behind on detached HEAD, got {:?}",
            rendered.text()
        );
    }

    #[test]
    fn from_extras_warns_on_ahead_format_missing_placeholder() {
        let mut extras = BTreeMap::new();
        let mut ab = toml::value::Table::new();
        ab.insert(
            "ahead_format".into(),
            toml::Value::String("↑{count}".into()),
        );
        extras.insert("ahead_behind".into(), toml::Value::Table(ab));
        let mut warnings = Vec::<String>::new();
        let seg = GitBranchSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("ahead_format"));
        assert!(warnings[0].contains("{n}"));
        assert_eq!(seg.cfg.ahead_behind.ahead_format.render(2), "↑2");
    }

    #[test]
    fn from_extras_warns_on_behind_format_missing_placeholder() {
        let mut extras = BTreeMap::new();
        let mut ab = toml::value::Table::new();
        ab.insert(
            "behind_format".into(),
            toml::Value::String("↓{count}".into()),
        );
        extras.insert("ahead_behind".into(), toml::Value::Table(ab));
        let mut warnings = Vec::<String>::new();
        let seg = GitBranchSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("behind_format"));
        assert!(warnings[0].contains("{n}"));
        assert_eq!(seg.cfg.ahead_behind.behind_format.render(3), "↓3");
    }

    #[test]
    fn format_template_parse_rejects_missing_placeholder() {
        assert!(FormatTemplate::parse("no placeholder").is_none());
        assert!(FormatTemplate::parse("↑{count}").is_none());
        assert!(FormatTemplate::parse("↑{n}").is_some());
    }

    #[test]
    fn format_template_render_substitutes_placeholder() {
        let tpl = FormatTemplate::parse("↑{n} commits").expect("valid");
        assert_eq!(tpl.render(7), "↑7 commits");
    }

    #[test]
    fn default_templates_contain_placeholder() {
        // AheadBehindConfig::default() panics if the module-private
        // DEFAULT_*_FORMAT consts drift away from FormatTemplate's
        // PLACEHOLDER contract. Pin the default build in CI so the
        // expect() in Default is proven at least once.
        let default = AheadBehindConfig::default();
        assert_eq!(default.ahead_format.render(2), "↑2");
        assert_eq!(default.behind_format.render(3), "↓3");
    }

    #[test]
    fn skips_ahead_behind_on_unborn_head() {
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_upstream(
                Head::Unborn {
                    symbolic_ref: "main".into(),
                },
                None,
            ))
            .unwrap()
            .expect("rendered");
        assert!(
            !rendered.text().contains('↑')
                && !rendered.text().contains('↓')
                && !rendered.text().contains('?'),
            "expected no ahead/behind marker on Unborn HEAD, got {:?}",
            rendered.text()
        );
    }

    #[test]
    fn skips_ahead_behind_on_other_ref_head() {
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_upstream(
                Head::OtherRef {
                    full_name: "refs/remotes/origin/feature".into(),
                },
                None,
            ))
            .unwrap()
            .expect("rendered");
        assert!(
            !rendered.text().contains('↑')
                && !rendered.text().contains('↓')
                && !rendered.text().contains('?'),
            "expected no ahead/behind marker on OtherRef HEAD, got {:?}",
            rendered.text()
        );
    }

    #[test]
    fn skips_ahead_behind_on_unborn_head_even_with_hide_when_no_upstream_false() {
        // The '?' marker is reserved for branches with no configured
        // tracking remote. Unborn HEAD isn't a branch, so it must not
        // render '?' regardless of hide_when_no_upstream.
        let mut seg = GitBranchSegment::default();
        seg.cfg.ahead_behind.hide_when_no_upstream = false;
        let rendered = seg
            .render(&ctx_with_upstream(
                Head::Unborn {
                    symbolic_ref: "main".into(),
                },
                None,
            ))
            .unwrap()
            .expect("rendered");
        assert!(
            !rendered.text().contains('?'),
            "Unborn HEAD should not render '?' even with hide_when_no_upstream=false; got {:?}",
            rendered.text()
        );
    }

    #[test]
    fn renders_ahead_with_custom_format() {
        let mut seg = GitBranchSegment::default();
        seg.cfg.ahead_behind.ahead_format = FormatTemplate::parse(">>{n}").expect("valid");
        let rendered = seg
            .render(&ctx_with_upstream(
                Head::Branch("main".into()),
                Some(UpstreamState {
                    ahead: 5,
                    behind: 0,
                    upstream_branch: "origin/main".into(),
                }),
            ))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main >>5");
    }

    #[test]
    fn from_extras_reads_ahead_behind_knobs() {
        let mut extras = BTreeMap::new();
        let mut ab = toml::value::Table::new();
        ab.insert("enabled".into(), toml::Value::Boolean(true));
        ab.insert("ahead_format".into(), toml::Value::String(">>{n}".into()));
        ab.insert("behind_format".into(), toml::Value::String("<<{n}".into()));
        ab.insert("hide_when_zero".into(), toml::Value::Boolean(false));
        ab.insert("hide_when_no_upstream".into(), toml::Value::Boolean(false));
        extras.insert("ahead_behind".into(), toml::Value::Table(ab));
        let mut warnings = Vec::<String>::new();
        let seg = GitBranchSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(seg.cfg.ahead_behind.enabled);
        assert_eq!(seg.cfg.ahead_behind.ahead_format.render(3), ">>3");
        assert_eq!(seg.cfg.ahead_behind.behind_format.render(5), "<<5");
        assert!(!seg.cfg.ahead_behind.hide_when_zero);
        assert!(!seg.cfg.ahead_behind.hide_when_no_upstream);
    }

    #[test]
    fn renders_submodule_like_main() {
        let gc = GitContext::new(
            RepoKind::Submodule,
            PathBuf::from("/parent/.git/modules/child"),
            Head::Branch("main".into()),
        );
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_git(Ok(Some(gc))))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main");
    }

    #[test]
    fn from_extras_rejects_max_length_zero() {
        let mut extras = BTreeMap::new();
        extras.insert("max_length".into(), toml::Value::Integer(0));
        let mut warnings = Vec::<String>::new();
        let seg = GitBranchSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("max_length"));
        assert_eq!(seg.cfg.max_length, DEFAULT_MAX_BRANCH_LEN);
    }

    #[test]
    fn from_extras_rejects_max_length_wrong_type() {
        let mut extras = BTreeMap::new();
        extras.insert("max_length".into(), toml::Value::String("wide".into()));
        let mut warnings = Vec::<String>::new();
        let seg = GitBranchSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("max_length"));
        assert_eq!(seg.cfg.max_length, DEFAULT_MAX_BRANCH_LEN);
    }

    #[test]
    fn renders_unborn_as_symbolic_ref_name() {
        let gc = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/repo/.git"),
            Head::Unborn {
                symbolic_ref: "master".into(),
            },
        );
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_git(Ok(Some(gc))))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "master");
    }

    #[test]
    fn applies_icon_and_label_when_configured() {
        let mut seg = GitBranchSegment::default();
        seg.cfg.icon = ">>".into();
        seg.cfg.label = "branch:".into();
        let gc = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/repo/.git"),
            Head::Branch("main".into()),
        );
        let rendered = seg
            .render(&ctx_with_git(Ok(Some(gc))))
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), ">> branch: main");
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(GitBranchSegment::default().defaults().priority, PRIORITY);
    }

    #[test]
    fn declares_git_data_dep() {
        assert_eq!(GitBranchSegment::default().data_deps(), &[DataDep::Git]);
    }

    #[test]
    fn from_extras_reads_icon_label_and_dirty_knobs() {
        let mut extras = BTreeMap::new();
        extras.insert("icon".into(), toml::Value::String("".into()));
        extras.insert("label".into(), toml::Value::String("br".into()));
        extras.insert("max_length".into(), toml::Value::Integer(10));
        extras.insert("truncation_marker".into(), toml::Value::String("..".into()));
        extras.insert("short_sha_length".into(), toml::Value::Integer(12));

        let mut dirty = toml::value::Table::new();
        dirty.insert("enabled".into(), toml::Value::Boolean(true));
        dirty.insert("format".into(), toml::Value::String("indicator".into()));
        dirty.insert("indicator".into(), toml::Value::String("●".into()));
        dirty.insert("clean_indicator".into(), toml::Value::String("✓".into()));
        extras.insert("dirty".into(), toml::Value::Table(dirty));

        let mut warnings = Vec::<String>::new();
        let seg = GitBranchSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(seg.cfg.icon, "");
        assert_eq!(seg.cfg.label, "br");
        assert_eq!(seg.cfg.max_length, 10);
        assert_eq!(seg.cfg.truncation_marker, "..");
        assert_eq!(seg.cfg.short_sha_length, 12);
        assert!(seg.cfg.dirty_enabled);
        assert_eq!(seg.cfg.dirty_indicator, "●");
        assert_eq!(seg.cfg.clean_indicator, "✓");
    }

    #[test]
    fn from_extras_counts_mode_warns_and_falls_back_to_indicator() {
        let mut extras = BTreeMap::new();
        let mut dirty = toml::value::Table::new();
        dirty.insert("format".into(), toml::Value::String("counts".into()));
        extras.insert("dirty".into(), toml::Value::Table(dirty));

        let mut warnings = Vec::<String>::new();
        let seg = GitBranchSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("counts"));
        assert!(seg.cfg.dirty_enabled);
    }

    #[test]
    fn from_extras_hidden_format_turns_dirty_off() {
        let mut extras = BTreeMap::new();
        let mut dirty = toml::value::Table::new();
        dirty.insert("format".into(), toml::Value::String("hidden".into()));
        extras.insert("dirty".into(), toml::Value::Table(dirty));

        let mut warnings = Vec::<String>::new();
        let seg = GitBranchSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert!(warnings.is_empty());
        assert!(!seg.cfg.dirty_enabled);
    }

    #[test]
    fn from_extras_rejects_short_sha_length_out_of_range() {
        for bad in [0i64, 41, -5, 999] {
            let mut extras = BTreeMap::new();
            extras.insert("short_sha_length".into(), toml::Value::Integer(bad));
            let mut warnings = Vec::<String>::new();
            let seg = GitBranchSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
            assert_eq!(warnings.len(), 1, "{bad}: {warnings:?}");
            assert_eq!(seg.cfg.short_sha_length, DEFAULT_SHORT_SHA_LEN);
        }
    }

    // --- truncate_middle ---

    #[test]
    fn truncate_middle_keeps_short_strings_verbatim() {
        assert_eq!(truncate_middle("main", 10, "…"), "main");
        assert_eq!(truncate_middle("feature/x", 9, "…"), "feature/x");
    }

    #[test]
    fn truncate_middle_preserves_prefix_and_suffix() {
        // "feature/authentication-v3" is 25 cells; budget 10 → 9
        // available chars → 5-head + marker + 4-tail (approx).
        let out = truncate_middle("feature/authentication-v3", 10, "…");
        assert!(out.contains('…'));
        assert!(out.len() <= 25);
        assert!(out.starts_with("feat"), "expected prefix kept, got {out}");
        assert!(out.ends_with("-v3") || out.ends_with("v3"));
    }

    #[test]
    fn truncate_middle_handles_zero_budget() {
        assert_eq!(truncate_middle("main", 0, "…"), "main");
    }

    #[test]
    fn truncate_middle_degrades_when_marker_exceeds_budget() {
        // marker "[truncated]" is wider than max=3 cells. Falls back
        // to keeping the first `max` graphemes.
        assert_eq!(truncate_middle("hello-world", 3, "[truncated]"), "hel");
    }
}
