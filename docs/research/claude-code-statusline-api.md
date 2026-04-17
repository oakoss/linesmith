# Claude Code Status Line API Contract

- Date: 2026-04-17
- Author: Claude Code research agent session
- Scope: Document the technical contract Claude Code uses to invoke user status line commands, so we can build against it correctly.

## Question

How does Claude Code invoke a user-defined status line? What data does it pass in, what output does it expect, what are the performance and lifecycle constraints?

## Sources

- [Claude Code Status Line Documentation](https://code.claude.com/docs/en/statusline)
- [Complete Guide: All Fields, Config, Ready-to-Use Scripts](https://gist.github.com/AKCodez/ffb420ba6a7662b5c3dda2edce7783de)
- [ccstatusline GitHub Repository](https://github.com/sirmalloc/ccstatusline)
- [Dan Does Code — Building a Custom Claude Code Statusline](https://www.dandoescode.com/blog/claude-code-custom-statusline)

## Findings

### Invocation mechanism

- Trigger: executes after each assistant message, permission mode change, or vim mode toggle
- Debouncing: 300ms window prevents rapid re-invocation
- Lifecycle: new process spawn per invocation — not persistent
- Visibility: hidden during autocomplete, help menus, and permission prompts

### Configuration

Configured via `settings.json` under the `statusLine` key with `type: "command"`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "sh ~/.claude/statusline.sh",
    "padding": 0
  }
}
```

### Input — stdin JSON

Claude Code pipes the full session state as JSON to stdin:

```json
{
  "model": { "id": "claude-sonnet-4-6", "display_name": "Claude Sonnet 4.6" },
  "session_id": "...",
  "session_name": "...",
  "cwd": "/path/to/project",
  "workspace": {
    "current_dir": ".",
    "project_dir": ".",
    "added_dirs": [],
    "git_worktree": { "name": "main", "path": "..." }
  },
  "context_window": {
    "used_percentage": 12.4,
    "remaining_percentage": 87.6,
    "context_window_size": 200000,
    "total_input_tokens": 24800,
    "total_output_tokens": 3200,
    "current_usage": {
      "input_tokens": 2000,
      "output_tokens": 500,
      "cache_creation_input_tokens": 0,
      "cache_read_input_tokens": 500
    }
  },
  "cost": {
    "total_cost_usd": 0.04,
    "total_duration_ms": 5000,
    "total_api_duration_ms": 3500,
    "total_lines_added": 42,
    "total_lines_removed": 12
  },
  "rate_limits": {
    "five_hour": {
      "used_percentage": 35.2,
      "resets_at": "2026-04-17T17:30:00Z"
    },
    "seven_day": {
      "used_percentage": 12.1,
      "resets_at": "2026-04-24T15:00:00Z"
    }
  },
  "output_style": { "name": "default" },
  "vim": { "mode": "normal" },
  "agent": { "name": "..." }
}
```

**Important nullability:**

- `current_usage` is `null` before the first API call in a session
- `rate_limits` is only present for Pro/Max subscriptions — missing for API-key users

### Output format

- Stdout: plain text (multiple lines become separate status bar lines)
- ANSI escape codes supported (colors, styles)
- OSC 8 clickable hyperlinks supported
- No documented max length, truncation, or terminal-width awareness

### Performance constraints

- No timeout specified in official docs
- Zero API cost — local execution only
- 300ms debouncing prevents rapid re-invocation
- Async execution — slow scripts don't block prompts

### Environment

- Working directory: session's cwd (also present in JSON)
- Env vars: inherits parent process; Claude Code doesn't set custom vars
- File access: full read access to workspace; git operations available

### Version notes / gotchas

- Recent change: 300ms debounce was added to prevent excessive invocation
- Cache fields (`cache_read_input_tokens`, `cache_creation_input_tokens`) track prompt caching
- `rate_limits` fields vary by subscription tier — always check before accessing

## Conclusions

The API contract is well-shaped for a typed Rust implementation. Every invocation is a fresh process with a rich JSON payload. No async I/O to the statusline is required; it reads, renders, exits.

**Critical nullability to handle:** `current_usage` null before first call, `rate_limits` absent on API-key tier. Our schema must model these as `Option<T>`.

**Critical correctness risk:** the `context_window.used_percentage` is known to be unreliable under 1M context, post-`/compact`, post-`/resume`, and during 429s (see `user-demand.md` research). We cannot trust this field blindly.

## Implications / actions

- Drives [ADR-0006: Tool-Agnostic JSON Schema](../adrs/0006-tool-agnostic-json-schema.md) (which extends this with Qwen fields)
- Informs segment system design — segments receive a parsed/validated context, not raw JSON
- Performance budget: aim for <20ms cold start; 300ms debounce gives us real breathing room but users notice lag

## Open questions

- What's the actual observed timeout before Claude Code gives up on a slow statusline? (no docs say)
- Does `added_dirs` ever carry content for multi-root workspaces, or is it always empty?
- How does `workspace.git_worktree.name` behave when there's no git — absent, null, or empty?
- What exactly does `agent.name` contain in practice (custom subagents? MCP tools?) — needs empirical observation
