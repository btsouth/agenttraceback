# Architecture Overview

AgentTraceback is a verified record and recovery tool for AI coding-agent runs, not an
observability SDK, guard, blocker, or generic activity dashboard. The daemon owns
ingestion, observation, normalization, correlation, persistence, search, recovery,
and the local API. The desktop application and CLI consume that API.

## Component Map

```text
agent adapters ─┐
generic wrapper ─┼─> normalize/redact ─> correlate/classify ─> SQLite + blobs
host observers ──┤                                                   │
Git/snapshots ───┘                                                   v
                                                       loopback REST/WebSocket
                                                              │          │
                                                              v          v
                                                        desktop CLI  +  agenttraceback
```

## Runtime Ownership

`agenttracebackd` is a per-user process. It owns adapter discovery and import, wrapper
session registration, host observers, project baselines, event processing, integrity
chains, search indexing, recovery jobs, local API service, retention, and health.

`agenttraceback` is a thin client and wrapper launcher. It does not open SQLite directly and
does not implement a second normalization path.

The Tauri application starts or connects to the daemon, bridges authenticated API
requests through Rust commands, and renders the user experience. It can close while
background recording continues when the user enables that behavior.

## Storage Boundary

SQLite stores metadata, normalized events, source relationships, correlation records,
project/session state, jobs, settings, and redacted search text. Full prompts,
responses, commands, terminal output, file content, and diffs that may contain secrets
belong in encrypted blobs.

All persisted state is addressed by UUIDv7 identifiers, timestamps are UTC
microseconds, and event integrity is an append-only SHA-256 chain. A `VERIFIED` UI
state is a projection over immutable correlation records, not a mutation of an event.

## Trust Boundary

Agent output is untrusted. Parsers are bounded, raw inputs are size-limited, and
recorded terminal or Markdown content is never executed as HTML. Agent-controlled
logs and hooks remain `REPORTED`; wrapper, filesystem, process, and Git observers are
independent `OBSERVED` sources.

The local API binds only to `127.0.0.1`, requires `Authorization: Bearer <token>`,
checks `Host`, rejects hostile browser origins, and uses `no-store` response headers.

## Current Implementation

Implemented now:

- Cargo and pnpm workspace layout with all specified crate boundaries.
- `agenttracebackd` process with single-instance lock, loopback HTTP server, token auth,
  capabilities and health endpoints, runtime metadata, and graceful shutdown.
- `agenttraceback status` client with stable JSON and documented daemon-unavailable exit
  code.
- SQLite schema, append-only event chains, encrypted content-addressed blobs, master
  key protection, redacted previews, bounded ingestion, and structured FTS search.
- Authenticated event retrieval, storage summary, and search API routes backed by the
  real store.
- Generic cross-platform PTY wrapper with exact exit-code propagation, ordered
  filesystem observation, conservative attribution, best-effort process ancestry,
  baseline manifests, overflow reconciliation, and Git before/after state.
- Tauri 2 desktop shell that starts or connects to the daemon and reads real health
  and capability data.
- Fixture-agent skeleton with deterministic file, process, failure, sensitive-path,
  malformed-input, and crash scenarios.
- File identities, snapshots, encrypted recovery content, deterministic diffs,
  immutable recovery plans, new-directory reconstruction, guarded in-place restore,
  Git worktree recovery, and explainable risk findings.
- Claude Code, Codex CLI, Hermes, OpenCode, and Gemini CLI semantic adapters with
  persisted cursors and fixture-backed conformance tests.
- Command Code detection profile and generic wrapper coverage.
- Reversible Claude hook planning, installation, backup, and uninstall.
- Product UI screens for onboarding, live activity, sessions, timeline, evidence,
  conversation, files, usage, findings, settings, export, and recovery.
- Redacted JSON/Markdown export, demo mode, support diagnostics, startup management,
  SQLite backup/restore, signed-update build hooks, and installer workflows.

Not implemented yet:

- Seven-day soak and packaged smoke-test evidence.
- Production signing/notarization credentials and a published release.
- Full export is explicit opt-in and may contain credentials or source content; the
  default remains redacted.
- Process and Git detail views are event-derived projections; dedicated normalized
  process/Git summary tables remain a post-v0.1 refinement.

No unavailable capture capability is reported as active. Storage and search report
their actual database state and event count.
