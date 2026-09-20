# Changelog

All notable changes are documented here. AgentTraceback follows Semantic Versioning after
the first public release.

## 0.1.0-alpha.3 — 2026-09-20

### Added

- Launch recorded agent runs from onboarding or the dashboard in a system terminal,
  with a native project folder picker and a working transcript capture checkbox.
- Import detected agent history directly from onboarding with progress and results.
- Added visible command copying and an app-styled text menu with copy, cut, paste,
  keyboard navigation, and native clipboard access.

### Fixed

- Replaced misleading onboarding switches with accurate capture descriptions.
- Kept AppImage library overrides out of launched system terminals.

## 0.1.0-alpha.2 — 2026-09-20

### Fixed

- Fixed desktop startup when unsigned builds omit updater configuration.
- Added a packaged Linux desktop startup check before release asset publication.
- Corrected README animation timing and invalidated the cached image URL.

## 0.1.0-alpha.1 — 2026-09-20

### Fixed

- Fixed Windows/macOS conditional compilation and cross-platform stale runtime cleanup.
- Preserved global-chain recording and search-index consistency after audited deletion.
- Added encrypted pre-restore backup plans and undo references for in-place recovery.
- Refused populated reconstruction destinations, single-file overwrites, and stale files.
- Prevented replacement-key creation when encrypted data already exists.
- Bound hook execution to the server-generated approval plan.
- Bounded JSONL batch reads and redacted imported session titles before persistence.
- Escaped untrusted Markdown fields and kept project capture inside the selected directory.
- Disabled Git hooks, filesystem monitors, and configured content filters during capture;
  recovery worktrees are created without checkout.
- Labeled unsigned integrity roots honestly and clarified their verification scope.
- Paused automated dependency version PRs during launch stabilization.


### Added

- Added the complete AgentTraceback desktop product flow: resumable onboarding,
  live dashboard, sessions and session detail, virtualized timeline, conversation,
  files, processes capability state, Git state, usage, findings, settings, command
  palette, evidence drawer, recovery planning, and redacted export.
- Added Claude Code, Codex CLI, Hermes Agent, OpenCode, and Gemini CLI adapters with
  persistent cursors, tolerant parsing, malformed-record quarantine, truncation and
  replacement recovery, and synthetic conformance fixtures.
- Added the Command Code detection profile with truthful semantic-unavailable
  capabilities and generic wrapper recording.
- Added reversible Claude Code hook plan/install/uninstall APIs, CLI commands,
  structural JSON merge, timestamped backup, stable ownership tags, and persisted
  receipts.
- Added project, session, timeline, usage, file-history, finding, dashboard, demo,
  export, verification, and adapter APIs backed by persisted SQLite records.
- Added redacted JSON and Markdown evidence bundles with defense-in-depth secret
  scrubbing.
- Added explicit opt-in full JSON/Markdown export with clear local-only warnings;
  redacted export remains the default.
- Added a clearly labeled, removable demo dataset with verified evidence, findings,
  encrypted before/after file versions, snapshots, exports, and safe recovery.
- Added Playwright end-to-end coverage against a real temporary daemon and demo
  data, plus generated launch screenshots and GIF.
- Added Tauri installer bundles, release artifact checksums, Windows/macOS/Linux
  release workflows, opt-in signed updater support, and per-user startup management
  for Linux, macOS, and Windows.
- Added SQLite backup/restore commands, daily rotated daemon logs with seven-day
  cleanup, support-bundle output, chain verification, and an audited deletion path.

### Milestone 5

- Added Hermes and OpenCode read-only SQLite adapters, Gemini JSON import, source
  version drift handling, capability diagnostics, and the Command Code detection
  profile. Every required adapter passes its fixture-backed conformance tests.

### Milestone 4

- Added the adapter SDK registry, Claude Code and Codex imports and tails,
  correlation-driven verification, progressive import cursors, hook planning, and
  reversible Claude configuration changes.

### Milestone 3

- Added file identities and versions, encrypted recovery content, immutable recovery
  plans, exact new-directory reconstruction, individual restore, Git worktree
  recovery, conflict refusal, structured diffs, and explainable risk findings.

### Changed

- Canonicalized the unreleased product, repository, package, binary, path, and Tauri
  identifiers as AgentTraceback. No compatibility aliases are retained for
  pre-release development data.
- Replaced the placeholder desktop shell with production screens backed only by real
  persisted daemon data. Demo records are explicitly labeled and removable.
- Extended the schema to version 4 for persisted hook receipts and explicit audited
  deletion while preserving ordinary append-only event behavior.

### Milestone 2

- Added project discovery, canonical path containment, `.agenttracebackignore`, and
  read-only Git state capture.
- Added deterministic baseline manifests with tracked/dirty/untracked coverage,
  BLAKE3 hashes, symlink metadata, size bounds, and reconciliation diffs.
- Added cross-platform PTY execution with argv preservation, resize polling, stdin
  forwarding, transcript callbacks, and exact child exit-code propagation.
- Added native recursive filesystem observation with ordered create/write/delete/
  rename events, debounce, overflow gaps, and rescan reconciliation.
- Added best-effort process ancestry observation keyed by PID plus start time.
- Added daemon-owned wrapper session lifecycle, baseline/Git persistence, active
  session coordination, conservative concurrent-session attribution, and chain
  finalization after watcher flush.
- Added `agenttraceback run -- <command>` and authenticated wrapper lifecycle API.

### Milestone 1

- Added `0001_initial.sql` with normalized metadata, evidence, correlation, recovery,
  import, blob, job, settings, and FTS tables.
- Added a single-writer SQLite actor, bounded read pool, idempotent source identity,
  superseding corrections, and append-only event triggers.
- Added HKDF-derived installation subkeys with macOS Keychain, Windows Credential
  Manager, Linux Secret Service, and owner-only Linux file fallback.
- Added encrypted content-addressed blobs using Zstandard and
  XChaCha20-Poly1305 with authenticated versioned headers.
- Added normalized event/action/evidence types, validation, redaction, encrypted raw
  payload ingestion, SHA-256 event chains, and tamper verification.
- Added structured query parsing and indexed FTS5 search without raw-content
  decryption.
- Added authenticated event retrieval, storage, capabilities, health, and search API
  routes backed by the real daemon store.
- Added a reproducible million-event benchmark. Latest release run: 1,000,000 events
  at 7,718 events/s with 5.32 ms search p95.

### Added

- Rust 2024 Cargo workspace and pnpm workspace foundation.
- Tauri 2 and React 19 desktop shell with real daemon health connection.
- `agenttracebackd` loopback API with per-installation bearer authentication, strict Host
  and Origin validation, single-instance locking, and owner-only runtime metadata.
- `agenttraceback status` human-readable and JSON output.
- Cross-platform CI and release workflow skeleton.
- Deterministic fixture-agent skeleton and initial security/API tests.
- Architecture contract, threat model, evidence model, recovery model, and ADRs.

### Security

- The local API binds only to `127.0.0.1`.
- Runtime metadata and the API token are written with owner-only permissions on Unix.
- Unauthenticated and hostile-origin requests are rejected.
