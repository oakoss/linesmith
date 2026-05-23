//! Codex CLI / Copilot CLI normalizers (stubs).
//!
//! Both tools route through the [`claude`](super::claude) normalizer as
//! the documented **Fallback** per `docs/specs/input-schema.md`
//! §"Heuristic detection". Tool-specific fields stay accessible to
//! plugins via [`StatusContext::raw`](crate::input::StatusContext::raw).
//!
//! ## Detection status
//!
//! - **Codex CLI**: rule TBD — needs a published statusLine API to
//!   compare against. [`super::detect_from_shape`] won't return
//!   [`Tool::CodexCli`](crate::input::Tool::CodexCli) via heuristic; an
//!   explicit [`ParseOpts::tool`](crate::input::ParseOpts) or
//!   `LINESMITH_TOOL=codex` selects it.
//! - **Copilot CLI**: rule TBD — same situation.
