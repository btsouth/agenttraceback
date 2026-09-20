# ADR 0004: Dedicated SQLite Writer and Append-Only Chains

- Status: Accepted
- Date: 2026-09-20

## Context

Concurrent event sources can append at high rates while API clients read and search.
SQLite permits one writer at a time, and evidence integrity requires sequence
assignment, event persistence, source identity, and chain advancement to commit
atomically.

## Decision

Use one dedicated writer thread with a bounded Tokio command queue and a pool of
read-only SQLite connections. A batch of up to 1,000 events is persisted in one
immediate transaction. Chain sequence and previous-hash cursors are cached within the
transaction, while source identities use a covering unique index for idempotent
re-import detection.

Events are append-only. Corrections insert a superseding event and mark the prior
source identity as superseded. Chain entries store the canonical target digest,
optional raw-payload digest, previous hash, and current hash.

## Consequences

Writer contention is explicit and bounded. Search reads do not block the writer under
normal WAL operation. The event store must keep its transaction small enough to avoid
unbounded memory while preserving batch throughput. Full event canonicalization and
RFC 8785 serialization are on the ingestion path.
