# Security Policy

## Supported Versions

AgentTraceback has not published a stable release. Security fixes are applied to the main
development line until the first supported release is announced.

## Reporting a Vulnerability

Do not open a public issue for a suspected vulnerability. Use GitHub's private
security advisory feature for the repository. If private reporting is unavailable,
contact the maintainers through the security contact published on the repository
profile.

Include:

- The affected revision and platform.
- Reproduction steps or a minimal proof of concept.
- Expected impact and whether local data or user files may be exposed.
- Whether the issue requires a malicious agent log, local user, browser origin, or
  another trust boundary.
- A redacted sample only. Never send API tokens, master keys, private source code,
  prompts, terminal output, or real credentials.

## Response Expectations

Maintainers will acknowledge a complete report, reproduce it privately, and publish
a fix and advisory when users can act on the information. Timelines depend on
severity and release readiness.

## Security Invariants

The project does not weaken these invariants to ship a feature:

- The local API binds to loopback only and requires authentication.
- Raw and recovery payloads are encrypted at rest before v0.1 release.
- Search, logs, diagnostics, and default exports contain redacted previews only.
- Snapshot and recovery paths are contained and symlink-safe.
- Recorded content is never executed as HTML.
- An agent configuration is never changed without explicit approval, backup, and a
  reversible plan.
- `VERIFIED` requires evidence from independent source boundaries.
