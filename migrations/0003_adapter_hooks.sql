-- Persisted receipts for reversible, user-approved adapter hook configuration.

CREATE TABLE adapter_hooks (
    id BLOB PRIMARY KEY,
    adapter_id TEXT NOT NULL UNIQUE,
    installation_id BLOB REFERENCES adapter_installations(id) ON DELETE SET NULL,
    config_path TEXT NOT NULL,
    backup_path TEXT NOT NULL,
    plan_digest TEXT NOT NULL,
    installed_at_us INTEGER NOT NULL
) STRICT;

CREATE INDEX adapter_hooks_adapter_idx ON adapter_hooks(adapter_id);
