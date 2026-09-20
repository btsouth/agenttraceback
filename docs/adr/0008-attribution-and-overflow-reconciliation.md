# ADR 0008: Conservative Attribution and Overflow Reconciliation

- Status: Accepted
- Date: 2026-09-20

## Context

Filesystem notifications do not carry a reliable writer PID across platforms. Two
agents can mutate the same repository concurrently, so assigning every write to the
only session that happens to be active would create false evidence.

## Decision

Keep one project observation and a set of active session IDs. A filesystem event is
attributed at `high` confidence only when exactly one session owns the project scope.
With zero or multiple sessions it remains `observed` and `unattributed`. Process
events become `exact` only through the registered PTY root PID and observed process
ancestry.

Watcher overflow emits a `recorder_gap`, rescans the project manifest, and emits
reconciliation events for deterministic manifest differences. Reconciliation follows
the same conservative attribution rule.

## Consequences

Ambiguous concurrent activity is never falsely linked to an agent. Users may see
unattributed file events, which is truthful standard-mode behavior. Final manifest
reconciliation recovers effects missed after watcher failure without upgrading them
to exact process attribution.
