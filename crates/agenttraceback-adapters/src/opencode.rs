use std::{path::PathBuf, process::Command, time::Duration};

use agenttraceback_adapter_sdk::{
    AdapterDescriptor, AdapterError, AdapterEvent, AdapterSessionMetadata, AgentAdapter,
    Capability, CapabilityAvailability, CapabilitySet, DetectContext, DetectedInstallation,
    EventSink, ImportCursor, ImportOutcome, ImportSource,
};
use agenttraceback_types::{
    AttributionConfidence, EntityId, EventAction, EventEnvelope, EventSource, ResultStatus,
    SourceKind, TargetKind,
};
use async_trait::async_trait;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::common::{deterministic_uuid, string_at, text_content};

const MAX_MESSAGES_PER_BATCH: usize = 500;
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

/// OpenCode local SQLite adapter.
#[derive(Clone, Debug, Default)]
pub struct OpenCodeAdapter;

#[async_trait]
impl AgentAdapter for OpenCodeAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            id: "opencode".to_owned(),
            display_name: "OpenCode".to_owned(),
            schema_version: 1,
            source_locations: vec![PathBuf::from("~/.local/share/opencode/opencode.db")],
            richer_capture_requires_change: false,
            documentation_url: Some("https://opencode.ai/docs/".to_owned()),
        }
    }

    async fn detect(
        &self,
        context: &DetectContext,
    ) -> Result<Vec<DetectedInstallation>, AdapterError> {
        let Some(home) = &context.home_dir else {
            return Ok(Vec::new());
        };
        let database = home.join(".local/share/opencode/opencode.db");
        let executable = context
            .path_entries
            .iter()
            .map(|entry| entry.join("opencode"))
            .find(|path| path.is_file());
        if !database.is_file() && executable.is_none() {
            return Ok(Vec::new());
        }
        let version = executable
            .and_then(|path| Command::new(path).arg("--version").output().ok())
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
        Ok(vec![DetectedInstallation {
            id: EntityId::from(deterministic_uuid("installation:opencode")),
            adapter_id: "opencode".to_owned(),
            display_name: "OpenCode".to_owned(),
            agent_version: version,
            source_roots: vec![database],
            diagnostic_code: None,
        }])
    }

    async fn capabilities(
        &self,
        _installation: &DetectedInstallation,
    ) -> Result<CapabilitySet, AdapterError> {
        Ok(CapabilitySet {
            capabilities: vec![
                capability("historical_sessions", CapabilityAvailability::Available),
                capability("live_sessions", CapabilityAvailability::Available),
                capability("prompts", CapabilityAvailability::Available),
                capability("responses", CapabilityAvailability::Available),
                capability("tool_calls", CapabilityAvailability::Available),
                capability("commands", CapabilityAvailability::Available),
                capability("subagents", CapabilityAvailability::Degraded),
                capability("tokens", CapabilityAvailability::Available),
            ],
        })
    }

    async fn discover_sources(
        &self,
        installation: &DetectedInstallation,
    ) -> Result<Vec<ImportSource>, AdapterError> {
        Ok(installation
            .source_roots
            .iter()
            .filter(|path| path.is_file())
            .map(|path| ImportSource {
                id: EntityId::from(deterministic_uuid(&format!(
                    "source:opencode:{}",
                    path.display()
                ))),
                adapter_id: "opencode".to_owned(),
                canonical_location: path.clone(),
                stable_identity: path.to_string_lossy().into_owned(),
                source_kind: "agent_database".to_owned(),
            })
            .collect())
    }

    async fn import(
        &self,
        source: &ImportSource,
        cursor: Option<ImportCursor>,
        sink: &dyn EventSink,
    ) -> Result<ImportOutcome, AdapterError> {
        import_source(source, cursor, sink).await
    }

    async fn tail(
        &self,
        source: &ImportSource,
        cursor: ImportCursor,
        sink: &dyn EventSink,
        cancel: CancellationToken,
    ) -> Result<(), AdapterError> {
        let mut cursor = cursor;
        loop {
            if cancel.is_cancelled() {
                return Ok(());
            }
            let outcome = import_source(source, Some(cursor), sink).await?;
            cursor = outcome.cursor;
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
    }
}

async fn import_source(
    source: &ImportSource,
    cursor: Option<ImportCursor>,
    sink: &dyn EventSink,
) -> Result<ImportOutcome, AdapterError> {
    let cursor = cursor.unwrap_or_default();
    let batch = load_batch(&source.canonical_location, cursor.native_cursor.as_deref())?;
    for event in &batch.events {
        sink.emit(event.clone()).await?;
    }
    Ok(ImportOutcome {
        events_imported: batch.events.len() as u64,
        cursor: ImportCursor {
            version: 1,
            byte_offset: 0,
            native_cursor: Some(batch.final_cursor),
            source_size: None,
            source_mtime_us: None,
            last_event_id: batch.last_event_id,
        },
        parser_version: "opencode-sqlite-v1".to_owned(),
        quarantined: batch.quarantined,
        warnings: batch.warnings,
    })
}

struct LoadedBatch {
    events: Vec<AdapterEvent>,
    final_cursor: String,
    last_event_id: Option<EntityId>,
    quarantined: u64,
    warnings: Vec<String>,
}

fn load_batch(
    path: &std::path::Path,
    native_cursor: Option<&str>,
) -> Result<LoadedBatch, AdapterError> {
    let (time, id) = parse_cursor(native_cursor);
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(db_error)?;
    let mut statement = connection
        .prepare(
            "SELECT m.id, m.session_id, m.time_created, m.data
             FROM message m
             WHERE m.time_created > ?1 OR (m.time_created = ?1 AND m.id > ?2)
             ORDER BY m.time_created, m.id LIMIT ?3",
        )
        .map_err(db_error)?;
    let messages = statement
        .query_map(
            rusqlite::params![time, id, MAX_MESSAGES_PER_BATCH as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let mut last_event_id = None;
    let mut final_cursor = native_cursor.map(str::to_owned);
    let mut pending_events = Vec::new();
    let mut quarantined = 0_u64;
    let mut warnings = Vec::new();
    for (message_id, session_id, time_created, data) in &messages {
        final_cursor = Some(format!("{time_created}:{message_id}"));
        if data.len() > MAX_RECORD_BYTES {
            quarantined += 1;
            warnings.push(format!(
                "quarantined OpenCode message {message_id}: record exceeds {MAX_RECORD_BYTES} bytes"
            ));
            continue;
        }
        let data: Value = match serde_json::from_str(data) {
            Ok(data) => data,
            Err(error) => {
                quarantined += 1;
                warnings.push(format!(
                    "quarantined OpenCode message {message_id}: {error}"
                ));
                continue;
            }
        };
        let session = load_session(&connection, session_id)?;
        let metadata = AdapterSessionMetadata {
            native_session_id: session_id.clone(),
            project_root: session
                .as_ref()
                .and_then(|(_, directory, _, _, _)| directory.clone()),
            title_preview: session
                .as_ref()
                .and_then(|(_, _, title, _, _)| title.clone()),
            agent_name: "opencode".to_owned(),
            harness_name: Some("opencode".to_owned()),
            provider_name: session
                .as_ref()
                .and_then(|(_, _, _, _, provider)| provider.clone()),
            model_name: session
                .as_ref()
                .and_then(|(_, _, _, model, _)| model.clone()),
            historical: true,
        };
        let role = string_at(&data, &["role"]).unwrap_or("assistant");
        let action = if role == "user" {
            EventAction::Prompt
        } else {
            EventAction::Response
        };
        let preview = data
            .get("content")
            .and_then(text_content)
            .or_else(|| string_at(&data, &["text"]).map(str::to_owned));
        let event = make_event(
            message_id,
            session_id,
            *time_created,
            action,
            None,
            preview,
            None,
        );
        last_event_id = Some(event.event.id);
        pending_events.push(AdapterEvent {
            event: event.event,
            raw_payload: Some(data.to_string().into_bytes()),
            session: Some(metadata.clone()),
        });
        let mut part_statement = connection
            .prepare(
                "SELECT id, data, time_created FROM part
                 WHERE message_id = ?1 ORDER BY time_created, id",
            )
            .map_err(db_error)?;
        let parts = part_statement
            .query_map([message_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        for (part_id, part_data, part_time) in parts {
            if part_data.len() > MAX_RECORD_BYTES {
                quarantined += 1;
                warnings.push(format!(
                    "quarantined OpenCode part {part_id}: record exceeds {MAX_RECORD_BYTES} bytes"
                ));
                continue;
            }
            let value: Value = match serde_json::from_str(&part_data) {
                Ok(value) => value,
                Err(error) => {
                    quarantined += 1;
                    warnings.push(format!("quarantined OpenCode part {part_id}: {error}"));
                    continue;
                }
            };
            let part_type = string_at(&value, &["type"]).unwrap_or_default();
            let (action, target_kind, target, preview) = match part_type {
                "text" => (
                    if role == "user" {
                        EventAction::Prompt
                    } else {
                        EventAction::Response
                    },
                    TargetKind::Session,
                    None,
                    string_at(&value, &["text"]).map(str::to_owned),
                ),
                "tool" | "patch" => {
                    let tool = string_at(&value, &["tool"]).unwrap_or("tool").to_owned();
                    let command = value
                        .get("state")
                        .and_then(|state| state.get("input"))
                        .and_then(|input| input.get("command"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    (
                        match tool.as_str() {
                            "bash" => EventAction::CommandExecute,
                            "edit" | "write" | "patch" => EventAction::FileWrite,
                            "read" => EventAction::FileRead,
                            _ => EventAction::ToolCall,
                        },
                        if matches!(tool.as_str(), "edit" | "write" | "patch" | "read") {
                            TargetKind::File
                        } else {
                            TargetKind::Command
                        },
                        command.or(Some(tool)),
                        Some(value.to_string()),
                    )
                }
                _ => continue,
            };
            let event = make_event(
                &format!("{message_id}:{part_id}"),
                session_id,
                part_time,
                action,
                Some(target_kind),
                preview,
                target,
            );
            last_event_id = Some(event.event.id);
            pending_events.push(AdapterEvent {
                event: event.event,
                raw_payload: Some(part_data.into_bytes()),
                session: Some(metadata.clone()),
            });
        }
        drop(part_statement);
    }
    drop(statement);
    drop(connection);
    Ok(LoadedBatch {
        events: pending_events,
        final_cursor: final_cursor.unwrap_or_default(),
        last_event_id,
        quarantined,
        warnings,
    })
}

type SessionMetadataRow = (
    String,
    Option<PathBuf>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn load_session(
    connection: &Connection,
    session_id: &str,
) -> Result<Option<SessionMetadataRow>, AdapterError> {
    connection
        .query_row(
            "SELECT s.directory, s.title, s.model, s.project_id
             FROM session s WHERE s.id = ?1",
            [session_id],
            |row| {
                let directory = row.get::<_, String>(0)?;
                let title = row.get::<_, String>(1)?;
                let model = row.get::<_, Option<String>>(2)?;
                let project_id = row.get::<_, String>(3)?;
                Ok((
                    project_id,
                    Some(PathBuf::from(directory)),
                    Some(title),
                    model.clone(),
                    model,
                ))
            },
        )
        .optional()
        .map_err(db_error)
}

fn make_event(
    id_suffix: &str,
    session_id: &str,
    timestamp_ms: i64,
    action: EventAction,
    target_kind: Option<TargetKind>,
    preview: Option<String>,
    target: Option<String>,
) -> AdapterEvent {
    let timestamp_us = timestamp_ms.saturating_mul(1_000);
    let mut event = EventEnvelope::new(
        EventSource {
            kind: SourceKind::AgentLog,
            original_source_kind: None,
            adapter_id: Some("opencode".to_owned()),
            source_event_id: format!("opencode:{session_id}:{id_suffix}"),
            raw_blob_id: None,
        },
        action,
        timestamp_us,
        timestamp_us,
    );
    event.session_id = Some(EntityId::from(deterministic_uuid(&format!(
        "opencode:{session_id}"
    ))));
    event.actor.agent = Some("opencode".to_owned());
    event.target.kind = target_kind.unwrap_or(TargetKind::Session);
    event.target.display = target.clone();
    event.target.normalized_path = matches!(event.target.kind, TargetKind::File)
        .then_some(target)
        .flatten();
    event.content.redacted_preview = preview;
    event.result.status = ResultStatus::Unknown;
    event.evidence.attribution = AttributionConfidence::Exact;
    AdapterEvent {
        event,
        raw_payload: None,
        session: None,
    }
}

fn parse_cursor(cursor: Option<&str>) -> (i64, String) {
    let Some(cursor) = cursor else {
        return (0, String::new());
    };
    let Some((time, id)) = cursor.split_once(':') else {
        return (0, String::new());
    };
    (time.parse().unwrap_or(0), id.to_owned())
}

fn db_error(error: rusqlite::Error) -> AdapterError {
    AdapterError::Parse(error.to_string())
}

fn capability(name: &str, availability: CapabilityAvailability) -> Capability {
    Capability {
        name: name.to_owned(),
        availability,
        detail: None,
    }
}
