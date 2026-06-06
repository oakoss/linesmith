//! `git_branch` segment: branch name + dirty indicator.
//!
//! Canonical definition: `docs/specs/git-segments.md`.
//!
//! Hidden when cwd is outside a git repo, when the repo is bare, or
//! when gix rejects the repo. Detached HEAD renders a short SHA;
//! unborn HEAD renders the symbolic-ref target (e.g. `main`).

use std::collections::BTreeMap;

use super::extras::parse_bool;
use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
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
    pub(crate) label: String,
    pub(crate) max_length: u16,
    pub(crate) truncation_marker: String,
    pub(crate) short_sha_length: u8,
    pub(crate) dirty_enabled: bool,
    pub(crate) dirty_indicator: String,
    pub(crate) clean_indicator: String,
    /// Hide the dirty marker when `rc.terminal_width` is below this
    /// threshold. `0` = never auto-hide.
    pub(crate) dirty_hide_below_cells: u16,
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
    /// Hide the ahead/behind marker when `rc.terminal_width` is below
    /// this threshold. `0` = never auto-hide.
    pub(crate) hide_below_cells: u16,
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
            hide_below_cells: 0,
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
            label: String::new(),
            max_length: DEFAULT_MAX_BRANCH_LEN,
            truncation_marker: DEFAULT_TRUNCATION_MARKER.into(),
            short_sha_length: DEFAULT_SHORT_SHA_LEN,
            dirty_enabled: true,
            dirty_indicator: DEFAULT_DIRTY_INDICATOR.into(),
            clean_indicator: String::new(),
            dirty_hide_below_cells: 0,
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
            if let Some(v) = parse_hide_below_cells(&dirty_map, "git_branch.dirty", warn) {
                cfg.dirty_hide_below_cells = v;
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
            if let Some(v) = parse_hide_below_cells(&ab_map, "git_branch.ahead_behind", warn) {
                cfg.ahead_behind.hide_below_cells = v;
            }
        }

        Self { cfg }
    }
}

/// `true` when the configured threshold is set and the current
/// terminal width is below it. Threshold `0` is the sentinel for
/// "never auto-hide" — the marker shows at every terminal width.
fn is_below_threshold(rc: &RenderContext, threshold: u16) -> bool {
    threshold > 0 && rc.terminal_width < threshold
}

/// Parse a `hide_below_cells` field as a `u16` cell threshold. Returns
/// `Some(n)` for a valid `u16`, `None` on missing key (silent), and
/// `None` plus a warning on a malformed value — leaving the caller's
/// existing threshold untouched rather than clearing it on a typo.
fn parse_hide_below_cells(
    table: &BTreeMap<String, toml::Value>,
    scope: &str,
    warn: &mut impl FnMut(&str),
) -> Option<u16> {
    let v = table.get("hide_below_cells")?;
    match v.as_integer().and_then(|n| u16::try_from(n).ok()) {
        Some(n) => Some(n),
        None => {
            warn(&format!(
                "segments.{scope}.hide_below_cells: expected 0..=65535; ignoring"
            ));
            None
        }
    }
}

impl Segment for GitBranchSegment {
    fn data_deps(&self) -> &'static [DataDep] {
        &[DataDep::Git]
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY).with_icon("\u{f126}")
    }

    fn render(&self, ctx: &DataContext, rc: &RenderContext) -> RenderResult {
        let arc = ctx.git();
        match &*arc {
            Err(_) | Ok(None) => Ok(None),
            // Bare repos have no working tree, so branch / dirty
            // state is meaningless. Submodules, linked worktrees, and
            // main checkouts all render normally.
            Ok(Some(gc)) if matches!(gc.repo_kind, RepoKind::Bare) => {
                crate::lsm_debug!("git_branch: bare repo; hiding");
                Ok(None)
            }
            Ok(Some(gc)) => {
                let text = self.assemble(gc, rc);
                if text.is_empty() {
                    return Ok(None);
                }
                Ok(Some(RenderedSegment::new(text).with_role(Role::Accent)))
            }
        }
    }

    fn shrink_to_fit(
        &self,
        ctx: &DataContext,
        _rc: &RenderContext,
        target: u16,
    ) -> Option<RenderedSegment> {
        // Same hide-rules as `render`: outside a repo or in a bare
        // repo, there's nothing structured to shed and no shorter
        // form to offer.
        let arc = ctx.git();
        let gc = match &*arc {
            Err(_) | Ok(None) => return None,
            Ok(Some(gc)) if matches!(gc.repo_kind, RepoKind::Bare) => return None,
            Ok(Some(gc)) => gc,
        };
        let text = self.assemble_compact(gc);
        if text.is_empty() {
            return None;
        }
        let rendered = RenderedSegment::new(text).with_role(Role::Accent);
        (rendered.width <= target).then_some(rendered)
    }
}

impl GitBranchSegment {
    fn assemble(&self, gc: &GitContext, rc: &RenderContext) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.cfg.label.is_empty() {
            parts.push(self.cfg.label.clone());
        }

        let head = self.render_head(&gc.head);
        if !head.is_empty() {
            parts.push(head);
        }

        if self.cfg.dirty_enabled && !is_below_threshold(rc, self.cfg.dirty_hide_below_cells) {
            if let Some(marker) = self.render_dirty(gc) {
                parts.push(marker);
            }
        }

        if self.cfg.ahead_behind.enabled
            && !is_below_threshold(rc, self.cfg.ahead_behind.hide_below_cells)
        {
            if let Some(marker) = self.render_ahead_behind(gc) {
                parts.push(marker);
            }
        }

        parts.join(" ")
    }

    /// `assemble` with both structured-tail markers (dirty,
    /// ahead/behind) suppressed regardless of config. The compact
    /// fallback the engine asks for via `shrink_to_fit` under layout
    /// pressure: shed decoration, keep the signal-bearing prefix
    /// (label + head).
    fn assemble_compact(&self, gc: &GitContext) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.cfg.label.is_empty() {
            parts.push(self.cfg.label.clone());
        }
        let head = self.render_head(&gc.head);
        if !head.is_empty() {
            parts.push(head);
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
    use crate::data_context::{DirtyState, GitContext, Head, RepoKind, UpstreamState};
    use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn minimal_status() -> StatusContext {
        StatusContext {
            tool: Tool::ClaudeCode,
            model: Some(ModelInfo {
                display_name: "Claude".into(),
            }),
            workspace: Some(WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            }),
            context_window: None,
            cost: None,
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            version: None,
            raw: Arc::new(serde_json::Value::Null),
        }
    }

    fn rc() -> RenderContext {
        RenderContext::new(80)
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
            .render(&ctx_with_git(Ok(None)), &rc())
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
            .render(&ctx_with_git(Err(err)), &rc())
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
            .render(&ctx_with_git(Ok(Some(gc))), &rc())
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
            .render(&ctx_with_git(Ok(Some(gc))), &rc())
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
            .render(&ctx_with_git(Ok(Some(gc))), &rc())
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
            .render(&ctx_with_git(Ok(Some(gc))), &rc())
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
            .render(
                &ctx_with_upstream(
                    Head::Branch("main".into()),
                    Some(UpstreamState {
                        ahead: 2,
                        behind: 0,
                        upstream_branch: "origin/main".into(),
                    }),
                ),
                &rc(),
            )
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main ↑2");
    }

    #[test]
    fn renders_behind_when_remote_leads() {
        let rendered = GitBranchSegment::default()
            .render(
                &ctx_with_upstream(
                    Head::Branch("main".into()),
                    Some(UpstreamState {
                        ahead: 0,
                        behind: 3,
                        upstream_branch: "origin/main".into(),
                    }),
                ),
                &rc(),
            )
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main ↓3");
    }

    #[test]
    fn renders_both_when_diverged() {
        let rendered = GitBranchSegment::default()
            .render(
                &ctx_with_upstream(
                    Head::Branch("main".into()),
                    Some(UpstreamState {
                        ahead: 2,
                        behind: 3,
                        upstream_branch: "origin/main".into(),
                    }),
                ),
                &rc(),
            )
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main ↑2 ↓3");
    }

    #[test]
    fn hides_ahead_behind_when_zero_by_default() {
        let rendered = GitBranchSegment::default()
            .render(
                &ctx_with_upstream(
                    Head::Branch("main".into()),
                    Some(UpstreamState {
                        ahead: 0,
                        behind: 0,
                        upstream_branch: "origin/main".into(),
                    }),
                ),
                &rc(),
            )
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main");
    }

    #[test]
    fn shows_zeros_when_configured() {
        let mut seg = GitBranchSegment::default();
        seg.cfg.ahead_behind.hide_when_zero = false;
        let rendered = seg
            .render(
                &ctx_with_upstream(
                    Head::Branch("main".into()),
                    Some(UpstreamState {
                        ahead: 0,
                        behind: 0,
                        upstream_branch: "origin/main".into(),
                    }),
                ),
                &rc(),
            )
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main ↑0 ↓0");
    }

    #[test]
    fn hides_ahead_behind_when_no_upstream_by_default() {
        let rendered = GitBranchSegment::default()
            .render(&ctx_with_upstream(Head::Branch("main".into()), None), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "main");
    }

    #[test]
    fn renders_question_mark_when_no_upstream_opted_in() {
        let mut seg = GitBranchSegment::default();
        seg.cfg.ahead_behind.hide_when_no_upstream = false;
        let rendered = seg
            .render(&ctx_with_upstream(Head::Branch("main".into()), None), &rc())
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
            .render(&dc, &rc())
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
            .render(
                &ctx_with_upstream(
                    Head::Unborn {
                        symbolic_ref: "main".into(),
                    },
                    None,
                ),
                &rc(),
            )
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
            .render(
                &ctx_with_upstream(
                    Head::OtherRef {
                        full_name: "refs/remotes/origin/feature".into(),
                    },
                    None,
                ),
                &rc(),
            )
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
            .render(
                &ctx_with_upstream(
                    Head::Unborn {
                        symbolic_ref: "main".into(),
                    },
                    None,
                ),
                &rc(),
            )
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
            .render(
                &ctx_with_upstream(
                    Head::Branch("main".into()),
                    Some(UpstreamState {
                        ahead: 5,
                        behind: 0,
                        upstream_branch: "origin/main".into(),
                    }),
                ),
                &rc(),
            )
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
            .render(&ctx_with_git(Ok(Some(gc))), &rc())
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
            .render(&ctx_with_git(Ok(Some(gc))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "master");
    }

    #[test]
    fn applies_label_when_configured() {
        let mut seg = GitBranchSegment::default();
        seg.cfg.label = "branch:".into();
        let gc = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/repo/.git"),
            Head::Branch("main".into()),
        );
        let rendered = seg
            .render(&ctx_with_git(Ok(Some(gc))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "branch: main");
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
    fn from_extras_reads_label_and_dirty_knobs() {
        let mut extras = BTreeMap::new();
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

    // --- Width-aware threshold ---

    /// Build a `DataContext` with a dirty working tree and an
    /// ahead/behind upstream so both markers fire at full width.
    fn ctx_with_dirty_and_upstream(ahead: u32, behind: u32) -> DataContext {
        let gc = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/repo/.git"),
            Head::Branch("main".into()),
        );
        gc.preseed_dirty_state(DirtyState::Dirty(None))
            .expect("fresh dirty cell");
        gc.preseed_upstream(Some(UpstreamState {
            ahead,
            behind,
            upstream_branch: "origin/main".into(),
        }))
        .expect("fresh upstream cell");
        let dc = DataContext::with_cwd(minimal_status(), None);
        dc.preseed_git(Ok(Some(gc))).expect("seed");
        dc
    }

    fn render_at(seg: &GitBranchSegment, terminal_width: u16, dc: &DataContext) -> String {
        let rendered = seg
            .render(dc, &RenderContext::new(terminal_width))
            .unwrap()
            .expect("rendered");
        rendered.text().to_string()
    }

    #[test]
    fn dirty_hide_below_cells_default_zero_keeps_existing_behavior() {
        // Default config: threshold 0 means "never auto-hide". The
        // dirty marker shows even at terminal_width=1.
        let seg = GitBranchSegment::default();
        let dc = ctx_with_dirty_and_upstream(0, 0);
        assert_eq!(render_at(&seg, 1, &dc), "main *");
        assert_eq!(render_at(&seg, 200, &dc), "main *");
    }

    #[test]
    fn dirty_marker_hidden_when_terminal_width_below_threshold() {
        let mut seg = GitBranchSegment::default();
        seg.cfg.dirty_hide_below_cells = 50;
        let dc = ctx_with_dirty_and_upstream(0, 0);
        // Width 49: below threshold → marker hidden.
        assert_eq!(render_at(&seg, 49, &dc), "main");
        // Width 50 and above: not-below → marker shown.
        assert_eq!(render_at(&seg, 50, &dc), "main *");
        assert_eq!(render_at(&seg, 100, &dc), "main *");
    }

    #[test]
    fn ahead_behind_hidden_when_terminal_width_below_threshold() {
        let mut seg = GitBranchSegment::default();
        seg.cfg.ahead_behind.hide_below_cells = 80;
        let dc = ctx_with_dirty_and_upstream(2, 1);
        // Width 79: below ahead/behind threshold → ahead/behind
        // hidden. Dirty still shows (its threshold defaults to 0).
        assert_eq!(render_at(&seg, 79, &dc), "main *");
        // Width 80 and above: not-below → ahead/behind shown.
        assert_eq!(render_at(&seg, 80, &dc), "main * ↑2 ↓1");
    }

    #[test]
    fn per_marker_thresholds_compose_independently() {
        // dirty.hide_below_cells = 50, ahead_behind.hide_below_cells = 80.
        // Three regimes: full / no-tracking / branch-only.
        let mut seg = GitBranchSegment::default();
        seg.cfg.dirty_hide_below_cells = 50;
        seg.cfg.ahead_behind.hide_below_cells = 80;
        let dc = ctx_with_dirty_and_upstream(2, 1);
        assert_eq!(render_at(&seg, 100, &dc), "main * ↑2 ↓1"); // full
        assert_eq!(render_at(&seg, 60, &dc), "main *"); // no tracking
        assert_eq!(render_at(&seg, 40, &dc), "main"); // branch only
    }

    #[test]
    fn enabled_false_overrides_hide_below_cells() {
        // `dirty.enabled = false` always hides, regardless of threshold.
        let mut seg = GitBranchSegment::default();
        seg.cfg.dirty_enabled = false;
        seg.cfg.dirty_hide_below_cells = 50;
        let dc = ctx_with_dirty_and_upstream(0, 0);
        // Wide terminal, threshold satisfied — but still hidden.
        assert_eq!(render_at(&seg, 200, &dc), "main");
    }

    #[test]
    fn from_extras_reads_dirty_hide_below_cells() {
        let mut dirty = toml::value::Table::new();
        dirty.insert("hide_below_cells".to_string(), toml::Value::Integer(60));
        let extras = BTreeMap::from([("dirty".to_string(), toml::Value::Table(dirty))]);
        let seg = GitBranchSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.dirty_hide_below_cells, 60);
    }

    #[test]
    fn from_extras_reads_ahead_behind_hide_below_cells() {
        let mut ab = toml::value::Table::new();
        ab.insert("hide_below_cells".to_string(), toml::Value::Integer(90));
        let extras = BTreeMap::from([("ahead_behind".to_string(), toml::Value::Table(ab))]);
        let seg = GitBranchSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.ahead_behind.hide_below_cells, 90);
    }

    #[test]
    fn ahead_behind_hide_when_zero_and_hide_below_cells_compose_multiplicatively() {
        // The two gates live in different layers: `hide_below_cells`
        // short-circuits in `assemble`, while `hide_when_zero` checks
        // counts inside `render_ahead_behind`. Either gate firing
        // hides the marker. Pin both branches so a refactor that
        // collapses them can't silently drop one path.
        let mut seg = GitBranchSegment::default();
        seg.cfg.ahead_behind.hide_below_cells = 80;
        // Width gate fires first (counts non-zero, but width < 80).
        let dc_diverged = ctx_with_dirty_and_upstream(2, 1);
        assert_eq!(render_at(&seg, 79, &dc_diverged), "main *");
        // Counts gate fires (width >= 80, but ahead == 0 && behind == 0
        // and hide_when_zero defaults to true).
        let dc_zero = ctx_with_dirty_and_upstream(0, 0);
        assert_eq!(render_at(&seg, 100, &dc_zero), "main *");
    }

    #[test]
    fn from_extras_warns_on_negative_hide_below_cells_and_keeps_default() {
        let mut dirty = toml::value::Table::new();
        dirty.insert("hide_below_cells".to_string(), toml::Value::Integer(-5));
        let extras = BTreeMap::from([("dirty".to_string(), toml::Value::Table(dirty))]);
        let mut warnings = vec![];
        let seg = GitBranchSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.dirty_hide_below_cells, 0);
        assert!(warnings
            .iter()
            .any(|w| w.contains("segments.git_branch.dirty.hide_below_cells")));
    }

    // --- shrink_to_fit (layout-pressure-aware compaction) ---

    #[test]
    fn shrink_to_fit_returns_compact_form_when_target_fits() {
        // Full assembly is "main * ↑2 ↓1" (12 cells). Compact form
        // is "main" (4 cells). Target ≥ 4 → engine gets the compact
        // form; the segment sheds dirty + ahead/behind.
        let seg = GitBranchSegment::default();
        let dc = ctx_with_dirty_and_upstream(2, 1);
        let dummy_rc = RenderContext::new(80);
        let shrunk = seg
            .shrink_to_fit(&dc, &dummy_rc, 4)
            .expect("compact form fits");
        assert_eq!(shrunk.text(), "main");
        assert_eq!(shrunk.style().role, Some(Role::Accent));
    }

    #[test]
    fn shrink_to_fit_returns_none_when_even_compact_form_overflows() {
        // Compact form is "main" (4 cells). Target 3 is below that,
        // so `shrink_to_fit` declines (returns `None`) rather than
        // emit a too-wide render. The engine's drop-on-decline
        // behavior is covered separately by the layout-side test.
        let seg = GitBranchSegment::default();
        let dc = ctx_with_dirty_and_upstream(2, 1);
        let dummy_rc = RenderContext::new(80);
        assert!(seg.shrink_to_fit(&dc, &dummy_rc, 3).is_none());
    }

    #[test]
    fn shrink_to_fit_returns_none_outside_repo() {
        let seg = GitBranchSegment::default();
        let dc = ctx_with_git(Ok(None));
        let dummy_rc = RenderContext::new(80);
        assert!(seg.shrink_to_fit(&dc, &dummy_rc, 100).is_none());
    }

    #[test]
    fn shrink_to_fit_returns_none_in_bare_repo() {
        let seg = GitBranchSegment::default();
        let gc = GitContext::new(
            RepoKind::Bare,
            PathBuf::from("/tmp/bare.git"),
            Head::Unborn {
                symbolic_ref: "main".into(),
            },
        );
        let dc = ctx_with_git(Ok(Some(gc)));
        let dummy_rc = RenderContext::new(80);
        assert!(seg.shrink_to_fit(&dc, &dummy_rc, 100).is_none());
    }

    #[test]
    fn shrink_to_fit_keeps_configured_label_in_compact_form() {
        let mut seg = GitBranchSegment::default();
        seg.cfg.label = "br:".into();
        let dc = ctx_with_dirty_and_upstream(2, 1);
        let dummy_rc = RenderContext::new(80);
        let shrunk = seg
            .shrink_to_fit(&dc, &dummy_rc, 50)
            .expect("compact form fits");
        assert_eq!(shrunk.text(), "br: main");
    }

    #[test]
    fn shrink_to_fit_strips_markers_even_when_thresholds_would_keep_them() {
        // Reflow path: a wide terminal (rc=200) with both
        // hide_below_cells thresholds at 0 means render() emits the
        // full assembly. shrink_to_fit is a separate engine-driven
        // gate that must still produce the compact form when the
        // engine asks, regardless of the user's threshold preferences.
        let seg = GitBranchSegment::default();
        let dc = ctx_with_dirty_and_upstream(2, 1);
        let wide_rc = RenderContext::new(200);
        let shrunk = seg
            .shrink_to_fit(&dc, &wide_rc, 50)
            .expect("compact form fits 50 cells");
        assert_eq!(shrunk.text(), "main");
    }
}
