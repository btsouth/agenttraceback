CREATE TABLE recovery_plans (
    id BLOB PRIMARY KEY,
    session_id BLOB REFERENCES sessions(id) ON DELETE CASCADE,
    action TEXT NOT NULL,
    destination TEXT NOT NULL,
    status TEXT NOT NULL,
    plan_digest TEXT NOT NULL,
    plan_blob_id TEXT NOT NULL,
    created_at_us INTEGER NOT NULL,
    executed_at_us INTEGER,
    result_blob_id TEXT
) STRICT;

CREATE TABLE recovery_runs (
    id BLOB PRIMARY KEY,
    plan_id BLOB NOT NULL REFERENCES recovery_plans(id) ON DELETE CASCADE,
    state TEXT NOT NULL,
    restored_files INTEGER NOT NULL DEFAULT 0 CHECK (restored_files >= 0),
    skipped_files INTEGER NOT NULL DEFAULT 0 CHECK (skipped_files >= 0),
    conflict_files INTEGER NOT NULL DEFAULT 0 CHECK (conflict_files >= 0),
    result_blob_id TEXT,
    created_at_us INTEGER NOT NULL,
    finished_at_us INTEGER,
    error_code TEXT
) STRICT;

CREATE INDEX recovery_plans_session_idx ON recovery_plans(session_id, created_at_us DESC);
CREATE INDEX recovery_runs_plan_idx ON recovery_runs(plan_id, created_at_us DESC);
