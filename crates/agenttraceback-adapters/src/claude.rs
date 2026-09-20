use std::{path::PathBuf, process::Command, time::Duration};

use agenttraceback_adapter_sdk::{
    AdapterDescriptor, AdapterError, AdapterEvent, AdapterSessionMetadata, AgentAdapter,
    Capability, CapabilityAvailability, CapabilitySet, DetectContext, DetectedInstallation,
    EventSink, HookPlan, HookReceipt, ImportCursor, ImportOutcome, ImportSource,
};
use agenttraceback_types::{
    AttributionConfidence, EntityId, EventAction, EventEnvelope, EventSource, ResultStatus,
    SourceKind, TargetKind,
};
use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::common::{
    deterministic_uuid, read_jsonl, source_prefix_digest, string_at, text_content, timestamp_us,
};

const MAX_RECORDS_PER_BATCH: usize = 1_000;

/// Claude Code adapter.
#[derive(Clone, Debug, Default)]
pub struct ClaudeCodeAdapter;

#[async_trait]
impl AgentAdapter for ClaudeCodeAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            id: "claude-code".to_owned(),
            display_name: "Claude Code".to_owned(),
            schema_version: 1,
            source_locations: vec![PathBuf::from("~/.claude/projects")],
            richer_capture_requires_change: true,
            documentation_url: Some("https://docs.anthropic.com/en/docs/claude-code".to_owned()),
        }
    }

    async fn detect(
        &self,
        context: &DetectContext,
    ) -> Result<Vec<DetectedInstallation>, AdapterError> {
        let Some(home) = &context.home_dir else {
            return Ok(Vec::new());
        };
        let source_root = home.join(".claude/projects");
        let executable = context
            .path_entries
            .iter()
            .map(|entry| entry.join("claude"))
            .find(|path| path.is_file());
        if !source_root.is_dir() && executable.is_none() {
            return Ok(Vec::new());
        }
        let version = executable
            .and_then(|path| Command::new(path).arg("--version").output().ok())
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
        Ok(vec![DetectedInstallation {
            id: EntityId::from(deterministic_uuid("installation:claude-code")),
            adapter_id: "claude-code".to_owned(),
            display_name: "Claude Code".to_owned(),
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
                capability("subagents", CapabilityAvailability::Degraded),
                capability("tokens", CapabilityAvailability::Available),
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
                            "source:claude:{}",
                            canonical.display()
                        ))),
                        adapter_id: "claude-code".to_owned(),
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

    async fn plan_hook_install(
        &self,
        installation: &DetectedInstallation,
    ) -> Result<Option<HookPlan>, AdapterError> {
        let settings_path = claude_settings_path(installation).ok_or_else(|| {
            AdapterError::UnsupportedSource("Claude settings path is unavailable".to_owned())
        })?;
        let before = read_optional(&settings_path)?;
        let merged = merge_agenttraceback_hooks(&before);
        let preview = serde_json::to_string_pretty(&merged)
            .map_err(|error| AdapterError::Parse(error.to_string()))?;
        let before_hash = blake3::hash(&before).to_hex().to_string();
        let plan_digest = blake3::hash(
            format!("{}\n{}\n{}", settings_path.display(), before_hash, preview).as_bytes(),
        )
        .to_hex()
        .to_string();
        Ok(Some(HookPlan {
            adapter_id: "claude-code".to_owned(),
            config_path: settings_path,
            before_hash,
            plan_digest,
            preview,
        }))
    }

    async fn install_hook(&self, approved_plan: HookPlan) -> Result<HookReceipt, AdapterError> {
        if approved_plan.adapter_id != "claude-code" {
            return Err(AdapterError::HookConflict(
                "hook plan belongs to a different adapter".to_owned(),
            ));
        }
        let before = read_optional(&approved_plan.config_path)?;
        if blake3::hash(&before).to_hex().to_string() != approved_plan.before_hash {
            return Err(AdapterError::HookConflict(
                "Claude settings changed after the plan was created".to_owned(),
            ));
        }
        let merged = merge_agenttraceback_hooks(&before);
        let backup_path = approved_plan
            .config_path
            .with_extension(format!("json.agenttraceback-backup-{}", current_time_us()));
        if approved_plan.config_path.exists() {
            std::fs::copy(&approved_plan.config_path, &backup_path)?;
        } else {
            std::fs::write(&backup_path, b"{}")?;
        }
        atomic_write_json(&approved_plan.config_path, &merged)?;
        Ok(HookReceipt {
            adapter_id: approved_plan.adapter_id,
            config_path: approved_plan.config_path,
            backup_path,
            plan_digest: approved_plan.plan_digest,
        })
    }

    async fn uninstall_hook(&self, receipt: HookReceipt) -> Result<(), AdapterError> {
        if receipt.adapter_id != "claude-code" {
            return Err(AdapterError::HookConflict(
                "hook receipt belongs to a different adapter".to_owned(),
            ));
        }
        let current = read_optional(&receipt.config_path)?;
        let mut value: Value =
            serde_json::from_slice(&current).unwrap_or_else(|_| serde_json::json!({}));
        remove_agenttraceback_hooks(&mut value);
        atomic_write_json(&receipt.config_path, &value)
    }
}

fn claude_settings_path(installation: &DetectedInstallation) -> Option<PathBuf> {
    installation
        .source_roots
        .first()
        .and_then(|projects| projects.parent())
        .map(|claude_root| claude_root.join("settings.json"))
}

fn read_optional(path: &std::path::Path) -> Result<Vec<u8>, AdapterError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(b"{}".to_vec()),
        Err(error) => Err(error.into()),
    }
}

fn merge_agenttraceback_hooks(before: &[u8]) -> Value {
    let mut root: Value = serde_json::from_slice(before).unwrap_or_else(|_| serde_json::json!({}));
    if !root.is_object() {
        root = serde_json::json!({});
    }
    let Some(root_object) = root.as_object_mut() else {
        return root;
    };
    let hooks = root_object
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        *hooks = serde_json::json!({});
    }
    let Some(hooks_object) = hooks.as_object_mut() else {
        return root;
    };
    for (event_name, command, stable_id) in [
        (
            "SessionStart",
            "agenttraceback hook claude-code session-start",
            "agenttraceback:claude-code:session-start",
        ),
        (
            "Stop",
            "agenttraceback hook claude-code stop",
            "agenttraceback:claude-code:stop",
        ),
    ] {
        let entries = hooks_object
            .entry(event_name)
            .or_insert_with(|| serde_json::json!([]));
        if !entries.is_array() {
            *entries = serde_json::json!([]);
        }
        let Some(entries) = entries.as_array_mut() else {
            continue;
        };
        let already_present = entries.iter().any(|entry| {
            string_at(entry, &["_agenttracebackId"]) == Some(stable_id)
                || entry
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|hooks| {
                        hooks
                            .iter()
                            .any(|hook| string_at(hook, &["command"]) == Some(command))
                    })
        });
        if !already_present {
            entries.push(serde_json::json!({
                "_agenttracebackId": stable_id,
                "matcher": "",
                "hooks": [{"type": "command", "command": command}]
            }));
        }
    }
    root
}

fn remove_agenttraceback_hooks(root: &mut Value) {
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for entries in hooks.values_mut() {
        let Some(entries) = entries.as_array_mut() else {
            continue;
        };
        for entry in entries.iter_mut() {
            let managed = string_at(entry, &["_agenttracebackId"])
                .is_some_and(|id| id.starts_with("agenttraceback:claude-code:"));
            if let Some(commands) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
                commands.retain(|hook| {
                    !string_at(hook, &["command"]).is_some_and(|command| {
                        command.starts_with("agenttraceback hook claude-code ")
                    })
                });
                if managed {
                    if commands.is_empty() {
                        entry.as_object_mut().map(|object| {
                            object.insert("_agenttracebackRemove".to_owned(), Value::Bool(true))
                        });
                    } else if let Some(object) = entry.as_object_mut() {
                        object.remove("_agenttracebackId");
                    }
                }
            }
        }
        entries.retain(|entry| {
            if entry
                .get("_agenttracebackRemove")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return false;
            }
            entry
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|commands| !commands.is_empty())
        });
    }
    hooks.retain(|_, entries| entries.as_array().is_none_or(|entries| !entries.is_empty()));
}

fn atomic_write_json(path: &std::path::Path, value: &Value) -> Result<(), AdapterError> {
    let parent = path
        .parent()
        .ok_or_else(|| AdapterError::UnsupportedSource("settings path has no parent".to_owned()))?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)
        .map_err(|error| AdapterError::Parse(error.to_string()))?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(AdapterError::Io)?;
    temporary
        .persist(path)
        .map_err(|error| AdapterError::Io(error.error))?;
    Ok(())
}

fn current_time_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
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
    for record in batch.records {
        if let Some(event) = parse_event(&record.value, record.raw, record.end_offset)? {
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
        parser_version: "claude-jsonl-v1".to_owned(),
        quarantined: batch.quarantined,
        warnings,
    })
}

fn parse_event(
    value: &Value,
    raw: Vec<u8>,
    end_offset: u64,
) -> Result<Option<AdapterEvent>, AdapterError> {
    let record_type = string_at(value, &["type"]).unwrap_or_default();
    if !matches!(record_type, "user" | "assistant") {
        return Ok(None);
    }
    let timestamp = timestamp_us(string_at(value, &["timestamp"]));
    let native_session_id = string_at(value, &["sessionId"])
        .or_else(|| string_at(value, &["session_id"]))
        .unwrap_or("claude-session")
        .to_owned();
    let session_id = EntityId::from(deterministic_uuid(&format!("claude:{native_session_id}")));
    let message = value.get("message").unwrap_or(value);
    let content = message.get("content").unwrap_or(message);
    let (action, target_kind, target_display, preview, result_status) = if record_type == "user" {
        let tool_result = content.as_array().and_then(|items| {
            items
                .iter()
                .find(|item| string_at(item, &["type"]) == Some("tool_result"))
        });
        if let Some(tool_result) = tool_result {
            (
                EventAction::ToolResult,
                TargetKind::Command,
                string_at(tool_result, &["tool_use_id"]).map(str::to_owned),
                text_content(tool_result.get("content").unwrap_or(tool_result)),
                if tool_result
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    ResultStatus::Failed
                } else {
                    ResultStatus::Success
                },
            )
        } else {
            (
                EventAction::Prompt,
                TargetKind::Session,
                None,
                text_content(content),
                ResultStatus::Unknown,
            )
        }
    } else {
        let tool_use = content.as_array().and_then(|items| {
            items
                .iter()
                .find(|item| string_at(item, &["type"]) == Some("tool_use"))
        });
        if let Some(tool_use) = tool_use {
            let name = string_at(tool_use, &["name"]).unwrap_or("tool");
            let action = if name == "Bash" {
                EventAction::CommandExecute
            } else {
                EventAction::ToolCall
            };
            (
                action,
                if action == EventAction::CommandExecute {
                    TargetKind::Command
                } else {
                    TargetKind::Unknown
                },
                Some(name.to_owned()),
                tool_use.get("input").and_then(|input| {
                    string_at(input, &["command"])
                        .map(str::to_owned)
                        .or_else(|| Some(input.to_string()))
                }),
                ResultStatus::Unknown,
            )
        } else {
            (
                EventAction::Response,
                TargetKind::Session,
                None,
                text_content(content),
                ResultStatus::Success,
            )
        }
    };
    let mut event = EventEnvelope::new(
        EventSource {
            kind: SourceKind::AgentLog,
            original_source_kind: None,
            adapter_id: Some("claude-code".to_owned()),
            source_event_id: string_at(value, &["uuid"])
                .map(str::to_owned)
                .unwrap_or_else(|| format!("claude:{native_session_id}:{end_offset}")),
            raw_blob_id: None,
        },
        action,
        timestamp,
        timestamp,
    );
    event.session_id = Some(session_id);
    event.actor.agent = Some("claude-code".to_owned());
    event.actor.model = string_at(message, &["model"]).map(str::to_owned);
    event.actor.subagent_id = value
        .get("isSidechain")
        .and_then(Value::as_bool)
        .filter(|sidechain| *sidechain)
        .map(|_| {
            string_at(value, &["parentUuid"])
                .unwrap_or("claude-subagent")
                .to_owned()
        });
    event.target.kind = target_kind;
    event.target.display = target_display;
    if let Some(tool_name) = event.target.display.clone() {
        let input = content
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| string_at(item, &["name"]) == Some(tool_name.as_str()))
            })
            .and_then(|item| item.get("input"));
        if let Some(input) = input {
            let path = string_at(input, &["file_path"])
                .or_else(|| string_at(input, &["path"]))
                .map(str::to_owned);
            match tool_name.as_str() {
                "Write" | "Edit" | "MultiEdit" => {
                    event.action = EventAction::FileWrite;
                    event.target.kind = TargetKind::File;
                    event.target.display = path.clone();
                    event.target.normalized_path = path;
                }
                "Read" => {
                    event.action = EventAction::FileRead;
                    event.target.kind = TargetKind::File;
                    event.target.display = path.clone();
                    event.target.normalized_path = path;
                }
                _ => {}
            }
        }
    }
    event.content.redacted_preview = preview;
    event.result.status = result_status;
    event.evidence.attribution = AttributionConfidence::Exact;
    Ok(Some(AdapterEvent {
        event,
        raw_payload: Some(raw),
        session: Some(AdapterSessionMetadata {
            native_session_id,
            project_root: string_at(value, &["cwd"]).map(PathBuf::from),
            title_preview: None,
            agent_name: "claude-code".to_owned(),
            harness_name: Some("claude-code".to_owned()),
            provider_name: Some("anthropic".to_owned()),
            model_name: string_at(message, &["model"]).map(str::to_owned),
            historical: true,
        }),
    }))
}

fn capability(name: &str, availability: CapabilityAvailability) -> Capability {
    Capability {
        name: name.to_owned(),
        availability,
        detail: None,
    }
}
