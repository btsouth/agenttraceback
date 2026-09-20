# Migrations

Current migrations are `0001_initial.sql` through `0004_audited_deletion.sql`.

- `0001_initial.sql` creates the core metadata, evidence, integrity, recovery, and
  search schema.
- `0002_recovery_plans.sql` adds immutable recovery plans and execution runs.
- `0003_adapter_hooks.sql` persists reversible hook receipts.
- `0004_audited_deletion.sql` adds an explicit, scoped deletion context and audit
  record while preserving append-only behavior for ordinary events.

Migrations are forward-only and owned by `agenttraceback-store`. Never modify a released
migration. Every migration must:

- Run against a backup taken before schema change.
- Preserve data or fail before committing a partial state.
- Be covered by upgrade tests from every released schema.
- Keep event, correlation, and chain invariants inside the same transaction where
  required.

Recovery from a failed migration restores the automatic pre-migration backup; it does
not run hand-written reverse SQL.
