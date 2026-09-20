# ADR 0011: Audited Deletion With Append-Only Events

Normal event updates and deletes remain blocked by SQLite triggers. User-requested
project or demo deletion opens a one-row deletion context for the exact project and
records a `deletion_audit` row before commit. Only events in that scoped project can
be removed while the context is active.

This preserves append-only evidence for normal operation without claiming that a
user cannot delete their own local history. Chain and source relationships are
removed in dependency order inside one transaction.
