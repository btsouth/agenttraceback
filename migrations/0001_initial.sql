-- AgentTraceback initial metadata, evidence, and integrity schema.
-- UUID identifiers are stored as 16-byte BLOBs. Timestamps are UTC microseconds.

CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at_us INTEGER NOT NULL
) STRICT;

CREATE TABLE installations (
    id BLOB PRIMARY KEY,
    created_at_us INTEGER NOT NULL,
    schema_version INTEGER NOT NULL,
    installation_public_key BLOB,
    key_protection_kind TEXT NOT NULL
) STRICT;

CREATE TABLE adapter_installations (
    id BLOB PRIMARY KEY,
    adapter_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    agent_version TEXT,
    detected_at_us INTEGER NOT NULL,
    last_seen_at_us INTEGER NOT NULL,
    status TEXT NOT NULL,
    diagnostic_code TEXT,
    UNIQUE (adapter_id)
) STRICT;

CREATE TABLE adapter_capabilities (
    adapter_installation_id BLOB NOT NULL REFERENCES adapter_installations(id) ON DELETE CASCADE,
    capability TEXT NOT NULL,
    availability TEXT NOT NULL,
    detail TEXT,
    checked_at_us INTEGER NOT NULL,
    PRIMARY KEY (adapter_installation_id, capability)
) STRICT;

CREATE TABLE projects (
    id BLOB PRIMARY KEY,
    display_name TEXT NOT NULL,
    canonical_root TEXT NOT NULL,
    comparison_root TEXT NOT NULL,
    vcs_kind TEXT NOT NULL,
    created_at_us INTEGER NOT NULL,
    last_seen_at_us INTEGER NOT NULL,
    UNIQUE (comparison_root)
) STRICT;

CREATE TABLE project_roots (
    id BLOB PRIMARY KEY,
    project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    canonical_path TEXT NOT NULL,
    comparison_path TEXT NOT NULL,
    root_kind TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    UNIQUE (project_id, comparison_path, root_kind)
) STRICT;

CREATE TABLE sessions (
    id BLOB PRIMARY KEY,
    project_id BLOB REFERENCES projects(id) ON DELETE SET NULL,
    title_preview TEXT,
    state TEXT NOT NULL,
    started_at_us INTEGER NOT NULL,
    ended_at_us INTEGER,
    agent_name TEXT,
    harness_name TEXT,
    provider_name TEXT,
    model_name TEXT,
    source_session_id TEXT,
    outcome TEXT NOT NULL,
    outcome_confidence TEXT NOT NULL,
    recovery_coverage TEXT NOT NULL,
    capture_health TEXT NOT NULL,
    risk_max_severity TEXT NOT NULL DEFAULT 'none',
    chain_root_id BLOB REFERENCES chain_roots(id) ON DELETE SET NULL
) STRICT;

CREATE TABLE session_sources (
    id BLOB PRIMARY KEY,
    session_id BLOB NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    adapter_installation_id BLOB REFERENCES adapter_installations(id) ON DELETE SET NULL,
    source_kind TEXT NOT NULL,
    source_identifier TEXT NOT NULL,
    first_seen_at_us INTEGER NOT NULL,
    last_seen_at_us INTEGER NOT NULL,
    UNIQUE (session_id, source_kind, source_identifier)
) STRICT;

CREATE TABLE events (
    id BLOB PRIMARY KEY,
    project_id BLOB REFERENCES projects(id) ON DELETE SET NULL,
    supersedes_event_id BLOB REFERENCES events(id) ON DELETE RESTRICT,
    occurred_at_us INTEGER NOT NULL,
    observed_at_us INTEGER NOT NULL,
    monotonic_ns INTEGER,
    action TEXT NOT NULL,
    raw_action TEXT,
    source_kind TEXT NOT NULL,
    source_event_id TEXT NOT NULL,
    adapter_id TEXT,
    source_evidence_class TEXT NOT NULL CHECK (source_evidence_class IN ('reported', 'observed')),
    actor_agent TEXT,
    actor_model TEXT,
    pid INTEGER,
    parent_pid INTEGER,
    actor_subagent_id TEXT,
    target_kind TEXT NOT NULL,
    target_display TEXT,
    target_comparison TEXT,
    target_external INTEGER NOT NULL DEFAULT 0 CHECK (target_external IN (0, 1)),
    result_status TEXT NOT NULL,
    exit_code INTEGER,
    duration_ms INTEGER,
    redacted_preview TEXT,
    before_hash TEXT,
    after_hash TEXT,
    raw_blob_id TEXT,
    payload_blob_id TEXT,
    risk_max_severity TEXT NOT NULL DEFAULT 'none',
    schema_version INTEGER NOT NULL,
    envelope_json TEXT NOT NULL
) STRICT;

CREATE TABLE event_sources (
    id BLOB PRIMARY KEY,
    event_id BLOB NOT NULL REFERENCES events(id) ON DELETE RESTRICT,
    source_kind TEXT NOT NULL,
    adapter_installation_id BLOB REFERENCES adapter_installations(id) ON DELETE SET NULL,
    adapter_id TEXT NOT NULL DEFAULT '',
    native_event_id TEXT NOT NULL,
    superseded_by_event_id BLOB REFERENCES events(id) ON DELETE SET NULL,
    source_location TEXT,
    source_offset INTEGER,
    raw_blob_id TEXT,
    parser_version TEXT,
    UNIQUE (event_id, source_kind, native_event_id)
) STRICT;

CREATE TABLE event_session_links (
    event_id BLOB NOT NULL REFERENCES events(id) ON DELETE RESTRICT,
    session_id BLOB NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    link_kind TEXT NOT NULL,
    attribution_confidence TEXT NOT NULL,
    algorithm_version INTEGER NOT NULL,
    created_at_us INTEGER NOT NULL,
    PRIMARY KEY (event_id, session_id)
) STRICT;

CREATE TABLE correlations (
    id BLOB PRIMARY KEY,
    session_id BLOB REFERENCES sessions(id) ON DELETE CASCADE,
    logical_event_id BLOB REFERENCES events(id) ON DELETE RESTRICT,
    reported_event_id BLOB NOT NULL REFERENCES events(id) ON DELETE RESTRICT,
    observed_event_id BLOB NOT NULL REFERENCES events(id) ON DELETE RESTRICT,
    correlation_kind TEXT NOT NULL,
    confidence TEXT NOT NULL,
    algorithm_version INTEGER NOT NULL,
    created_at_us INTEGER NOT NULL,
    supersedes_id BLOB REFERENCES correlations(id) ON DELETE RESTRICT,
    UNIQUE (reported_event_id, observed_event_id, algorithm_version)
) STRICT;

CREATE TABLE correlation_conflicts (
    id BLOB PRIMARY KEY,
    session_id BLOB REFERENCES sessions(id) ON DELETE CASCADE,
    left_event_id BLOB NOT NULL REFERENCES events(id) ON DELETE RESTRICT,
    right_event_id BLOB NOT NULL REFERENCES events(id) ON DELETE RESTRICT,
    reason_code TEXT NOT NULL,
    detail TEXT NOT NULL,
    created_at_us INTEGER NOT NULL,
    resolved_by_correlation_id BLOB REFERENCES correlations(id) ON DELETE SET NULL
) STRICT;

CREATE TABLE processes (
    id BLOB PRIMARY KEY,
    session_id BLOB REFERENCES sessions(id) ON DELETE SET NULL,
    project_id BLOB REFERENCES projects(id) ON DELETE SET NULL,
    pid INTEGER NOT NULL,
    start_identity TEXT NOT NULL,
    parent_process_id BLOB REFERENCES processes(id) ON DELETE SET NULL,
    executable_display TEXT NOT NULL,
    argv_preview TEXT,
    argv_blob_id TEXT,
    cwd_display TEXT,
    started_at_us INTEGER NOT NULL,
    ended_at_us INTEGER,
    exit_code INTEGER,
    source_event_id BLOB REFERENCES events(id) ON DELETE SET NULL,
    attribution_confidence TEXT NOT NULL,
    UNIQUE (pid, start_identity)
) STRICT;

CREATE TABLE files (
    id BLOB PRIMARY KEY,
    project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    current_display_path TEXT NOT NULL,
    current_comparison_path TEXT NOT NULL,
    stable_file_identity TEXT,
    first_seen_at_us INTEGER NOT NULL,
    last_seen_at_us INTEGER NOT NULL,
    sensitive_class TEXT,
    UNIQUE (project_id, current_comparison_path)
) STRICT;

CREATE TABLE file_versions (
    id BLOB PRIMARY KEY,
    file_id BLOB NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    session_id BLOB REFERENCES sessions(id) ON DELETE SET NULL,
    event_id BLOB REFERENCES events(id) ON DELETE SET NULL,
    display_path_at_time TEXT NOT NULL,
    content_hash TEXT,
    blob_id TEXT,
    byte_length INTEGER,
    media_kind TEXT,
    executable INTEGER,
    symlink_target TEXT,
    capture_status TEXT NOT NULL,
    created_at_us INTEGER NOT NULL
) STRICT;

CREATE TABLE snapshots (
    id BLOB PRIMARY KEY,
    project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    session_id BLOB REFERENCES sessions(id) ON DELETE SET NULL,
    snapshot_kind TEXT NOT NULL,
    coverage TEXT NOT NULL,
    started_at_us INTEGER NOT NULL,
    completed_at_us INTEGER,
    manifest_hash TEXT,
    status TEXT NOT NULL
) STRICT;

CREATE TABLE snapshot_entries (
    snapshot_id BLOB NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
    file_id BLOB REFERENCES files(id) ON DELETE SET NULL,
    display_path TEXT NOT NULL,
    comparison_path TEXT NOT NULL,
    file_version_id BLOB REFERENCES file_versions(id) ON DELETE SET NULL,
    entry_kind TEXT NOT NULL,
    capture_status TEXT NOT NULL,
    git_object_id TEXT,
    PRIMARY KEY (snapshot_id, comparison_path)
) STRICT;

CREATE TABLE git_states (
    id BLOB PRIMARY KEY,
    session_id BLOB REFERENCES sessions(id) ON DELETE CASCADE,
    snapshot_phase TEXT NOT NULL,
    repo_root TEXT NOT NULL,
    head_oid TEXT,
    branch_name TEXT,
    upstream_name TEXT,
    status_blob_id TEXT,
    captured_at_us INTEGER NOT NULL
) STRICT;

CREATE TABLE git_operations (
    id BLOB PRIMARY KEY,
    session_id BLOB REFERENCES sessions(id) ON DELETE CASCADE,
    event_id BLOB REFERENCES events(id) ON DELETE SET NULL,
    operation_kind TEXT NOT NULL,
    before_ref TEXT,
    after_ref TEXT,
    commit_oid TEXT,
    created_at_us INTEGER NOT NULL
) STRICT;

CREATE TABLE risk_findings (
    id BLOB PRIMARY KEY,
    session_id BLOB REFERENCES sessions(id) ON DELETE CASCADE,
    event_id BLOB REFERENCES events(id) ON DELETE RESTRICT,
    rule_id TEXT NOT NULL,
    rule_version TEXT NOT NULL,
    severity TEXT NOT NULL,
    initial_status TEXT NOT NULL,
    title TEXT NOT NULL,
    explanation TEXT NOT NULL,
    matched_preview TEXT,
    created_at_us INTEGER NOT NULL
) STRICT;

CREATE TABLE annotations (
    id BLOB PRIMARY KEY,
    session_id BLOB REFERENCES sessions(id) ON DELETE CASCADE,
    finding_id BLOB REFERENCES risk_findings(id) ON DELETE CASCADE,
    target_kind TEXT NOT NULL,
    target_id TEXT NOT NULL,
    annotation_kind TEXT NOT NULL,
    redacted_value TEXT,
    encrypted_value_blob_id TEXT,
    created_at_us INTEGER NOT NULL,
    supersedes_annotation_id BLOB REFERENCES annotations(id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE retention_tombstones (
    id BLOB PRIMARY KEY,
    session_id BLOB REFERENCES sessions(id) ON DELETE SET NULL,
    blob_digest TEXT NOT NULL,
    retention_class TEXT NOT NULL,
    reason_code TEXT NOT NULL,
    deleted_at_us INTEGER NOT NULL
) STRICT;

CREATE TABLE import_sources (
    id BLOB PRIMARY KEY,
    adapter_installation_id BLOB REFERENCES adapter_installations(id) ON DELETE CASCADE,
    source_kind TEXT NOT NULL,
    canonical_location TEXT NOT NULL,
    stable_identity TEXT NOT NULL,
    status TEXT NOT NULL,
    last_error_code TEXT,
    UNIQUE (adapter_installation_id, stable_identity)
) STRICT;

CREATE TABLE import_cursors (
    import_source_id BLOB PRIMARY KEY REFERENCES import_sources(id) ON DELETE CASCADE,
    cursor_version INTEGER NOT NULL,
    byte_offset INTEGER NOT NULL,
    native_cursor TEXT,
    source_size INTEGER,
    source_mtime_us INTEGER,
    last_event_id BLOB REFERENCES events(id) ON DELETE SET NULL,
    updated_at_us INTEGER NOT NULL
) STRICT;

CREATE TABLE blobs (
    id TEXT PRIMARY KEY,
    digest TEXT NOT NULL,
    format_version INTEGER NOT NULL,
    plaintext_bytes INTEGER NOT NULL CHECK (plaintext_bytes >= 0),
    encrypted_bytes INTEGER NOT NULL CHECK (encrypted_bytes >= 0),
    media_category TEXT NOT NULL,
    retention_class TEXT NOT NULL,
    reference_count INTEGER NOT NULL DEFAULT 0 CHECK (reference_count >= 0),
    state TEXT NOT NULL,
    created_at_us INTEGER NOT NULL,
    expires_at_us INTEGER,
    UNIQUE (digest)
) STRICT;

CREATE TABLE chain_entries (
    id BLOB PRIMARY KEY,
    session_id BLOB REFERENCES sessions(id) ON DELETE RESTRICT,
    chain_scope TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    entry_kind TEXT NOT NULL,
    target_id TEXT NOT NULL,
    canonical_digest BLOB NOT NULL,
    raw_payload_digest BLOB,
    previous_hash BLOB NOT NULL,
    entry_hash BLOB NOT NULL,
    created_at_us INTEGER NOT NULL,
    UNIQUE (chain_scope, sequence),
    UNIQUE (entry_kind, target_id)
) STRICT;

CREATE TABLE chain_roots (
    id BLOB PRIMARY KEY,
    session_id BLOB REFERENCES sessions(id) ON DELETE SET NULL,
    chain_version INTEGER NOT NULL,
    entry_count INTEGER NOT NULL,
    root_hash BLOB NOT NULL,
    signature BLOB,
    signing_key_id TEXT,
    finalized_at_us INTEGER,
    verification_status TEXT NOT NULL
) STRICT;

CREATE TABLE jobs (
    id BLOB PRIMARY KEY,
    job_kind TEXT NOT NULL,
    state TEXT NOT NULL,
    progress_current INTEGER NOT NULL DEFAULT 0,
    progress_total INTEGER,
    request_blob_id TEXT,
    result_blob_id TEXT,
    error_code TEXT,
    created_at_us INTEGER NOT NULL,
    started_at_us INTEGER,
    finished_at_us INTEGER
) STRICT;

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    updated_at_us INTEGER NOT NULL
) STRICT;

CREATE VIRTUAL TABLE events_fts USING fts5(
    event_id UNINDEXED,
    redacted_preview,
    target_display,
    agent,
    model,
    session_title,
    project_name,
    git_metadata,
    finding_text,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE INDEX events_time_idx ON events(occurred_at_us, id);
CREATE INDEX events_project_time_idx ON events(project_id, occurred_at_us);
CREATE INDEX events_action_time_idx ON events(action, occurred_at_us);
CREATE INDEX events_target_time_idx ON events(target_comparison, occurred_at_us);
CREATE INDEX events_risk_idx ON events(risk_max_severity, occurred_at_us);
CREATE INDEX event_session_links_session_idx ON event_session_links(session_id, event_id);
CREATE INDEX event_session_links_confidence_idx ON event_session_links(event_id, attribution_confidence);
CREATE INDEX event_sources_event_idx ON event_sources(event_id);
CREATE UNIQUE INDEX event_sources_active_identity_idx
    ON event_sources(source_kind, adapter_id, native_event_id)
    WHERE superseded_by_event_id IS NULL;
CREATE INDEX sessions_start_idx ON sessions(started_at_us DESC, id);
CREATE INDEX sessions_project_idx ON sessions(project_id, started_at_us DESC);
CREATE INDEX sessions_agent_idx ON sessions(agent_name, started_at_us DESC);
CREATE INDEX sessions_model_idx ON sessions(model_name, started_at_us DESC);
CREATE INDEX sessions_outcome_idx ON sessions(outcome, started_at_us DESC);
CREATE INDEX sessions_risk_idx ON sessions(risk_max_severity, started_at_us DESC);
CREATE INDEX files_stable_identity_idx ON files(project_id, stable_file_identity);
CREATE INDEX file_versions_file_time_idx ON file_versions(file_id, created_at_us DESC);
CREATE UNIQUE INDEX import_sources_identity_idx ON import_sources(adapter_installation_id, stable_identity);
CREATE INDEX correlations_active_pair_idx ON correlations(reported_event_id, observed_event_id, algorithm_version);
CREATE INDEX jobs_state_created_idx ON jobs(state, created_at_us);

CREATE TRIGGER events_no_update
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TRIGGER events_no_delete
BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TRIGGER events_fts_insert
AFTER INSERT ON events
BEGIN
    INSERT INTO events_fts(
        rowid,
        event_id,
        redacted_preview,
        target_display,
        agent,
        model,
        session_title,
        project_name,
        git_metadata,
        finding_text
    )
    VALUES (
        new.rowid,
        lower(hex(new.id)),
        COALESCE(new.redacted_preview, ''),
        COALESCE(new.target_display, ''),
        COALESCE(new.actor_agent, ''),
        COALESCE(new.actor_model, ''),
        COALESCE((SELECT title_preview FROM sessions s
                  JOIN event_session_links l ON l.session_id = s.id
                  WHERE l.event_id = new.id LIMIT 1), ''),
        COALESCE((SELECT p.display_name FROM projects p WHERE p.id = new.project_id), ''),
        '',
        ''
    );
END;

CREATE TRIGGER event_session_links_fts_update
AFTER INSERT ON event_session_links
BEGIN
    UPDATE events_fts
    SET session_title = COALESCE(
        (SELECT title_preview FROM sessions WHERE id = new.session_id),
        session_title
    )
    WHERE rowid = (SELECT rowid FROM events WHERE id = new.event_id);
END;
