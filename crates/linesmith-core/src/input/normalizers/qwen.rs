//! Qwen Code normalizer (stub).
//!
//! Qwen Code payloads route through the [`claude`](super::claude)
//! normalizer as the documented **Fallback** per
//! `docs/specs/input-schema.md` §"Heuristic detection". The fallback is
//! safe because Qwen's canonical schema is a near-superset of CC's;
//! tool-specific Qwen fields stay accessible to plugins via
//! [`StatusContext::raw`](crate::input::StatusContext::raw).
//!
//! ## Why no `normalize` here yet
//!
//! The prior detection rule ("`version` field present ⇒ Qwen") is
//! invalid since Claude Code 2.x emits `version` itself (verified
//! 2026-04-28). A real Qwen normalizer needs a current Qwen sample
//! payload to compare against current CC and pick a discriminator that
//! survives. Until then, [`super::detect_from_shape`] never returns
//! [`Tool::QwenCode`](crate::input::Tool::QwenCode) via heuristic —
//! only an explicit [`ParseOpts::tool`](crate::input::ParseOpts) or
//! `LINESMITH_TOOL=qwen` can.
