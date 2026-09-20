-- Keep a reference to the encrypted undo plan even when a restore run fails.
ALTER TABLE recovery_runs ADD COLUMN backup_plan_id BLOB REFERENCES recovery_plans(id);

-- Global chains retain their hashes when a user explicitly deletes project data.
-- Only the target identifier and deletion audit reference survive, not content.
CREATE TABLE deleted_chain_targets (
    target_id TEXT PRIMARY KEY,
    deletion_audit_id BLOB NOT NULL REFERENCES deletion_audit(id)
) STRICT;

UPDATE chain_roots SET verification_status = 'unsigned' WHERE signature IS NULL;

-- Audited deletions must also release FTS row IDs for subsequent inserts.
DELETE FROM events_fts WHERE NOT EXISTS (
    SELECT 1 FROM events WHERE events.rowid = events_fts.rowid
        AND lower(hex(events.id)) = events_fts.event_id
);
CREATE TRIGGER events_fts_delete AFTER DELETE ON events
BEGIN
    DELETE FROM events_fts WHERE rowid = old.rowid;
END;
