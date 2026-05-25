# Security policy

## Supported versions

Only the latest `linesmith/v*` release on GitHub Releases / crates.io receives security fixes. Pre-1.0 minor bumps may be breaking per the project's semver posture (see [`docs/specs/release-process.md`](docs/specs/release-process.md) §Tag format and semver posture).

The scaffolding crates `linesmith-core` and `linesmith-plugin` ship to crates.io to satisfy cargo's transitive-dep rule (per [ADR-0019](docs/adrs/0019-publish-linesmith-core-as-scaffolding-from-v0-1.md)); they have no SemVer guarantee for direct consumers. Security fixes to these crates ship as part of the next `linesmith` binary release.

## Reporting a vulnerability

Use GitHub's [private vulnerability reporting](https://github.com/oakoss/linesmith/security/advisories/new) on this repository. The form is the preferred channel — it keeps the disclosure private and creates a draft advisory for coordinated disclosure.

If GitHub's reporting form is unavailable, email `jbabin91@gmail.com` with `[linesmith security]` in the subject line.

Please include:

- A description of the issue and its impact
- Steps to reproduce (a minimal `linesmith` invocation + any required config / env)
- Affected version(s) (`linesmith --version` output)
- Any proposed mitigation

## Response expectations

- **Acknowledgement** within 72 hours
- **Initial assessment** within 7 days
- **Fix or mitigation timeline** communicated after assessment

This is a solo-maintained project; response times reflect best-effort availability rather than a contractual SLA. Coordinated-disclosure timelines are negotiable based on severity.

## Disclosure

After a fix ships:

1. The released advisory documents the issue, affected versions, fix version, and credit to the reporter (unless anonymity is requested).
2. The CHANGELOG entry for the fix release links the advisory.
3. The release attestation chain (SLSA Level 3 via `actions/attest-build-provenance` — see [`docs/specs/release-process.md`](docs/specs/release-process.md) §Supply-chain posture) covers the fixed artifact.

## Scope

In-scope:

- The `linesmith` binary's behavior given hostile input (malformed JSON on stdin, hostile config files, hostile rhai plugins).
- Credential handling in the `linesmith` binary (OAuth token storage / leakage via logs or terminal output).
- Supply-chain integrity of releases (tag manipulation, registry confusion).

Out-of-scope:

- Vulnerabilities in upstream dependencies — report those to the dependency's maintainers. We track advisories via `cargo audit` and ship patched versions on the next routine release.
- Issues in third-party rhai plugins users install themselves; rhai's sandboxing model is documented in [ADR-0004](docs/adrs/0004-rhai-for-plugins.md).
- Issues in Claude Code or other AI CLIs that linesmith integrates with — those have their own disclosure channels.
