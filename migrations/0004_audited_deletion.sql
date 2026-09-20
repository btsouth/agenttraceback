-- User-requested deletion is explicit, scoped, and audited. Ordinary event mutation
-- remains blocked by the append-only trigger.

CREATE TABLE deletion_context (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    project_id BLOB NOT NULL
) STRICT;

CREATE TABLE deletion_audit (
    id BLOB PRIMARY KEY,
    target_kind TEXT NOT NULL,
    target_id TEXT NOT NULL,
    event_count INTEGER NOT NULL CHECK (event_count >= 0),
    created_at_us INTEGER NOT NULL
) STRICT;

DROP TRIGGER events_no_delete;

CREATE TRIGGER events_no_delete
BEFORE DELETE ON events
WHEN NOT EXISTS (
    SELECT 1 FROM deletion_context
    WHERE project_id = OLD.project_id
)
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;
