use std::path::{Path, PathBuf};

use agenttraceback_adapter_sdk::{
    AdapterDescriptor, AdapterError, AgentAdapter, Capability, CapabilityAvailability,
    CapabilitySet, DetectContext, DetectedInstallation, EventSink, ImportCursor, ImportOutcome,
    ImportSource,
};
use agenttraceback_types::EntityId;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::common::deterministic_uuid;

const EXECUTABLE_NAMES: &[&str] = &["command-code", "commandcode", "cmdcode"];
const KNOWN_ROOTS: &[&str] = &[".commandcode", ".command-code", ".config/command-code"];

/// Command Code detection and generic-wrapper profile.
#[derive(Clone, Debug, Default)]
pub struct CommandCodeAdapter;

#[async_trait]
impl AgentAdapter for CommandCodeAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            id: "command-code".to_owned(),
            display_name: "Command Code".to_owned(),
            schema_version: 1,
            source_locations: KNOWN_ROOTS
                .iter()
                .map(|path| PathBuf::from(format!("~/{path}")))
                .collect(),
            richer_capture_requires_change: false,
            documentation_url: Some("https://commandcode.ai/".to_owned()),
        }
    }

    async fn detect(
        &self,
        context: &DetectContext,
    ) -> Result<Vec<DetectedInstallation>, AdapterError> {
        let executable = context.path_entries.iter().find_map(|entry| {
            EXECUTABLE_NAMES
                .iter()
                .map(|name| executable_path(entry, name))
                .find(|path| path.is_file())
        });
        let roots = context.home_dir.as_ref().map_or_else(Vec::new, |home| {
            KNOWN_ROOTS
                .iter()
                .map(|root| home.join(root))
                .filter(|path| path.exists())
                .collect()
        });
        if executable.is_none() && roots.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![DetectedInstallation {
            id: EntityId::from(deterministic_uuid("installation:command-code")),
            adapter_id: "command-code".to_owned(),
            display_name: "Command Code".to_owned(),
            agent_version: None,
            source_roots: roots,
            diagnostic_code: Some("semantic_source_unavailable".to_owned()),
        }])
    }

    async fn capabilities(
        &self,
        _installation: &DetectedInstallation,
    ) -> Result<CapabilitySet, AdapterError> {
        Ok(CapabilitySet {
            capabilities: vec![
                capability("generic_wrapper", CapabilityAvailability::Available),
                capability("historical_sessions", CapabilityAvailability::Unavailable),
                capability("live_sessions", CapabilityAvailability::Unavailable),
                capability("prompts", CapabilityAvailability::Unavailable),
                capability("responses", CapabilityAvailability::Unavailable),
                capability("tool_calls", CapabilityAvailability::Unavailable),
                capability("commands", CapabilityAvailability::Unavailable),
            ],
        })
    }

    async fn discover_sources(
        &self,
        _installation: &DetectedInstallation,
    ) -> Result<Vec<ImportSource>, AdapterError> {
        // No stable public semantic source is currently documented. Detection and the
        // generic wrapper remain useful without guessing at an unstable private format.
        Ok(Vec::new())
    }

    async fn import(
        &self,
        _source: &ImportSource,
        _cursor: Option<ImportCursor>,
        _sink: &dyn EventSink,
    ) -> Result<ImportOutcome, AdapterError> {
        Err(AdapterError::UnsupportedSource(
            "Command Code semantic import is not available; use the generic wrapper".to_owned(),
        ))
    }

    async fn tail(
        &self,
        _source: &ImportSource,
        _cursor: ImportCursor,
        _sink: &dyn EventSink,
        _cancel: CancellationToken,
    ) -> Result<(), AdapterError> {
        Err(AdapterError::UnsupportedSource(
            "Command Code semantic tailing is not available; use the generic wrapper".to_owned(),
        ))
    }
}

fn executable_path(directory: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        directory.join(format!("{name}.exe"))
    } else {
        directory.join(name)
    }
}

fn capability(name: &str, availability: CapabilityAvailability) -> Capability {
    let detail = (availability == CapabilityAvailability::Unavailable)
        .then(|| "No documented stable local semantic source was found.".to_owned());
    Capability {
        name: name.to_owned(),
        availability,
        detail,
    }
}
