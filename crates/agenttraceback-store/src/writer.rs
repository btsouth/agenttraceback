use std::collections::HashMap;

use agenttraceback_crypto::{ChainHash, canonical_digest, hash_chain_entry};
use agenttraceback_types::{EntityId, EventEnvelope, EventIntegrity};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, named_params};
use tokio::sync::{mpsc, oneshot};

use crate::{
    AdapterHookRecord, AdapterInstallationRecord, CorrelationConflictInsert, CorrelationInsert,
    FileRecord, FileVersionRecord, GitStateRecord, ImportCursorRecord, ImportSourceRecord,
    ProjectRecord, RecoveryPlanRecord, RecoveryRunRecord, RiskFindingRecord, SessionRecord,
    SnapshotRecord, StoreError, StoreResult, migration,
};

pub(crate) enum WriteCommand {
    AppendEvents {
        events: Vec<EventEnvelope>,
        reply: oneshot::Sender<StoreResult<Vec<EventEnvelope>>>,
    },
    RecordBlob {
        metadata: crate::BlobMetadata,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    FinalizeChain {
        session_id: Option<EntityId>,
        reply: oneshot::Sender<StoreResult<EntityId>>,
    },
    UpsertProject {
        project: ProjectRecord,
        reply: oneshot::Sender<StoreResult<EntityId>>,
    },
    CreateSession {
        session: SessionRecord,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    UpsertSession {
        session: SessionRecord,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    UpsertAdapterInstallation {
        installation: AdapterInstallationRecord,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    UpsertImportSource {
        source: ImportSourceRecord,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    SaveImportCursor {
        cursor: ImportCursorRecord,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    SaveAdapterHook {
        hook: AdapterHookRecord,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    DeleteAdapterHook {
        adapter_id: String,
        reply: oneshot::Sender<StoreResult<bool>>,
    },
    DeleteProjectData {
        project_id: EntityId,
        reply: oneshot::Sender<StoreResult<u64>>,
    },
    CompleteSession {
        session_id: EntityId,
        ended_at_us: i64,
        outcome: String,
        capture_health: String,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    SaveSnapshot {
        snapshot: SnapshotRecord,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    SaveGitState {
        state: GitStateRecord,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    UpsertFile {
        file: FileRecord,
        reply: oneshot::Sender<StoreResult<EntityId>>,
    },
    InsertFileVersion {
        version: FileVersionRecord,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    SaveRiskFindings {
        findings: Vec<RiskFindingRecord>,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    SaveRecoveryPlan {
        plan: RecoveryPlanRecord,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    SaveRecoveryRun {
        run: RecoveryRunRecord,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    SaveCorrelation {
        correlation: Option<CorrelationInsert>,
        conflict: Option<CorrelationConflictInsert>,
        reply: oneshot::Sender<StoreResult<()>>,
    },
    Close {
        reply: oneshot::Sender<()>,
    },
}

pub(crate) fn spawn_writer(
    mut connection: Connection,
    mut receiver: mpsc::Receiver<WriteCommand>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("agenttraceback-store-writer".to_owned())
        .spawn(move || {
            while let Some(command) = receiver.blocking_recv() {
                match command {
                    WriteCommand::AppendEvents { events, reply } => {
                        let result = append_events(&mut connection, events);
                        let _ = reply.send(result);
                    }
                    WriteCommand::RecordBlob { metadata, reply } => {
                        let result = record_blob(&connection, metadata);
                        let _ = reply.send(result);
                    }
                    WriteCommand::FinalizeChain { session_id, reply } => {
                        let result = finalize_chain(&mut connection, session_id);
                        let _ = reply.send(result);
                    }
                    WriteCommand::UpsertProject { project, reply } => {
                        let result = upsert_project(&mut connection, project);
                        let _ = reply.send(result);
                    }
                    WriteCommand::CreateSession { session, reply } => {
                        let result = create_session(&mut connection, session);
                        let _ = reply.send(result);
                    }
                    WriteCommand::UpsertSession { session, reply } => {
                        let result = upsert_session(&connection, session);
                        let _ = reply.send(result);
                    }
                    WriteCommand::UpsertAdapterInstallation {
                        installation,
                        reply,
                    } => {
                        let result = upsert_adapter_installation(&connection, installation);
                        let _ = reply.send(result);
                    }
                    WriteCommand::UpsertImportSource { source, reply } => {
                        let result = upsert_import_source(&connection, source);
                        let _ = reply.send(result);
                    }
                    WriteCommand::SaveImportCursor { cursor, reply } => {
                        let result = save_import_cursor(&connection, cursor);
                        let _ = reply.send(result);
                    }
                    WriteCommand::SaveAdapterHook { hook, reply } => {
                        let result = save_adapter_hook(&connection, hook);
                        let _ = reply.send(result);
                    }
                    WriteCommand::DeleteAdapterHook { adapter_id, reply } => {
                        let result = delete_adapter_hook(&connection, &adapter_id);
                        let _ = reply.send(result);
                    }
                    WriteCommand::DeleteProjectData { project_id, reply } => {
                        let result = delete_project_data(&mut connection, project_id);
                        let _ = reply.send(result);
                    }
                    WriteCommand::CompleteSession {
                        session_id,
                        ended_at_us,
                        outcome,
                        capture_health,
                        reply,
                    } => {
                        let result = complete_session(
                            &connection,
                            session_id,
                            ended_at_us,
                            &outcome,
                            &capture_health,
                        );
                        let _ = reply.send(result);
                    }
                    WriteCommand::SaveSnapshot { snapshot, reply } => {
                        let result = save_snapshot(&mut connection, snapshot);
                        let _ = reply.send(result);
                    }
                    WriteCommand::SaveGitState { state, reply } => {
                        let result = save_git_state(&connection, state);
                        let _ = reply.send(result);
                    }
                    WriteCommand::UpsertFile { file, reply } => {
                        let result = upsert_file(&mut connection, file);
                        let _ = reply.send(result);
                    }
                    WriteCommand::InsertFileVersion { version, reply } => {
                        let result = insert_file_version(&connection, version);
                        let _ = reply.send(result);
                    }
                    WriteCommand::SaveRiskFindings { findings, reply } => {
                        let result = save_risk_findings(&mut connection, findings);
                        let _ = reply.send(result);
                    }
                    WriteCommand::SaveRecoveryPlan { plan, reply } => {
                        let result = save_recovery_plan(&connection, plan);
                        let _ = reply.send(result);
                    }
                    WriteCommand::SaveRecoveryRun { run, reply } => {
                        let result = save_recovery_run(&connection, run);
                        let _ = reply.send(result);
                    }
                    WriteCommand::SaveCorrelation {
                        correlation,
                        conflict,
                        reply,
                    } => {
                        let result = save_correlation(&mut connection, correlation, conflict);
                        let _ = reply.send(result);
                    }
                    WriteCommand::Close { reply } => {
                        let _ = reply.send(());
                        break;
                    }
                }
            }
        })
}

fn append_events(
    connection: &mut Connection,
    events: Vec<EventEnvelope>,
) -> StoreResult<Vec<EventEnvelope>> {
    for event in &events {
        event.validate().map_err(StoreError::Validation)?;
    }

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StoreError::Sqlite)?;
    let mut persisted = Vec::with_capacity(events.len());
    let mut chain_cursors = HashMap::<String, (u64, ChainHash)>::new();
    for event in events {
        let canonical = source_identity_value(&event)?;
        if let Some(existing) = find_existing_source(&transaction, &event)? {
            let existing_canonical = source_identity_value(&existing.envelope)?;
            if canonical_digest(&canonical)? == canonical_digest(&existing_canonical)? {
                persisted.push(existing.envelope);
                continue;
            }
            let stored = persist_new_event(
                &transaction,
                event,
                Some(existing.event_id),
                &mut chain_cursors,
            )?;
            transaction
                .execute(
                    "UPDATE event_sources SET superseded_by_event_id = ?1 WHERE id = ?2",
                    rusqlite::params![id_blob(stored.id), existing.source_row_id],
                )
                .map_err(StoreError::Sqlite)?;
            persisted.push(stored);
            continue;
        }
        persisted.push(persist_new_event(
            &transaction,
            event,
            None,
            &mut chain_cursors,
        )?);
    }
    transaction.commit().map_err(StoreError::Sqlite)?;
    Ok(persisted)
}

struct ExistingSource {
    event_id: EntityId,
    source_row_id: Vec<u8>,
    envelope: EventEnvelope,
}

fn find_existing_source(
    transaction: &Transaction<'_>,
    event: &EventEnvelope,
) -> StoreResult<Option<ExistingSource>> {
    let row = transaction
        .query_row(
            "SELECT e.id, s.id, e.envelope_json
             FROM event_sources s
             JOIN events e ON e.id = s.event_id
             WHERE s.source_kind = ?1
               AND s.adapter_id = ?2
               AND s.native_event_id = ?3
               AND s.superseded_by_event_id IS NULL
             LIMIT 1",
            rusqlite::params![
                event.source.kind.as_str(),
                event.source.adapter_id.as_deref().unwrap_or(""),
                event.source.source_event_id
            ],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(StoreError::Sqlite)?;

    row.map(|(event_id, source_row_id, envelope_json)| {
        Ok(ExistingSource {
            event_id: uuid_from_blob(&event_id)?,
            source_row_id,
            envelope: serde_json::from_str(&envelope_json).map_err(StoreError::EnvelopeJson)?,
        })
    })
    .transpose()
}

fn persist_new_event(
    transaction: &Transaction<'_>,
    mut event: EventEnvelope,
    supersedes_event_id: Option<EntityId>,
    chain_cursors: &mut HashMap<String, (u64, ChainHash)>,
) -> StoreResult<EventEnvelope> {
    let chain_scope = event
        .session_id
        .map_or_else(|| "global".to_owned(), |id| id.to_string());
    if chain_root_exists(transaction, event.session_id)? {
        return Err(StoreError::ChainFinalized);
    }
    let (previous_sequence, previous_hash) =
        next_chain_position(transaction, &chain_scope, chain_cursors)?;
    let sequence = previous_sequence.saturating_add(1);
    event.sequence = None;
    event.integrity = None;
    let canonical = canonical_digest(&event)?;
    let raw_digest = parse_optional_digest(event.source.raw_blob_id.as_deref())?;
    let entry_hash = hash_chain_entry(previous_hash, "source_event", canonical, raw_digest);
    event.sequence = Some(sequence);
    event.integrity = Some(EventIntegrity {
        previous_hash: previous_hash.to_hex(),
        event_hash: entry_hash.to_hex(),
    });

    let envelope_json = serde_json::to_string(&event).map_err(StoreError::EnvelopeJson)?;
    let source_event_row_id = id_blob(EntityId::new());
    let occurred_at_us = event.occurred_at_us;
    let observed_at_us = event.observed_at_us;
    transaction
        .execute(
            "INSERT INTO events (
                id, project_id, supersedes_event_id, occurred_at_us, observed_at_us,
                monotonic_ns, action, raw_action, source_kind, source_event_id, adapter_id,
                source_evidence_class, actor_agent, actor_model, pid, parent_pid,
                actor_subagent_id, target_kind, target_display, target_comparison,
                target_external, result_status, exit_code, duration_ms, redacted_preview,
                before_hash, after_hash, raw_blob_id, payload_blob_id, risk_max_severity,
                schema_version, envelope_json
            ) VALUES (
                :id, :project_id, :supersedes_event_id, :occurred_at_us, :observed_at_us,
                :monotonic_ns, :action, :raw_action, :source_kind, :source_event_id,
                :adapter_id, :source_evidence_class, :actor_agent, :actor_model, :pid,
                :parent_pid, :actor_subagent_id, :target_kind, :target_display,
                :target_comparison, :target_external, :result_status, :exit_code,
                :duration_ms, :redacted_preview, :before_hash, :after_hash, :raw_blob_id,
                :payload_blob_id, :risk_max_severity, :schema_version, :envelope_json
            )",
            named_params! {
                ":id": id_blob(event.id),
                ":project_id": optional_id_blob(event.project_id),
                ":supersedes_event_id": optional_id_blob(supersedes_event_id),
                ":occurred_at_us": occurred_at_us,
                ":observed_at_us": observed_at_us,
                ":monotonic_ns": optional_u64_to_i64(event.monotonic_ns)?,
                ":action": event.action.as_str(),
                ":raw_action": event.raw_action.as_deref(),
                ":source_kind": event.source.kind.as_str(),
                ":source_event_id": event.source.source_event_id.as_str(),
                ":adapter_id": event.source.adapter_id.as_deref(),
                ":source_evidence_class": event.evidence.class.as_str(),
                ":actor_agent": event.actor.agent.as_deref(),
                ":actor_model": event.actor.model.as_deref(),
                ":pid": event.actor.pid.map(i64::from),
                ":parent_pid": event.actor.parent_pid.map(i64::from),
                ":actor_subagent_id": event.actor.subagent_id.as_deref(),
                ":target_kind": event.target.kind.as_str(),
                ":target_display": event.target.display.as_deref(),
                ":target_comparison": event.target.normalized_path.as_deref(),
                ":target_external": i64::from(event.target.external),
                ":result_status": event.result.status.as_str(),
                ":exit_code": event.result.exit_code.map(i64::from),
                ":duration_ms": optional_u64_to_i64(event.result.duration_ms)?,
                ":redacted_preview": event.content.redacted_preview.as_deref(),
                ":before_hash": event.content.before_hash.as_deref(),
                ":after_hash": event.content.after_hash.as_deref(),
                ":raw_blob_id": event.source.raw_blob_id.as_deref(),
                ":payload_blob_id": event.content.payload_blob_id.as_deref(),
                ":risk_max_severity": event.risk.severity.as_str(),
                ":schema_version": i64::from(event.schema_version),
                ":envelope_json": envelope_json,
            },
        )
        .map_err(StoreError::Sqlite)?;

    transaction
        .execute(
            "INSERT INTO event_sources (
                id, event_id, source_kind, adapter_installation_id, adapter_id,
                native_event_id, superseded_by_event_id, source_location, source_offset,
                raw_blob_id, parser_version
             ) VALUES (?1, ?2, ?3, NULL, ?4, ?5, NULL, NULL, NULL, ?6, NULL)",
            rusqlite::params![
                source_event_row_id,
                id_blob(event.id),
                event.source.kind.as_str(),
                event.source.adapter_id.as_deref().unwrap_or(""),
                event.source.source_event_id.as_str(),
                event.source.raw_blob_id.as_deref(),
            ],
        )
        .map_err(StoreError::Sqlite)?;

    for blob_id in [
        event.source.raw_blob_id.as_deref(),
        event.content.payload_blob_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        transaction
            .execute(
                "UPDATE blobs SET reference_count = reference_count + 1 WHERE id = ?1",
                [blob_id],
            )
            .map_err(StoreError::Sqlite)?;
    }

    if let Some(session_id) = event.session_id {
        transaction
            .execute(
                "INSERT INTO event_session_links (
                    event_id, session_id, link_kind, attribution_confidence,
                    algorithm_version, created_at_us
                 ) VALUES (?1, ?2, 'source', ?3, 1, ?4)",
                rusqlite::params![
                    id_blob(event.id),
                    id_blob(session_id),
                    event.evidence.attribution.as_str(),
                    occurred_at_us,
                ],
            )
            .map_err(StoreError::Sqlite)?;
    }

    transaction
        .execute(
            "INSERT INTO chain_entries (
                id, session_id, chain_scope, sequence, entry_kind, target_id,
                canonical_digest, raw_payload_digest, previous_hash, entry_hash, created_at_us
             ) VALUES (?1, ?2, ?3, ?4, 'source_event', ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                id_blob(EntityId::new()),
                optional_id_blob(event.session_id),
                chain_scope,
                u64_to_i64(sequence)?,
                event.id.to_string(),
                canonical.0.as_slice(),
                raw_digest.map(|digest| digest.0.to_vec()),
                previous_hash.0.as_slice(),
                entry_hash.0.as_slice(),
                migration::current_time_us(),
            ],
        )
        .map_err(StoreError::Sqlite)?;
    chain_cursors.insert(chain_scope, (sequence, entry_hash));
    Ok(event)
}

fn record_blob(connection: &Connection, metadata: crate::BlobMetadata) -> StoreResult<()> {
    connection
        .execute(
            "INSERT INTO blobs (
                id, digest, format_version, plaintext_bytes, encrypted_bytes,
                media_category, retention_class, reference_count, state,
                created_at_us, expires_at_us
             ) VALUES (
                :id, :digest, :format_version, :plaintext_bytes, :encrypted_bytes,
                :media_category, :retention_class, 0, 'active', :created_at_us, :expires_at_us
             )
             ON CONFLICT(id) DO UPDATE SET
                encrypted_bytes = excluded.encrypted_bytes,
                state = 'active',
                expires_at_us = excluded.expires_at_us",
            named_params! {
                ":id": metadata.id,
                ":digest": metadata.digest,
                ":format_version": i64::from(metadata.format_version),
                ":plaintext_bytes": u64_to_i64(metadata.plaintext_bytes)?,
                ":encrypted_bytes": u64_to_i64(metadata.encrypted_bytes)?,
                ":media_category": metadata.media_category,
                ":retention_class": metadata.retention_class,
                ":created_at_us": metadata.created_at_us,
                ":expires_at_us": metadata.expires_at_us,
            },
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn finalize_chain(
    connection: &mut Connection,
    session_id: Option<EntityId>,
) -> StoreResult<EntityId> {
    if let Some(existing) = existing_chain_root(connection, session_id)? {
        return Ok(existing);
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StoreError::Sqlite)?;
    let chain_scope = session_id.map_or_else(|| "global".to_owned(), |id| id.to_string());
    let (entry_count, root_hash) = last_chain_position(&transaction, &chain_scope)?;
    if entry_count == 0 {
        return Err(StoreError::EmptyChain);
    }
    let root_id = EntityId::new();
    transaction
        .execute(
            "INSERT INTO chain_roots (
                id, session_id, chain_version, entry_count, root_hash, signature,
                signing_key_id, finalized_at_us, verification_status
             ) VALUES (?1, ?2, 1, ?3, ?4, NULL, NULL, ?5, 'verified')",
            rusqlite::params![
                id_blob(root_id),
                optional_id_blob(session_id),
                u64_to_i64(entry_count)?,
                root_hash.0.as_slice(),
                migration::current_time_us(),
            ],
        )
        .map_err(StoreError::Sqlite)?;
    if let Some(session_id) = session_id {
        transaction
            .execute(
                "UPDATE sessions SET chain_root_id = ?1 WHERE id = ?2",
                rusqlite::params![id_blob(root_id), id_blob(session_id)],
            )
            .map_err(StoreError::Sqlite)?;
    }
    transaction.commit().map_err(StoreError::Sqlite)?;
    Ok(root_id)
}

fn upsert_project(connection: &mut Connection, project: ProjectRecord) -> StoreResult<EntityId> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StoreError::Sqlite)?;
    let existing = transaction
        .query_row(
            "SELECT id FROM projects WHERE comparison_root = ?1 OR id = ?2 LIMIT 1",
            rusqlite::params![project.comparison_root.as_str(), id_blob(project.id)],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(StoreError::Sqlite)?;
    let project_id = if let Some(existing) = existing {
        let project_id = uuid_from_blob(&existing)?;
        transaction
            .execute(
                "UPDATE projects
                 SET display_name = ?1, canonical_root = ?2, comparison_root = ?3,
                     vcs_kind = ?4, last_seen_at_us = ?5
                 WHERE id = ?6",
                rusqlite::params![
                    project.display_name,
                    project.canonical_root,
                    project.comparison_root,
                    project.vcs_kind,
                    project.last_seen_at_us,
                    existing,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        project_id
    } else {
        transaction
            .execute(
                "INSERT INTO projects (
                    id, display_name, canonical_root, comparison_root, vcs_kind,
                    created_at_us, last_seen_at_us
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    id_blob(project.id),
                    project.display_name,
                    project.canonical_root,
                    project.comparison_root,
                    project.vcs_kind,
                    project.created_at_us,
                    project.last_seen_at_us,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        project.id
    };
    for root in project.roots {
        transaction
            .execute(
                "INSERT INTO project_roots (
                    id, project_id, canonical_path, comparison_path, root_kind, enabled
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(project_id, comparison_path, root_kind) DO UPDATE SET
                    canonical_path = excluded.canonical_path,
                    enabled = excluded.enabled",
                rusqlite::params![
                    id_blob(root.id),
                    id_blob(project_id),
                    root.canonical_path,
                    root.comparison_path,
                    root.root_kind,
                    i64::from(root.enabled),
                ],
            )
            .map_err(StoreError::Sqlite)?;
    }
    transaction.commit().map_err(StoreError::Sqlite)?;
    Ok(project_id)
}

fn create_session(connection: &mut Connection, session: SessionRecord) -> StoreResult<()> {
    connection
        .execute(
            "INSERT INTO sessions (
                id, project_id, title_preview, state, started_at_us, ended_at_us,
                agent_name, harness_name, provider_name, model_name, source_session_id,
                outcome, outcome_confidence, recovery_coverage, capture_health,
                risk_max_severity, chain_root_id
             ) VALUES (
                :id, :project_id, :title_preview, :state, :started_at_us, :ended_at_us,
                :agent_name, :harness_name, :provider_name, :model_name,
                :source_session_id, :outcome, :outcome_confidence, :recovery_coverage,
                :capture_health, 'none', NULL
             )",
            named_params! {
                ":id": id_blob(session.id),
                ":project_id": optional_id_blob(session.project_id),
                ":title_preview": session.title_preview,
                ":state": session.state,
                ":started_at_us": session.started_at_us,
                ":ended_at_us": session.ended_at_us,
                ":agent_name": session.agent_name,
                ":harness_name": session.harness_name,
                ":provider_name": session.provider_name,
                ":model_name": session.model_name,
                ":source_session_id": session.source_session_id,
                ":outcome": session.outcome,
                ":outcome_confidence": session.outcome_confidence,
                ":recovery_coverage": session.recovery_coverage,
                ":capture_health": session.capture_health,
            },
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn upsert_session(connection: &Connection, session: SessionRecord) -> StoreResult<()> {
    connection
        .execute(
            "INSERT INTO sessions (
                id, project_id, title_preview, state, started_at_us, ended_at_us,
                agent_name, harness_name, provider_name, model_name, source_session_id,
                outcome, outcome_confidence, recovery_coverage, capture_health,
                risk_max_severity, chain_root_id
             ) VALUES (
                :id, :project_id, :title_preview, :state, :started_at_us, :ended_at_us,
                :agent_name, :harness_name, :provider_name, :model_name,
                :source_session_id, :outcome, :outcome_confidence, :recovery_coverage,
                :capture_health, 'none', NULL
             )
             ON CONFLICT(id) DO UPDATE SET
                project_id = COALESCE(excluded.project_id, sessions.project_id),
                title_preview = COALESCE(excluded.title_preview, sessions.title_preview),
                state = CASE WHEN sessions.state IN ('complete', 'archived')
                             THEN sessions.state ELSE excluded.state END,
                started_at_us = MIN(sessions.started_at_us, excluded.started_at_us),
                ended_at_us = COALESCE(excluded.ended_at_us, sessions.ended_at_us),
                agent_name = COALESCE(excluded.agent_name, sessions.agent_name),
                harness_name = COALESCE(excluded.harness_name, sessions.harness_name),
                provider_name = COALESCE(excluded.provider_name, sessions.provider_name),
                model_name = COALESCE(excluded.model_name, sessions.model_name),
                source_session_id = COALESCE(excluded.source_session_id, sessions.source_session_id)",
            named_params! {
                ":id": id_blob(session.id),
                ":project_id": optional_id_blob(session.project_id),
                ":title_preview": session.title_preview,
                ":state": session.state,
                ":started_at_us": session.started_at_us,
                ":ended_at_us": session.ended_at_us,
                ":agent_name": session.agent_name,
                ":harness_name": session.harness_name,
                ":provider_name": session.provider_name,
                ":model_name": session.model_name,
                ":source_session_id": session.source_session_id,
                ":outcome": session.outcome,
                ":outcome_confidence": session.outcome_confidence,
                ":recovery_coverage": session.recovery_coverage,
                ":capture_health": session.capture_health,
            },
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn upsert_adapter_installation(
    connection: &Connection,
    installation: AdapterInstallationRecord,
) -> StoreResult<()> {
    connection
        .execute(
            "INSERT INTO adapter_installations (
                id, adapter_id, display_name, agent_version, detected_at_us,
                last_seen_at_us, status, diagnostic_code
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(adapter_id) DO UPDATE SET
                display_name = excluded.display_name,
                agent_version = COALESCE(excluded.agent_version, adapter_installations.agent_version),
                last_seen_at_us = excluded.last_seen_at_us,
                status = excluded.status,
                diagnostic_code = excluded.diagnostic_code",
            rusqlite::params![
                id_blob(installation.id),
                installation.adapter_id,
                installation.display_name,
                installation.agent_version,
                installation.detected_at_us,
                installation.last_seen_at_us,
                installation.status,
                installation.diagnostic_code,
            ],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn upsert_import_source(connection: &Connection, source: ImportSourceRecord) -> StoreResult<()> {
    connection
        .execute(
            "INSERT INTO import_sources (
                id, adapter_installation_id, source_kind, canonical_location,
                stable_identity, status, last_error_code
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(adapter_installation_id, stable_identity) DO UPDATE SET
                canonical_location = excluded.canonical_location,
                source_kind = excluded.source_kind,
                status = excluded.status,
                last_error_code = excluded.last_error_code",
            rusqlite::params![
                id_blob(source.id),
                id_blob(source.adapter_installation_id),
                source.source_kind,
                source.canonical_location,
                source.stable_identity,
                source.status,
                source.last_error_code,
            ],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn save_import_cursor(connection: &Connection, cursor: ImportCursorRecord) -> StoreResult<()> {
    connection
        .execute(
            "INSERT INTO import_cursors (
                import_source_id, cursor_version, byte_offset, native_cursor,
                source_size, source_mtime_us, last_event_id, updated_at_us
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(import_source_id) DO UPDATE SET
                cursor_version = excluded.cursor_version,
                byte_offset = excluded.byte_offset,
                native_cursor = excluded.native_cursor,
                source_size = excluded.source_size,
                source_mtime_us = excluded.source_mtime_us,
                last_event_id = excluded.last_event_id,
                updated_at_us = excluded.updated_at_us",
            rusqlite::params![
                id_blob(cursor.import_source_id),
                i64::from(cursor.cursor_version),
                u64_to_i64(cursor.byte_offset)?,
                cursor.native_cursor,
                cursor.source_size.map(u64_to_i64).transpose()?,
                cursor.source_mtime_us,
                optional_id_blob(cursor.last_event_id),
                cursor.updated_at_us,
            ],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn save_adapter_hook(connection: &Connection, hook: AdapterHookRecord) -> StoreResult<()> {
    connection
        .execute(
            "INSERT INTO adapter_hooks (
                id, adapter_id, installation_id, config_path, backup_path,
                plan_digest, installed_at_us
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(adapter_id) DO UPDATE SET
                installation_id = excluded.installation_id,
                config_path = excluded.config_path,
                backup_path = excluded.backup_path,
                plan_digest = excluded.plan_digest,
                installed_at_us = excluded.installed_at_us",
            rusqlite::params![
                id_blob(hook.id),
                hook.adapter_id,
                optional_id_blob(hook.installation_id),
                hook.config_path.to_string_lossy(),
                hook.backup_path.to_string_lossy(),
                hook.plan_digest,
                hook.installed_at_us,
            ],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn delete_adapter_hook(connection: &Connection, adapter_id: &str) -> StoreResult<bool> {
    let deleted = connection
        .execute(
            "DELETE FROM adapter_hooks WHERE adapter_id = ?1",
            [adapter_id],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(deleted > 0)
}

fn delete_project_data(connection: &mut Connection, project_id: EntityId) -> StoreResult<u64> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StoreError::Sqlite)?;
    let project = id_blob(project_id);
    transaction
        .execute(
            "INSERT OR REPLACE INTO deletion_context(id, project_id) VALUES (1, ?1)",
            [project.as_slice()],
        )
        .map_err(StoreError::Sqlite)?;
    for statement in [
        "DELETE FROM correlation_conflicts
         WHERE left_event_id IN (SELECT id FROM events WHERE project_id = ?1)
            OR right_event_id IN (SELECT id FROM events WHERE project_id = ?1)",
        "DELETE FROM correlations
         WHERE reported_event_id IN (SELECT id FROM events WHERE project_id = ?1)
            OR observed_event_id IN (SELECT id FROM events WHERE project_id = ?1)",
        "DELETE FROM risk_findings
         WHERE event_id IN (SELECT id FROM events WHERE project_id = ?1)",
        "DELETE FROM event_sources
         WHERE event_id IN (SELECT id FROM events WHERE project_id = ?1)",
        "DELETE FROM event_session_links
         WHERE event_id IN (SELECT id FROM events WHERE project_id = ?1)",
        "DELETE FROM chain_entries
         WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
        "DELETE FROM recovery_runs
         WHERE plan_id IN (SELECT id FROM recovery_plans WHERE session_id IN
             (SELECT id FROM sessions WHERE project_id = ?1))",
        "DELETE FROM recovery_plans
         WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
        "DELETE FROM git_states WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
        "DELETE FROM file_versions WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
        "DELETE FROM snapshot_entries WHERE snapshot_id IN
             (SELECT id FROM snapshots WHERE project_id = ?1)",
        "DELETE FROM snapshots WHERE project_id = ?1",
    ] {
        transaction
            .execute(statement, [project.as_slice()])
            .map_err(StoreError::Sqlite)?;
    }
    let deleted_events = u64::try_from(
        transaction
            .execute(
                "DELETE FROM events WHERE project_id = ?1",
                [project.as_slice()],
            )
            .map_err(StoreError::Sqlite)?,
    )
    .map_err(|_| StoreError::InvalidStoredValue("deleted event count"))?;
    transaction
        .execute(
            "INSERT INTO deletion_audit(
                id, target_kind, target_id, event_count, created_at_us
             ) VALUES (?1, 'project', ?2, ?3, ?4)",
            rusqlite::params![
                id_blob(EntityId::new()),
                project_id.to_string(),
                u64_to_i64(deleted_events)?,
                migration::current_time_us(),
            ],
        )
        .map_err(StoreError::Sqlite)?;
    transaction
        .execute("DELETE FROM deletion_context WHERE id = 1", [])
        .map_err(StoreError::Sqlite)?;
    transaction
        .execute(
            "DELETE FROM sessions WHERE project_id = ?1",
            [project.as_slice()],
        )
        .map_err(StoreError::Sqlite)?;
    transaction
        .execute(
            "DELETE FROM files WHERE project_id = ?1",
            [project.as_slice()],
        )
        .map_err(StoreError::Sqlite)?;
    transaction
        .execute(
            "DELETE FROM project_roots WHERE project_id = ?1",
            [project.as_slice()],
        )
        .map_err(StoreError::Sqlite)?;
    transaction
        .execute("DELETE FROM projects WHERE id = ?1", [project.as_slice()])
        .map_err(StoreError::Sqlite)?;
    transaction.commit().map_err(StoreError::Sqlite)?;
    Ok(deleted_events)
}

fn complete_session(
    connection: &Connection,
    session_id: EntityId,
    ended_at_us: i64,
    outcome: &str,
    capture_health: &str,
) -> StoreResult<()> {
    let updated = connection
        .execute(
            "UPDATE sessions
             SET state = 'complete', ended_at_us = ?1, outcome = ?2, capture_health = ?3
             WHERE id = ?4",
            rusqlite::params![ended_at_us, outcome, capture_health, id_blob(session_id),],
        )
        .map_err(StoreError::Sqlite)?;
    if updated == 0 {
        return Err(StoreError::InvalidStoredValue("session row does not exist"));
    }
    Ok(())
}

fn save_snapshot(connection: &mut Connection, snapshot: SnapshotRecord) -> StoreResult<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StoreError::Sqlite)?;
    transaction
        .execute(
            "INSERT INTO snapshots (
                id, project_id, session_id, snapshot_kind, coverage, started_at_us,
                completed_at_us, manifest_hash, status
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                completed_at_us = excluded.completed_at_us,
                manifest_hash = excluded.manifest_hash,
                status = excluded.status",
            rusqlite::params![
                id_blob(snapshot.id),
                id_blob(snapshot.project_id),
                optional_id_blob(snapshot.session_id),
                snapshot.snapshot_kind,
                snapshot.coverage,
                snapshot.started_at_us,
                snapshot.completed_at_us,
                snapshot.manifest_hash,
                snapshot.status,
            ],
        )
        .map_err(StoreError::Sqlite)?;
    for entry in snapshot.entries {
        transaction
            .execute(
                "INSERT INTO snapshot_entries (
                    snapshot_id, file_id, display_path, comparison_path, file_version_id,
                    entry_kind, capture_status, git_object_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(snapshot_id, comparison_path) DO UPDATE SET
                    file_id = excluded.file_id,
                    file_version_id = excluded.file_version_id,
                    entry_kind = excluded.entry_kind,
                    capture_status = excluded.capture_status,
                    git_object_id = excluded.git_object_id",
                rusqlite::params![
                    id_blob(snapshot.id),
                    optional_id_blob(entry.file_id),
                    entry.display_path,
                    entry.comparison_path,
                    optional_id_blob(entry.file_version_id),
                    entry.entry_kind,
                    entry.capture_status,
                    entry.git_object_id,
                ],
            )
            .map_err(StoreError::Sqlite)?;
    }
    transaction.commit().map_err(StoreError::Sqlite)
}

fn save_git_state(connection: &Connection, state: GitStateRecord) -> StoreResult<()> {
    connection
        .execute(
            "INSERT INTO git_states (
                id, session_id, snapshot_phase, repo_root, head_oid, branch_name,
                upstream_name, status_blob_id, captured_at_us
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                id_blob(state.id),
                id_blob(state.session_id),
                state.snapshot_phase,
                state.repo_root,
                state.head_oid,
                state.branch_name,
                state.upstream_name,
                state.status_blob_id,
                state.captured_at_us,
            ],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn upsert_file(connection: &mut Connection, file: FileRecord) -> StoreResult<EntityId> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StoreError::Sqlite)?;
    let existing = transaction
        .query_row(
            "SELECT id FROM files WHERE project_id = ?1 AND current_comparison_path = ?2",
            rusqlite::params![
                id_blob(file.project_id),
                file.current_comparison_path.as_str()
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(StoreError::Sqlite)?;
    let file_id = if let Some(existing) = existing {
        let file_id = uuid_from_blob(&existing)?;
        transaction
            .execute(
                "UPDATE files SET current_display_path = ?1, stable_file_identity = COALESCE(?2, stable_file_identity),
                    last_seen_at_us = ?3, sensitive_class = ?4 WHERE id = ?5",
                rusqlite::params![
                    file.current_display_path,
                    file.stable_file_identity,
                    file.last_seen_at_us,
                    file.sensitive_class,
                    existing,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        file_id
    } else {
        transaction
            .execute(
                "INSERT INTO files (
                    id, project_id, current_display_path, current_comparison_path,
                    stable_file_identity, first_seen_at_us, last_seen_at_us, sensitive_class
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    id_blob(file.id),
                    id_blob(file.project_id),
                    file.current_display_path,
                    file.current_comparison_path,
                    file.stable_file_identity,
                    file.first_seen_at_us,
                    file.last_seen_at_us,
                    file.sensitive_class,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        file.id
    };
    transaction.commit().map_err(StoreError::Sqlite)?;
    Ok(file_id)
}

fn insert_file_version(connection: &Connection, version: FileVersionRecord) -> StoreResult<()> {
    connection
        .execute(
            "INSERT INTO file_versions (
                id, file_id, session_id, event_id, display_path_at_time, content_hash,
                blob_id, byte_length, media_kind, executable, symlink_target,
                capture_status, created_at_us
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                id_blob(version.id),
                id_blob(version.file_id),
                optional_id_blob(version.session_id),
                optional_id_blob(version.event_id),
                version.display_path_at_time,
                version.content_hash,
                version.blob_id,
                version.byte_length.map(u64_to_i64).transpose()?,
                version.media_kind,
                version.executable.map(i64::from),
                version.symlink_target,
                version.capture_status,
                version.created_at_us,
            ],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn save_risk_findings(
    connection: &mut Connection,
    findings: Vec<RiskFindingRecord>,
) -> StoreResult<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StoreError::Sqlite)?;
    for finding in findings {
        transaction
            .execute(
                "INSERT INTO risk_findings (
                    id, session_id, event_id, rule_id, rule_version, severity,
                    initial_status, title, explanation, matched_preview, created_at_us
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO NOTHING",
                rusqlite::params![
                    id_blob(finding.id),
                    optional_id_blob(finding.session_id),
                    id_blob(finding.event_id),
                    finding.rule_id,
                    finding.rule_version,
                    finding.severity,
                    finding.initial_status,
                    finding.title,
                    finding.explanation,
                    finding.matched_preview,
                    finding.created_at_us,
                ],
            )
            .map_err(StoreError::Sqlite)?;
    }
    transaction.commit().map_err(StoreError::Sqlite)
}

fn save_recovery_plan(connection: &Connection, plan: RecoveryPlanRecord) -> StoreResult<()> {
    connection
        .execute(
            "INSERT INTO recovery_plans (
                id, session_id, action, destination, status, plan_digest,
                plan_blob_id, created_at_us, executed_at_us, result_blob_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                status = excluded.status,
                executed_at_us = excluded.executed_at_us,
                result_blob_id = excluded.result_blob_id",
            rusqlite::params![
                id_blob(plan.id),
                id_blob(plan.session_id),
                plan.action,
                plan.destination,
                plan.status,
                plan.plan_digest,
                plan.plan_blob_id,
                plan.created_at_us,
                plan.executed_at_us,
                plan.result_blob_id,
            ],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn save_recovery_run(connection: &Connection, run: RecoveryRunRecord) -> StoreResult<()> {
    connection
        .execute(
            "INSERT INTO recovery_runs (
                id, plan_id, state, restored_files, skipped_files, conflict_files,
                result_blob_id, created_at_us, finished_at_us, error_code
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                state = excluded.state,
                restored_files = excluded.restored_files,
                skipped_files = excluded.skipped_files,
                conflict_files = excluded.conflict_files,
                result_blob_id = excluded.result_blob_id,
                finished_at_us = excluded.finished_at_us,
                error_code = excluded.error_code",
            rusqlite::params![
                id_blob(run.id),
                id_blob(run.plan_id),
                run.state,
                u64_to_i64(run.restored_files)?,
                u64_to_i64(run.skipped_files)?,
                u64_to_i64(run.conflict_files)?,
                run.result_blob_id,
                run.created_at_us,
                run.finished_at_us,
                run.error_code,
            ],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn save_correlation(
    connection: &mut Connection,
    correlation: Option<CorrelationInsert>,
    conflict: Option<CorrelationConflictInsert>,
) -> StoreResult<()> {
    if correlation.is_none() && conflict.is_none() {
        return Ok(());
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StoreError::Sqlite)?;
    if let Some(correlation) = correlation {
        if chain_root_exists(&transaction, correlation.session_id)? {
            return Err(StoreError::ChainFinalized);
        }
        transaction
            .execute(
                "INSERT INTO correlations (
                    id, session_id, logical_event_id, reported_event_id, observed_event_id,
                    correlation_kind, confidence, algorithm_version, created_at_us, supersedes_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)
                 ON CONFLICT(reported_event_id, observed_event_id, algorithm_version) DO NOTHING",
                rusqlite::params![
                    id_blob(correlation.id),
                    optional_id_blob(correlation.session_id),
                    id_blob(correlation.logical_event_id),
                    id_blob(correlation.reported_event_id),
                    id_blob(correlation.observed_event_id),
                    correlation.correlation_kind,
                    correlation.confidence,
                    i64::from(correlation.algorithm_version),
                    migration::current_time_us(),
                ],
            )
            .map_err(StoreError::Sqlite)?;
        let canonical = canonical_digest(&serde_json::json!({
            "id": correlation.id.to_string(),
            "sessionId": correlation.session_id.map(|id| id.to_string()),
            "logicalEventId": correlation.logical_event_id.to_string(),
            "reportedEventId": correlation.reported_event_id.to_string(),
            "observedEventId": correlation.observed_event_id.to_string(),
            "kind": correlation.correlation_kind,
            "confidence": correlation.confidence,
            "algorithmVersion": correlation.algorithm_version,
        }))?;
        append_auxiliary_chain_entry(
            &transaction,
            correlation.session_id,
            "correlation",
            correlation.id.to_string(),
            canonical,
        )?;
    }
    if let Some(conflict) = conflict {
        if chain_root_exists(&transaction, conflict.session_id)? {
            return Err(StoreError::ChainFinalized);
        }
        transaction
            .execute(
                "INSERT INTO correlation_conflicts (
                    id, session_id, left_event_id, right_event_id, reason_code, detail,
                    created_at_us, resolved_by_correlation_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
                rusqlite::params![
                    id_blob(conflict.id),
                    optional_id_blob(conflict.session_id),
                    id_blob(conflict.left_event_id),
                    id_blob(conflict.right_event_id),
                    conflict.reason_code,
                    conflict.detail,
                    migration::current_time_us(),
                ],
            )
            .map_err(StoreError::Sqlite)?;
        let canonical = canonical_digest(&serde_json::json!({
            "id": conflict.id.to_string(),
            "sessionId": conflict.session_id.map(|id| id.to_string()),
            "leftEventId": conflict.left_event_id.to_string(),
            "rightEventId": conflict.right_event_id.to_string(),
            "reasonCode": conflict.reason_code,
            "detail": conflict.detail,
        }))?;
        append_auxiliary_chain_entry(
            &transaction,
            conflict.session_id,
            "conflict",
            conflict.id.to_string(),
            canonical,
        )?;
    }
    transaction.commit().map_err(StoreError::Sqlite)
}

fn append_auxiliary_chain_entry(
    transaction: &Transaction<'_>,
    session_id: Option<EntityId>,
    entry_kind: &str,
    target_id: String,
    canonical: ChainHash,
) -> StoreResult<()> {
    let chain_scope = session_id.map_or_else(|| "global".to_owned(), |id| id.to_string());
    let (sequence, previous_hash) = last_chain_position(transaction, &chain_scope)?;
    let sequence = sequence.saturating_add(1);
    let entry_hash = hash_chain_entry(previous_hash, entry_kind, canonical, None);
    transaction
        .execute(
            "INSERT INTO chain_entries (
                id, session_id, chain_scope, sequence, entry_kind, target_id,
                canonical_digest, raw_payload_digest, previous_hash, entry_hash, created_at_us
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?10)",
            rusqlite::params![
                id_blob(EntityId::new()),
                optional_id_blob(session_id),
                chain_scope,
                u64_to_i64(sequence)?,
                entry_kind,
                target_id,
                canonical.0.as_slice(),
                previous_hash.0.as_slice(),
                entry_hash.0.as_slice(),
                migration::current_time_us(),
            ],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn chain_root_exists(
    transaction: &Transaction<'_>,
    session_id: Option<EntityId>,
) -> StoreResult<bool> {
    let count = transaction
        .query_row(
            "SELECT COUNT(*) FROM chain_roots
             WHERE (?1 IS NULL AND session_id IS NULL) OR session_id = ?1",
            [optional_id_blob(session_id)],
            |row| row.get::<_, i64>(0),
        )
        .map_err(StoreError::Sqlite)?;
    Ok(count > 0)
}

fn existing_chain_root(
    connection: &Connection,
    session_id: Option<EntityId>,
) -> StoreResult<Option<EntityId>> {
    connection
        .query_row(
            "SELECT id FROM chain_roots
             WHERE (?1 IS NULL AND session_id IS NULL) OR session_id = ?1
             ORDER BY finalized_at_us DESC LIMIT 1",
            [optional_id_blob(session_id)],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(StoreError::Sqlite)?
        .map(|bytes| uuid_from_blob(&bytes))
        .transpose()
}

fn last_chain_position(
    transaction: &Transaction<'_>,
    chain_scope: &str,
) -> StoreResult<(u64, ChainHash)> {
    let row = transaction
        .query_row(
            "SELECT sequence, entry_hash FROM chain_entries
             WHERE chain_scope = ?1 ORDER BY sequence DESC LIMIT 1",
            [chain_scope],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .map_err(StoreError::Sqlite)?;
    match row {
        Some((sequence, hash)) => Ok((
            u64::try_from(sequence).map_err(|_| StoreError::InvalidStoredValue("sequence"))?,
            ChainHash(hash_array(&hash)?),
        )),
        None => Ok((0, ChainHash::zero())),
    }
}

fn next_chain_position(
    transaction: &Transaction<'_>,
    chain_scope: &str,
    chain_cursors: &mut HashMap<String, (u64, ChainHash)>,
) -> StoreResult<(u64, ChainHash)> {
    if let Some((sequence, hash)) = chain_cursors.get(chain_scope) {
        return Ok((*sequence, *hash));
    }
    let position = last_chain_position(transaction, chain_scope)?;
    chain_cursors.insert(chain_scope.to_owned(), position);
    Ok(position)
}

fn canonical_event(event: &EventEnvelope) -> StoreResult<EventEnvelope> {
    let mut canonical = event.clone();
    canonical.sequence = None;
    canonical.integrity = None;
    Ok(canonical)
}

fn source_identity_value(event: &EventEnvelope) -> StoreResult<EventEnvelope> {
    let mut comparable = canonical_event(event)?;
    comparable.id = EntityId::nil();
    comparable.observed_at_us = 0;
    comparable.monotonic_ns = None;
    Ok(comparable)
}

fn parse_optional_digest(value: Option<&str>) -> StoreResult<Option<ChainHash>> {
    value
        .map(|value| ChainHash::from_hex(value).map_err(StoreError::Chain))
        .transpose()
}

fn id_blob(id: EntityId) -> [u8; 16] {
    *id.as_uuid().as_bytes()
}

fn optional_id_blob(id: Option<EntityId>) -> Option<[u8; 16]> {
    id.map(id_blob)
}

pub(crate) fn uuid_from_blob(bytes: &[u8]) -> StoreResult<EntityId> {
    let bytes: [u8; 16] = bytes
        .try_into()
        .map_err(|_| StoreError::InvalidStoredValue("UUID length"))?;
    Ok(EntityId::from(uuid::Uuid::from_bytes(bytes)))
}

pub(crate) fn hash_array(bytes: &[u8]) -> StoreResult<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| StoreError::InvalidStoredValue("hash length"))
}

fn optional_u64_to_i64(value: Option<u64>) -> StoreResult<Option<i64>> {
    value.map(u64_to_i64).transpose()
}

fn u64_to_i64(value: u64) -> StoreResult<i64> {
    i64::try_from(value).map_err(|_| StoreError::InvalidStoredValue("integer overflow"))
}
