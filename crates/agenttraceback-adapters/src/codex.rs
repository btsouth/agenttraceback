use std::{path::PathBuf, process::Command, time::Duration};

use agenttraceback_adapter_sdk::{
    AdapterDescriptor, AdapterError, AdapterEvent, AdapterSessionMetadata, AgentAdapter,
    Capability, CapabilityAvailability, CapabilitySet, DetectContext, DetectedInstallation,
    EventSink, ImportCursor, ImportOutcome, ImportSource,
};
use agenttraceback_types::{
    EntityId, EventAction, EventEnvelope, EventSource, ResultStatus, SourceKind, TargetKind,
};
use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::common::{
    deterministic_uuid, read_jsonl, source_prefix_digest, string_at, text_content, timestamp_us,
};

const MAX_RECORDS_PER_BATCH: usize = 1_000;

/// OpenAI Codex CLI adapter.
#[derive(Clone, Debug, Default)]
pub struct CodexCliAdapter;

#[async_trait]
impl AgentAdapter for CodexCliAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            id: "codex".to_owned(),
            display_name: "Codex CLI".to_owned(),
            schema_version: 1,
            source_locations: vec![PathBuf::from("~/.codex/sessions")],
            richer_capture_requires_change: false,
            documentation_url: Some("https://github.com/openai/codex".to_owned()),
        }
    }

    async fn detect(
        &self,
        context: &DetectContext,
    ) -> Result<Vec<DetectedInstallation>, AdapterError> {
        let Some(home) = &context.home_dir else {
            return Ok(Vec::new());
        };
        let source_root = home.join(".codex/sessions");
        let executable = context
            .path_entries
            .iter()
            .map(|entry| entry.join("codex"))
            .find(|path| path.is_file());
        if !source_root.is_dir() && executable.is_none() {
            return Ok(Vec::new());
        }
        let version = executable
            .and_then(|path| Command::new(path).arg("--version").output().ok())
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
        Ok(vec![DetectedInstallation {
            id: EntityId::from(deterministic_uuid("installation:codex")),
            adapter_id: "codex".to_owned(),
            display_name: "Codex CLI".to_owned(),
            agent_version: version,
            source_roots: vec![source_root],
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
                capability("subagents", CapabilityAvailability::Unavailable),
                capability("tokens", CapabilityAvailability::Degraded),
                capability("file_reads", CapabilityAvailability::Degraded),
                capability("file_writes", CapabilityAvailability::Degraded),
            ],
        })
    }

    async fn discover_sources(
        &self,
        installation: &DetectedInstallation,
    ) -> Result<Vec<ImportSource>, AdapterError> {
        let mut sources = Vec::new();
        for root in &installation.source_roots {
            if !root.is_dir() {
                continue;
            }
            for entry in walkdir::WalkDir::new(root).follow_links(false).max_depth(8) {
                let entry = entry.map_err(|error| AdapterError::Parse(error.to_string()))?;
                if entry.file_type().is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "jsonl")
                {
                    let canonical = entry.path().to_path_buf();
                    sources.push(ImportSource {
                        id: EntityId::from(deterministic_uuid(&format!(
                            "source:codex:{}",
                            canonical.display()
                        ))),
                        adapter_id: "codex".to_owned(),
                        stable_identity: canonical.to_string_lossy().into_owned(),
                        canonical_location: canonical,
                        source_kind: "agent_log".to_owned(),
                    });
                }
            }
        }
        sources.sort_by(|left, right| left.canonical_location.cmp(&right.canonical_location));
        Ok(sources)
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
    let mut cursor = cursor.unwrap_or_default();
    let mut warnings = Vec::new();
    let source_size = std::fs::metadata(&source.canonical_location)
        .ok()
        .map(|metadata| metadata.len());
    let prefix_changed = cursor.byte_offset > 0
        && source_size.is_some_and(|size| size >= cursor.byte_offset)
        && cursor.native_cursor.as_deref().is_some_and(|stored| {
            source_prefix_digest(&source.canonical_location, cursor.byte_offset)
                .is_ok_and(|current| current != stored)
        });
    if cursor.byte_offset > 0
        && (source_size.is_some_and(|size| size < cursor.byte_offset) || prefix_changed)
    {
        warnings.push(format!(
            "{} was truncated or replaced; restarting import from byte zero",
            source.canonical_location.display()
        ));
        cursor = ImportCursor::default();
    }
    let batch = read_jsonl(
        &source.canonical_location,
        cursor.byte_offset,
        MAX_RECORDS_PER_BATCH,
    )
    .await?;
    let mut events_imported = 0_u64;
    let mut last_event_id = None;
    let mut fallback_session_id = source
        .canonical_location
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("codex-session")
        .to_owned();
    let mut fallback_project_root = None;
    if cursor.byte_offset > 0 {
        // The session header occurs only at the start of a rollout. Restore its
        // identity when resuming a later batch so one rollout stays one session.
        for header in read_jsonl(&source.canonical_location, 0, 1).await?.records {
            if header.value.get("type").and_then(Value::as_str) == Some("session_meta") {
                if let Some(id) = string_at(&header.value, &["payload", "id"]) {
                    fallback_session_id = id.to_owned();
                }
                fallback_project_root =
                    string_at(&header.value, &["payload", "cwd"]).map(PathBuf::from);
            }
        }
    }
    for record in batch.records {
        if let Some(event) = parse_event(
            &record.value,
            record.raw,
            record.end_offset,
            &fallback_session_id,
            fallback_project_root.as_deref(),
        )? {
            if let Some(metadata) = &event.session {
                fallback_session_id = metadata.native_session_id.clone();
                if metadata.project_root.is_some() {
                    fallback_project_root = metadata.project_root.clone();
                }
            }
            last_event_id = Some(event.event.id);
            sink.emit(event).await?;
            events_imported += 1;
        }
    }
    Ok(ImportOutcome {
        events_imported,
        cursor: ImportCursor {
            version: 1,
            byte_offset: batch.end_offset,
            native_cursor: source_prefix_digest(&source.canonical_location, batch.end_offset).ok(),
            source_size: std::fs::metadata(&source.canonical_location)
                .ok()
                .map(|metadata| metadata.len()),
            source_mtime_us: std::fs::metadata(&source.canonical_location)
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|duration| duration.as_micros().try_into().ok()),
            last_event_id,
        },
        parser_version: "codex-jsonl-v1".to_owned(),
        quarantined: batch.quarantined,
        warnings,
    })
}

fn parse_event(
    value: &Value,
    raw: Vec<u8>,
    end_offset: u64,
    fallback_native_session_id: &str,
    fallback_project_root: Option<&std::path::Path>,
) -> Result<Option<AdapterEvent>, AdapterError> {
    let timestamp = timestamp_us(string_at(value, &["timestamp"]));
    let record_type = string_at(value, &["type"]).unwrap_or_default();
    let payload = value.get("payload").unwrap_or(value);
    // Message/tool IDs are not session IDs. Only the rollout header owns `id`.
    let native_session_id = (record_type == "session_meta")
        .then(|| string_at(payload, &["id"]))
        .flatten()
        .or_else(|| string_at(payload, &["session_id"]))
        .or_else(|| string_at(value, &["session_id"]))
        .unwrap_or(fallback_native_session_id)
        .to_owned();
    let session_id = EntityId::from(deterministic_uuid(&format!("codex:{native_session_id}")));
    let session_meta = || AdapterSessionMetadata {
        native_session_id: native_session_id.clone(),
        project_root: string_at(payload, &["cwd"])
            .map(PathBuf::from)
            .or_else(|| fallback_project_root.map(PathBuf::from)),
        title_preview: None,
        agent_name: "codex".to_owned(),
        harness_name: Some("codex-cli".to_owned()),
        provider_name: string_at(payload, &["model_provider"]).map(str::to_owned),
        model_name: string_at(payload, &["model"]).map(str::to_owned),
        historical: true,
    };
    let source = || EventSource {
        kind: SourceKind::AgentLog,
        original_source_kind: None,
        adapter_id: Some("codex".to_owned()),
        source_event_id: format!("codex:{native_session_id}:{end_offset}"),
        raw_blob_id: None,
    };
    let mut event = if record_type == "session_meta" {
        EventEnvelope::new(source(), EventAction::SessionStart, timestamp, timestamp)
    } else if record_type == "event_msg" {
        let event_type = string_at(payload, &["type"]).unwrap_or_default();
        let action = match event_type {
            "user_message" => EventAction::Prompt,
            "agent_message" => EventAction::Response,
            _ => return Ok(None),
        };
        EventEnvelope::new(source(), action, timestamp, timestamp)
    } else if record_type == "response_item" {
        let item_type = string_at(payload, &["type"]).unwrap_or_default();
        let action = match item_type {
            "message" => match string_at(payload, &["role"]) {
                Some("user") => EventAction::Prompt,
                Some("assistant") => EventAction::Response,
                _ => return Ok(None),
            },
            "function_call" => {
                let name = string_at(payload, &["name"]).unwrap_or("tool");
                if name == "apply_patch" {
                    EventAction::FileWrite
                } else if matches!(name, "shell" | "exec_command" | "local_shell") {
                    EventAction::CommandExecute
                } else {
                    EventAction::ToolCall
                }
            }
            "function_call_output" | "custom_tool_call_output" => EventAction::ToolResult,
            "reasoning" => return Ok(None),
            _ => EventAction::ToolResult,
        };
        EventEnvelope::new(source(), action, timestamp, timestamp)
    } else {
        return Ok(None);
    };
    event.session_id = Some(session_id);
    event.actor.agent = Some("codex".to_owned());
    event.actor.model = string_at(payload, &["model"]).map(str::to_owned);
    event.evidence.attribution = agenttraceback_types::AttributionConfidence::Exact;
    if matches!(
        event.action,
        EventAction::CommandExecute | EventAction::CommandResult
    ) {
        event.target.kind = TargetKind::Command;
        event.target.display = string_at(payload, &["name"])
            .or_else(|| string_at(payload, &["command"]))
            .map(str::to_owned);
    } else if event.action == EventAction::Prompt || event.action == EventAction::Response {
        event.target.kind = TargetKind::Session;
    } else if matches!(
        event.action,
        EventAction::FileWrite | EventAction::FileCreate | EventAction::FileDelete
    ) {
        let arguments = string_at(payload, &["arguments"]).unwrap_or_default();
        let (action, path) = parse_apply_patch(arguments);
        event.action = action;
        event.target.kind = TargetKind::File;
        event.target.display = path.clone();
        event.target.normalized_path = path;
    }
    if let Some(text) = string_at(payload, &["message"])
        .map(str::to_owned)
        .or_else(|| string_at(payload, &["text"]).map(str::to_owned))
        .or_else(|| string_at(payload, &["arguments"]).map(str::to_owned))
        .or_else(|| string_at(payload, &["output"]).map(str::to_owned))
        .or_else(|| payload.get("content").and_then(text_content))
    {
        event.content.redacted_preview = Some(text);
    }
    event.result.status = if event.action == EventAction::CommandResult {
        ResultStatus::Success
    } else {
        ResultStatus::Unknown
    };
    Ok(Some(AdapterEvent {
        event,
        raw_payload: Some(raw),
        session: Some(session_meta()),
    }))
}

fn parse_apply_patch(arguments: &str) -> (EventAction, Option<String>) {
    let lines = arguments.lines().map(str::trim);
    for line in lines {
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            return (EventAction::FileWrite, Some(path.to_owned()));
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            return (EventAction::FileCreate, Some(path.to_owned()));
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            return (EventAction::FileDelete, Some(path.to_owned()));
        }
    }
    (EventAction::FileWrite, None)
}

fn capability(name: &str, availability: CapabilityAvailability) -> Capability {
    Capability {
        name: name.to_owned(),
        availability,
        detail: None,
    }
}
