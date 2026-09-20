# ADR 0003: Single-Instance Lock and Runtime Metadata

- Status: Accepted
- Date: 2026-09-20

## Context

Two daemon processes writing the same SQLite database, blob store, and evidence chain
would violate storage and integrity invariants. Clients also need a deterministic way
to discover the active daemon.

## Decision

Acquire an exclusive lock in the data root before opening storage or publishing an
endpoint. A second instance fails with a conflict instead of starting another API.
After binding an ephemeral loopback port, atomically publish runtime metadata. Remove
the metadata during graceful shutdown only when its PID matches the shutting-down
process.

## Consequences

Distinct `AGENTTRACEBACK_DATA_DIR` values can run independent daemons for tests and
development. Because runtime files may survive a crash, clients treat missing or
unreachable metadata as unavailable and may replace stale metadata only after a stale
process check.
