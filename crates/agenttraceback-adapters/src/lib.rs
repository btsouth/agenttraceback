//! Built-in semantic adapters for supported coding agents.

mod claude;
mod codex;
mod command_code;
mod common;
mod gemini;
mod hermes;
mod opencode;

use std::sync::Arc;

use agenttraceback_adapter_sdk::AgentAdapter;

pub use claude::ClaudeCodeAdapter;
pub use codex::CodexCliAdapter;
pub use command_code::CommandCodeAdapter;
pub use gemini::GeminiCliAdapter;
pub use hermes::HermesAdapter;
pub use opencode::OpenCodeAdapter;

/// Registry of built-in adapters.
#[derive(Clone, Default)]
pub struct AdapterRegistry {
    adapters: Vec<Arc<dyn AgentAdapter>>,
}

impl AdapterRegistry {
    /// Creates the first-party adapter registry.
    #[must_use]
    pub fn built_in() -> Self {
        Self {
            adapters: vec![
                Arc::new(ClaudeCodeAdapter),
                Arc::new(CodexCliAdapter),
                Arc::new(CommandCodeAdapter),
                Arc::new(HermesAdapter),
                Arc::new(OpenCodeAdapter),
                Arc::new(GeminiCliAdapter),
            ],
        }
    }

    /// Returns adapters in deterministic ID order.
    #[must_use]
    pub fn adapters(&self) -> &[Arc<dyn AgentAdapter>] {
        &self.adapters
    }

    /// Looks up an adapter by ID.
    #[must_use]
    pub fn get(&self, adapter_id: &str) -> Option<Arc<dyn AgentAdapter>> {
        self.adapters
            .iter()
            .find(|adapter| adapter.descriptor().id == adapter_id)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, sync::Arc};

    use agenttraceback_adapter_sdk::{
        AdapterEvent, AgentAdapter, DetectContext, DetectedInstallation, EventSink, ImportCursor,
        ImportSource,
    };
    use agenttraceback_types::{EntityId, EventAction, ResultStatus};
    use async_trait::async_trait;
    use rusqlite::Connection;
    use tokio::sync::Mutex;

    use super::{
        ClaudeCodeAdapter, CodexCliAdapter, CommandCodeAdapter, GeminiCliAdapter, HermesAdapter,
        OpenCodeAdapter,
    };

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<AdapterEvent>>,
    }

    #[async_trait]
    impl EventSink for RecordingSink {
        async fn emit(
            &self,
            event: AdapterEvent,
        ) -> Result<(), agenttraceback_adapter_sdk::AdapterError> {
            self.events.lock().await.push(event);
            Ok(())
        }
    }

    fn source(path: &std::path::Path, id: &str) -> ImportSource {
        ImportSource {
            id: EntityId::new(),
            adapter_id: id.to_owned(),
            canonical_location: path.to_path_buf(),
            stable_identity: path.to_string_lossy().into_owned(),
            source_kind: "agent_log".to_owned(),
        }
    }

    #[tokio::test]
    async fn codex_import_is_incremental_and_handles_partial_lines() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("rollout.jsonl");
        let first_lines = [
            r#"{"timestamp":"2026-09-20T10:00:00Z","type":"session_meta","payload":{"id":"session-1","cwd":"/tmp/project","model":"gpt-5","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-09-20T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fix the bug"}]}}"#,
            r#"{"timestamp":"2026-09-20T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":\"cargo test\"}"}}"#,
        ];
        fs::write(&path, format!("{}\n{}\n", first_lines[0], first_lines[1])).expect("fixture");
        let sink = RecordingSink::default();
        let adapter = CodexCliAdapter;
        let outcome = adapter
            .import(&source(&path, "codex"), None, &sink)
            .await
            .expect("first import");
        assert_eq!(outcome.events_imported, 2);
        assert_eq!(
            sink.events.lock().await[1].event.action,
            EventAction::Prompt
        );

        fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n{}",
                first_lines[0],
                first_lines[1],
                first_lines[2],
                r#"{"timestamp":"2026-09-20T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","output":"ok"}}"#
            ),
        )
        .expect("append");
        let outcome = adapter
            .import(
                &source(&path, "codex"),
                Some(ImportCursor {
                    version: 1,
                    byte_offset: outcome.cursor.byte_offset,
                    ..ImportCursor::default()
                }),
                &sink,
            )
            .await
            .expect("incremental import");
        assert_eq!(outcome.events_imported, 2);
        assert_eq!(
            sink.events.lock().await.last().expect("event").event.action,
            EventAction::ToolResult
        );
    }

    #[tokio::test]
    async fn claude_import_normalizes_messages_tools_and_results() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("session.jsonl");
        let lines = [
            r#"{"type":"user","sessionId":"claude-1","cwd":"/tmp/project","timestamp":"2026-09-20T10:00:00Z","uuid":"u1","message":{"role":"user","content":"implement auth"}}"#,
            r#"{"type":"assistant","sessionId":"claude-1","timestamp":"2026-09-20T10:00:01Z","uuid":"a1","message":{"role":"assistant","model":"claude-sonnet","content":[{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            r#"{"type":"user","sessionId":"claude-1","timestamp":"2026-09-20T10:00:02Z","uuid":"r1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"tests passed"}]}}"#,
        ];
        fs::write(&path, format!("{}\n", lines.join("\n"))).expect("fixture");
        let sink = RecordingSink::default();
        let outcome = ClaudeCodeAdapter
            .import(&source(&path, "claude-code"), None, &sink)
            .await
            .expect("import");
        assert_eq!(outcome.events_imported, 3);
        let events = sink.events.lock().await;
        assert_eq!(events[0].event.action, EventAction::Prompt);
        assert_eq!(events[1].event.action, EventAction::CommandExecute);
        assert_eq!(events[2].event.action, EventAction::ToolResult);
        assert_eq!(events[2].event.result.status, ResultStatus::Success);
    }

    #[test]
    fn registry_contains_required_adapters() {
        let registry = super::AdapterRegistry::built_in();
        assert!(registry.get("claude-code").is_some());
        assert!(registry.get("codex").is_some());
        assert!(Arc::strong_count(&registry.adapters()[0]) >= 1);
        let _: PathBuf = PathBuf::from(".");
    }

    #[tokio::test]
    async fn claude_hook_install_is_reversible_and_preserves_settings() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let claude_root = directory.path().join(".claude");
        let projects = claude_root.join("projects");
        fs::create_dir_all(&projects).expect("projects");
        fs::write(
            claude_root.join("settings.json"),
            r#"{"theme":"dark","hooks":{"Other":[{"matcher":"x","hooks":[{"type":"command","command":"keep"}]}]}}"#,
        )
        .expect("settings");
        let installation = DetectedInstallation {
            id: EntityId::new(),
            adapter_id: "claude-code".to_owned(),
            display_name: "Claude Code".to_owned(),
            agent_version: None,
            source_roots: vec![projects],
            diagnostic_code: None,
        };
        let adapter = ClaudeCodeAdapter;
        let plan = adapter
            .plan_hook_install(&installation)
            .await
            .expect("plan")
            .expect("hook plan");
        let receipt = adapter.install_hook(plan).await.expect("install");
        let installed: serde_json::Value = serde_json::from_slice(
            &fs::read(claude_root.join("settings.json")).expect("installed settings"),
        )
        .expect("json");
        assert_eq!(installed["theme"], "dark");
        assert!(installed["hooks"]["Other"].is_array());
        assert!(installed["hooks"]["SessionStart"].is_array());
        adapter.uninstall_hook(receipt).await.expect("uninstall");
        let restored: serde_json::Value = serde_json::from_slice(
            &fs::read(claude_root.join("settings.json")).expect("restored settings"),
        )
        .expect("json");
        assert_eq!(restored["theme"], "dark");
        assert!(restored["hooks"]["Other"].is_array());
        assert!(restored["hooks"].get("SessionStart").is_none());
    }

    #[tokio::test]
    async fn codex_handles_partial_records_and_truncation_without_reemission() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("rollout.jsonl");
        let first = r#"{"timestamp":"2026-09-20T10:00:00Z","type":"session_meta","payload":{"id":"session-1","cwd":"/tmp/project"}}"#;
        let second = r#"{"timestamp":"2026-09-20T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"one"}]}}"#;
        fs::write(&path, format!("{first}\n{second}\n{{\"timestamp\":")).expect("fixture");
        let sink = RecordingSink::default();
        let adapter = CodexCliAdapter;
        let outcome = adapter
            .import(&source(&path, "codex"), None, &sink)
            .await
            .expect("first import");
        assert_eq!(outcome.events_imported, 2);
        assert_eq!(sink.events.lock().await.len(), 2);

        fs::write(&path, format!("{first}\n")).expect("truncate");
        let outcome = adapter
            .import(&source(&path, "codex"), Some(outcome.cursor), &sink)
            .await
            .expect("restart after truncation");
        assert_eq!(outcome.events_imported, 1);
        assert!(!outcome.warnings.is_empty());
    }

    #[tokio::test]
    async fn hermes_import_is_incremental_and_quarantines_bad_tool_calls() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("state.db");
        let connection = Connection::open(&path).expect("database");
        connection
            .execute_batch(
                "CREATE TABLE sessions (id TEXT PRIMARY KEY, cwd TEXT, title TEXT, model TEXT, parent_session_id TEXT, started_at REAL);
                 CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, tool_name TEXT, tool_calls TEXT, finish_reason TEXT);
                 INSERT INTO sessions VALUES ('hermes-1', '/tmp/project', 'Fix auth', 'deepseek', NULL, 1789900800.0);
                 INSERT INTO messages VALUES (1, 'hermes-1', 'user', 'fix the bug', NULL, NULL, NULL);
                 INSERT INTO messages VALUES (2, 'hermes-1', 'assistant', 'running tests', 'bash', 'not-json', 'stop');",
            )
            .expect("fixture rows");
        let adapter = HermesAdapter;
        let sink = RecordingSink::default();
        let outcome = adapter
            .import(&source(&path, "hermes"), None, &sink)
            .await
            .expect("first import");
        assert_eq!(outcome.events_imported, 2);
        assert_eq!(outcome.quarantined, 1);
        let cursor = outcome.cursor;

        let repeated = adapter
            .import(&source(&path, "hermes"), Some(cursor.clone()), &sink)
            .await
            .expect("repeat import");
        assert_eq!(repeated.events_imported, 0);

        connection
            .execute(
                "INSERT INTO messages VALUES (3, 'hermes-1', 'tool', 'tests passed', NULL, NULL, 'stop')",
                [],
            )
            .expect("append");
        let appended = adapter
            .import(&source(&path, "hermes"), Some(cursor), &sink)
            .await
            .expect("append import");
        assert_eq!(appended.events_imported, 1);
    }

    #[tokio::test]
    async fn opencode_import_handles_unknown_fields_and_malformed_parts() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("opencode.db");
        let connection = Connection::open(&path).expect("database");
        connection
            .execute_batch(
                "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL, title TEXT NOT NULL, model TEXT, project_id TEXT NOT NULL);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);
                 CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, data TEXT NOT NULL, time_created INTEGER NOT NULL);
                 INSERT INTO session VALUES ('opencode-1', '/tmp/project', 'Add tests', 'anthropic/claude', 'project-1');
                 INSERT INTO message VALUES ('m1', 'opencode-1', 1789900800000, '{\"role\":\"user\",\"futureField\":42}');
                 INSERT INTO part VALUES ('p1', 'm1', '{\"type\":\"text\",\"text\":\"add tests\",\"future\":true}', 1789900800001);",
            )
            .expect("fixture rows");
        let adapter = OpenCodeAdapter;
        let sink = RecordingSink::default();
        let outcome = adapter
            .import(&source(&path, "opencode"), None, &sink)
            .await
            .expect("first import");
        assert_eq!(outcome.events_imported, 2);
        let cursor = outcome.cursor;
        assert_eq!(
            adapter
                .import(&source(&path, "opencode"), Some(cursor.clone()), &sink)
                .await
                .expect("repeat import")
                .events_imported,
            0
        );

        connection
            .execute_batch(
                "INSERT INTO message VALUES ('m2', 'opencode-1', 1789900801000, '{\"role\":\"assistant\"}');
                 INSERT INTO part VALUES ('p2', 'm2', '{malformed', 1789900801001);",
            )
            .expect("append malformed");
        let appended = adapter
            .import(&source(&path, "opencode"), Some(cursor), &sink)
            .await
            .expect("append import");
        assert_eq!(appended.events_imported, 1);
        assert_eq!(appended.quarantined, 1);
    }

    #[tokio::test]
    async fn gemini_import_resumes_by_message_index() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("session.json");
        let first = serde_json::json!({
            "sessionId": "gemini-1",
            "cwd": "/tmp/project",
            "futureTopLevel": {"kept": true},
            "messages": [{
                "role": "user",
                "timestamp": "2026-09-20T10:00:00Z",
                "content": [{"text": "fix it"}],
                "futureMessageField": 1
            }]
        });
        fs::write(&path, serde_json::to_vec(&first).expect("json")).expect("fixture");
        let adapter = GeminiCliAdapter;
        let sink = RecordingSink::default();
        let outcome = adapter
            .import(&source(&path, "gemini-cli"), None, &sink)
            .await
            .expect("first import");
        assert_eq!(outcome.events_imported, 1);

        let repeated = adapter
            .import(
                &source(&path, "gemini-cli"),
                Some(outcome.cursor.clone()),
                &sink,
            )
            .await
            .expect("repeat import");
        assert_eq!(repeated.events_imported, 0);

        let mut second = first.clone();
        second["messages"]
            .as_array_mut()
            .expect("messages")
            .push(serde_json::json!({
                "role": "assistant",
                "timestamp": "2026-09-20T10:00:01Z",
                "content": "done"
            }));
        fs::write(&path, serde_json::to_vec(&second).expect("json")).expect("append");
        let appended = adapter
            .import(&source(&path, "gemini-cli"), Some(outcome.cursor), &sink)
            .await
            .expect("append import");
        assert_eq!(appended.events_imported, 1);
    }

    #[tokio::test]
    async fn command_code_detection_is_truthful_and_wrapper_compatible() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join(if cfg!(windows) {
            "commandcode.exe"
        } else {
            "commandcode"
        });
        fs::write(&executable, b"fixture").expect("executable fixture");
        let context = DetectContext {
            home_dir: None,
            path_entries: vec![directory.path().to_path_buf()],
        };
        let adapter = CommandCodeAdapter;
        let detected = adapter.detect(&context).await.expect("detect");
        assert_eq!(detected.len(), 1);
        assert_eq!(
            detected[0].diagnostic_code.as_deref(),
            Some("semantic_source_unavailable")
        );
        let capabilities = adapter
            .capabilities(&detected[0])
            .await
            .expect("capabilities");
        assert_eq!(
            capabilities
                .get("generic_wrapper")
                .expect("wrapper capability")
                .availability,
            agenttraceback_adapter_sdk::CapabilityAvailability::Available
        );
        assert!(
            adapter
                .discover_sources(&detected[0])
                .await
                .expect("sources")
                .is_empty()
        );
    }
}
