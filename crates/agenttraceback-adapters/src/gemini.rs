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
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::common::{deterministic_uuid, string_at, text_content, timestamp_us};

const MAX_RECORDS: usize = 5_000;
const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;

/// Gemini CLI local chat adapter.
#[derive(Clone, Debug, Default)]
pub struct GeminiCliAdapter;

#[async_trait]
impl AgentAdapter for GeminiCliAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            id: "gemini-cli".to_owned(),
            display_name: "Gemini CLI".to_owned(),
            schema_version: 1,
            source_locations: vec![PathBuf::from("~/.gemini/tmp")],
            richer_capture_requires_change: false,
            documentation_url: Some("https://github.com/google-gemini/gemini-cli".to_owned()),
        }
    }

    async fn detect(
        &self,
        context: &DetectContext,
    ) -> Result<Vec<DetectedInstallation>, AdapterError> {
        let Some(home) = &context.home_dir else {
            return Ok(Vec::new());
        };
        let tmp_root = home.join(".gemini/tmp");
        let executable = context
            .path_entries
            .iter()
            .map(|entry| entry.join("gemini"))
            .find(|path| path.is_file());
        let gemini_root = home.join(".gemini");
        if !tmp_root.is_dir() && !gemini_root.is_dir() && executable.is_none() {
            return Ok(Vec::new());
        }
        let version = executable
            .and_then(|path| Command::new(path).arg("--version").output().ok())
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
        Ok(vec![DetectedInstallation {
            id: EntityId::from(deterministic_uuid("installation:gemini-cli")),
            adapter_id: "gemini-cli".to_owned(),
            display_name: "Gemini CLI".to_owned(),
            agent_version: version,
            source_roots: vec![tmp_root],
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
                capability("live_sessions", CapabilityAvailability::Degraded),
                capability("prompts", CapabilityAvailability::Available),
                capability("responses", CapabilityAvailability::Available),
                capability("tool_calls", CapabilityAvailability::Available),
                capability("commands", CapabilityAvailability::Degraded),
                capability("subagents", CapabilityAvailability::Unavailable),
                capability("tokens", CapabilityAvailability::Degraded),
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
                        .is_some_and(|extension| extension == "json")
                {
                    let path = entry.path().to_path_buf();
                    sources.push(ImportSource {
                        id: EntityId::from(deterministic_uuid(&format!(
                            "source:gemini:{}",
                            path.display()
                        ))),
                        adapter_id: "gemini-cli".to_owned(),
                        stable_identity: path.to_string_lossy().into_owned(),
                        canonical_location: path,
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
    let mut cursor = cursor.unwrap_or_default();
    let metadata = std::fs::metadata(&source.canonical_location)?;
    let modified_at_us = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| duration.as_micros().try_into().ok());
    if cursor.source_size == Some(metadata.len()) && cursor.source_mtime_us == modified_at_us {
        return Ok(ImportOutcome {
            cursor,
            parser_version: "gemini-json-v1".to_owned(),
            ..ImportOutcome::default()
        });
    }
    if cursor.byte_offset > metadata.len() {
        cursor.byte_offset = 0;
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err(AdapterError::UnsupportedSource(format!(
            "{} exceeds the {} byte Gemini source limit",
            source.canonical_location.display(),
            MAX_SOURCE_BYTES
        )));
    }
    let bytes = std::fs::read(&source.canonical_location)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        AdapterError::Parse(format!("{}: {error}", source.canonical_location.display()))
    })?;
    let native_session_id = string_at(&value, &["sessionId"])
        .or_else(|| string_at(&value, &["session_id"]))
        .map(str::to_owned)
        .or_else(|| {
            source
                .canonical_location
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "gemini-session".to_owned());
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .cloned()
        .unwrap_or_default();
    let mut warnings = Vec::new();
    let mut start_index = cursor
        .native_cursor
        .as_deref()
        .and_then(|value| value.strip_prefix("index:"))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if start_index > messages.len() {
        warnings.push(format!(
            "{} was truncated or replaced; restarting Gemini import",
            source.canonical_location.display()
        ));
        start_index = 0;
    }
    let metadata_agent = AdapterSessionMetadata {
        native_session_id: native_session_id.clone(),
        project_root: string_at(&value, &["cwd"])
            .or_else(|| string_at(&value, &["projectRoot"]))
            .map(PathBuf::from),
        title_preview: string_at(&value, &["title"]).map(str::to_owned),
        agent_name: "gemini-cli".to_owned(),
        harness_name: Some("gemini-cli".to_owned()),
        provider_name: Some("google".to_owned()),
        model_name: string_at(&value, &["model"]).map(str::to_owned),
        historical: true,
    };
    let mut events_imported = 0_u64;
    let mut last_event_id = None;
    let end_index = messages.len().min(start_index.saturating_add(MAX_RECORDS));
    for (index, message) in messages[start_index..end_index].iter().enumerate() {
        let index = start_index + index;
        let record_id = format!("message:{index}");
        let role = string_at(message, &["role"])
            .or_else(|| string_at(message, &["type"]))
            .unwrap_or("assistant");
        let timestamp = timestamp_us(
            string_at(message, &["timestamp"]).or_else(|| string_at(message, &["time"])),
        );
        let content = message.get("content").unwrap_or(message);
        let action = match role {
            "user" => EventAction::Prompt,
            "assistant" | "model" => EventAction::Response,
            _ => continue,
        };
        let mut event = make_event(
            &record_id,
            &native_session_id,
            timestamp,
            action,
            text_content(content),
        );
        event.event.result.status = ResultStatus::Unknown;
        last_event_id = Some(event.event.id);
        sink.emit(AdapterEvent {
            event: event.event,
            raw_payload: Some(message.to_string().into_bytes()),
            session: Some(metadata_agent.clone()),
        })
        .await?;
        events_imported += 1;
        if let Some(function_calls) = message
            .get("functionCalls")
            .or_else(|| message.get("function_calls"))
            .and_then(Value::as_array)
        {
            for (call_index, call) in function_calls.iter().enumerate() {
                let name = string_at(call, &["name"]).unwrap_or("tool");
                let mut tool_event = make_event(
                    &format!("message:{index}:tool:{call_index}"),
                    &native_session_id,
                    timestamp,
                    if name == "run_shell_command" {
                        EventAction::CommandExecute
                    } else {
                        EventAction::ToolCall
                    },
                    call.get("args").map(Value::to_string),
                );
                tool_event.event.target.kind =
                    if tool_event.event.action == EventAction::CommandExecute {
                        TargetKind::Command
                    } else {
                        TargetKind::Unknown
                    };
                tool_event.event.target.display = Some(name.to_owned());
                last_event_id = Some(tool_event.event.id);
                sink.emit(AdapterEvent {
                    event: tool_event.event,
                    raw_payload: Some(call.to_string().into_bytes()),
                    session: Some(metadata_agent.clone()),
                })
                .await?;
                events_imported += 1;
            }
        }
    }
    Ok(ImportOutcome {
        events_imported,
        cursor: ImportCursor {
            version: 1,
            byte_offset: metadata.len(),
            native_cursor: Some(format!("index:{end_index}")),
            source_size: Some(metadata.len()),
            source_mtime_us: modified_at_us,
            last_event_id,
        },
        parser_version: "gemini-json-v1".to_owned(),
        quarantined: 0,
        warnings,
    })
}

fn make_event(
    id_suffix: &str,
    native_session_id: &str,
    timestamp_us: i64,
    action: EventAction,
    preview: Option<String>,
) -> AdapterEvent {
    let mut event = EventEnvelope::new(
        EventSource {
            kind: SourceKind::AgentLog,
            original_source_kind: None,
            adapter_id: Some("gemini-cli".to_owned()),
            source_event_id: format!("gemini:{native_session_id}:{id_suffix}"),
            raw_blob_id: None,
        },
        action,
        timestamp_us,
        timestamp_us,
    );
    event.session_id = Some(EntityId::from(deterministic_uuid(&format!(
        "gemini:{native_session_id}"
    ))));
    event.actor.agent = Some("gemini-cli".to_owned());
    event.target.kind = TargetKind::Session;
    event.content.redacted_preview = preview;
    event.evidence.attribution = AttributionConfidence::Exact;
    AdapterEvent {
        event,
        raw_payload: None,
        session: None,
    }
}

fn capability(name: &str, availability: CapabilityAvailability) -> Capability {
    Capability {
        name: name.to_owned(),
        availability,
        detail: None,
    }
}
