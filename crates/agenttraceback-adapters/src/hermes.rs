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
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::common::{deterministic_uuid, string_at};

const MAX_MESSAGES_PER_BATCH: usize = 1_000;
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

/// Hermes Agent local state adapter.
#[derive(Clone, Debug, Default)]
pub struct HermesAdapter;

#[async_trait]
impl AgentAdapter for HermesAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            id: "hermes".to_owned(),
            display_name: "Hermes Agent".to_owned(),
            schema_version: 1,
            source_locations: vec![PathBuf::from("~/.hermes/state.db")],
            richer_capture_requires_change: false,
            documentation_url: Some("https://github.com/NousResearch/hermes-agent".to_owned()),
        }
    }

    async fn detect(
        &self,
        context: &DetectContext,
    ) -> Result<Vec<DetectedInstallation>, AdapterError> {
        let Some(home) = &context.home_dir else {
            return Ok(Vec::new());
        };
        let database = home.join(".hermes/state.db");
        let executable = context
            .path_entries
            .iter()
            .map(|entry| entry.join("hermes"))
            .find(|path| path.is_file());
        if !database.is_file() && executable.is_none() {
            return Ok(Vec::new());
        }
        let version = executable
            .and_then(|path| Command::new(path).arg("--version").output().ok())
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
        Ok(vec![DetectedInstallation {
            id: EntityId::from(deterministic_uuid("installation:hermes")),
            adapter_id: "hermes".to_owned(),
            display_name: "Hermes Agent".to_owned(),
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
                capability("cost", CapabilityAvailability::Available),
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
                    "source:hermes:{}",
                    path.display()
                ))),
                adapter_id: "hermes".to_owned(),
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
                _ = tokio::time::sleep(Duration::from_millis(250)) => {}
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
    let last_id = cursor
        .native_cursor
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let batch = load_batch(&source.canonical_location, last_id)?;
    for event in &batch.events {
        sink.emit(event.clone()).await?;
    }
    Ok(ImportOutcome {
        events_imported: batch.events.len() as u64,
        cursor: ImportCursor {
            version: 1,
            byte_offset: 0,
            native_cursor: Some(batch.latest_id.to_string()),
            source_size: None,
            source_mtime_us: None,
            last_event_id: batch.last_event_id,
        },
        parser_version: "hermes-sqlite-v1".to_owned(),
        quarantined: batch.quarantined,
        warnings: batch.warnings,
    })
}

struct LoadedBatch {
    events: Vec<AdapterEvent>,
    latest_id: i64,
    last_event_id: Option<EntityId>,
    quarantined: u64,
    warnings: Vec<String>,
}

fn load_batch(path: &std::path::Path, last_id: i64) -> Result<LoadedBatch, AdapterError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(db_error)?;
    let mut statement = connection
        .prepare(
            "SELECT m.id, m.session_id, m.role, m.content, m.tool_name, m.tool_calls,
                    m.finish_reason, s.cwd, s.title, s.model, s.parent_session_id,
                    s.started_at
             FROM messages m JOIN sessions s ON s.id = m.session_id
             WHERE m.id > ?1 ORDER BY m.id LIMIT ?2",
        )
        .map_err(db_error)?;
    let rows = statement
        .query_map(
            rusqlite::params![last_id, MAX_MESSAGES_PER_BATCH as i64],
            |row| {
                Ok(HermesRow {
                    id: row.get(0)?,
                    native_session_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    tool_name: row.get(4)?,
                    tool_calls: row.get(5)?,
                    finish_reason: row.get(6)?,
                    cwd: row.get(7)?,
                    title: row.get(8)?,
                    model: row.get(9)?,
                    parent_session_id: row.get(10)?,
                    started_at: row.get(11)?,
                })
            },
        )
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let mut events = Vec::new();
    let mut latest_id = last_id;
    let mut last_event_id = None;
    let mut quarantined = 0_u64;
    let mut warnings = Vec::new();
    for row in rows {
        latest_id = latest_id.max(row.id);
        let record_size = row.content.as_ref().map_or(0, String::len)
            + row.tool_calls.as_ref().map_or(0, String::len);
        if record_size > MAX_RECORD_BYTES {
            quarantined += 1;
            warnings.push(format!(
                "quarantined Hermes message {}: record exceeds {MAX_RECORD_BYTES} bytes",
                row.id
            ));
            continue;
        }
        let timestamp_us = (row.started_at * 1_000_000.0) as i64;
        let action = match row.role.as_str() {
            "user" => EventAction::Prompt,
            "assistant" => EventAction::Response,
            "tool" => EventAction::ToolResult,
            _ => continue,
        };
        let metadata = AdapterSessionMetadata {
            native_session_id: row.native_session_id.clone(),
            project_root: row.cwd.map(PathBuf::from),
            title_preview: row.title,
            agent_name: "hermes".to_owned(),
            harness_name: Some("hermes-agent".to_owned()),
            provider_name: None,
            model_name: row.model,
            historical: true,
        };
        let mut event = make_event(
            &format!("message:{}", row.id),
            &row.native_session_id,
            timestamp_us,
            action,
            row.content.or_else(|| row.tool_name.clone()),
            row.finish_reason.as_deref(),
        );
        event.event.actor.subagent_id = row.parent_session_id;
        last_event_id = Some(event.event.id);
        events.push(AdapterEvent {
            event: event.event,
            raw_payload: None,
            session: Some(metadata.clone()),
        });
        if let Some(tool_calls) = row.tool_calls {
            let calls = match serde_json::from_str::<Value>(&tool_calls) {
                Ok(Value::Array(calls)) => calls,
                Ok(_) => {
                    quarantined += 1;
                    warnings.push(format!(
                        "quarantined Hermes tool calls for message {}: expected an array",
                        row.id
                    ));
                    Vec::new()
                }
                Err(error) => {
                    quarantined += 1;
                    warnings.push(format!(
                        "quarantined Hermes tool calls for message {}: {error}",
                        row.id
                    ));
                    Vec::new()
                }
            };
            for (index, call) in calls.into_iter().enumerate() {
                let tool_name = string_at(&call, &["function", "name"])
                    .or_else(|| string_at(&call, &["name"]))
                    .unwrap_or("tool");
                let arguments = call
                    .get("function")
                    .and_then(|function| function.get("arguments"))
                    .or_else(|| call.get("arguments"))
                    .map(Value::to_string);
                let action = if tool_name == "bash" {
                    EventAction::CommandExecute
                } else {
                    EventAction::ToolCall
                };
                let mut tool_event = make_event(
                    &format!("message:{}:tool:{index}", row.id),
                    &row.native_session_id,
                    timestamp_us.saturating_add(index as i64),
                    action,
                    arguments,
                    None,
                );
                tool_event.event.target.kind = if action == EventAction::CommandExecute {
                    TargetKind::Command
                } else {
                    TargetKind::Unknown
                };
                tool_event.event.target.display = Some(tool_name.to_owned());
                last_event_id = Some(tool_event.event.id);
                events.push(AdapterEvent {
                    event: tool_event.event,
                    raw_payload: None,
                    session: Some(metadata.clone()),
                });
            }
        }
    }
    Ok(LoadedBatch {
        events,
        latest_id,
        last_event_id,
        quarantined,
        warnings,
    })
}

struct HermesRow {
    id: i64,
    native_session_id: String,
    role: String,
    content: Option<String>,
    tool_name: Option<String>,
    tool_calls: Option<String>,
    finish_reason: Option<String>,
    cwd: Option<String>,
    title: Option<String>,
    model: Option<String>,
    parent_session_id: Option<String>,
    started_at: f64,
}

fn make_event(
    id_suffix: &str,
    native_session_id: &str,
    timestamp_us: i64,
    action: EventAction,
    preview: Option<String>,
    finish_reason: Option<&str>,
) -> AdapterEvent {
    let mut event = EventEnvelope::new(
        EventSource {
            kind: SourceKind::AgentLog,
            original_source_kind: None,
            adapter_id: Some("hermes".to_owned()),
            source_event_id: format!("hermes:{native_session_id}:{id_suffix}"),
            raw_blob_id: None,
        },
        action,
        timestamp_us,
        timestamp_us,
    );
    event.session_id = Some(EntityId::from(deterministic_uuid(&format!(
        "hermes:{native_session_id}"
    ))));
    event.actor.agent = Some("hermes".to_owned());
    event.target.kind = TargetKind::Session;
    event.content.redacted_preview = preview;
    event.result.status = match finish_reason {
        Some("stop" | "end_turn") => ResultStatus::Success,
        Some("error") => ResultStatus::Failed,
        _ => ResultStatus::Unknown,
    };
    event.evidence.attribution = AttributionConfidence::Exact;
    AdapterEvent {
        event,
        raw_payload: None,
        session: None,
    }
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
