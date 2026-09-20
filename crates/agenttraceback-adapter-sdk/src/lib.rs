//! Agent adapter contract and shared conformance types.

use std::path::PathBuf;

use agenttraceback_types::{EntityId, EventEnvelope};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Adapter operation error.
#[derive(Debug, Error)]
pub enum AdapterError {
    /// Source discovery or read failed.
    #[error("adapter I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// A source record could not be parsed.
    #[error("adapter parse failed: {0}")]
    Parse(String),
    /// The source version is unsupported or degraded.
    #[error("unsupported adapter source: {0}")]
    UnsupportedSource(String),
    /// Hook changes are not supported by this adapter.
    #[error("hook installation is not supported by this adapter")]
    HookUnsupported,
    /// A hook plan no longer matches the target configuration.
    #[error("hook plan is stale: {0}")]
    HookConflict(String),
    /// Sink persistence failed.
    #[error("adapter event sink failed: {0}")]
    Sink(String),
}

/// Capability values exposed by detection and settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAvailability {
    /// Capability is available.
    Available,
    /// Capability is unavailable.
    Unavailable,
    /// Capability is available with limitations.
    Degraded,
}

/// One declared capability.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    /// Stable capability name.
    pub name: String,
    /// Availability.
    pub availability: CapabilityAvailability,
    /// Truthful limitation detail.
    pub detail: Option<String>,
}

/// Complete capability declaration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitySet {
    /// Capabilities.
    pub capabilities: Vec<Capability>,
}

impl CapabilitySet {
    /// Returns one capability.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Capability> {
        self.capabilities
            .iter()
            .find(|capability| capability.name == name)
    }
}

/// Adapter identity and version contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterDescriptor {
    /// Stable adapter ID.
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// Adapter schema version.
    pub schema_version: u16,
    /// Known source locations.
    pub source_locations: Vec<PathBuf>,
    /// Whether richer capture requires a hook or config change.
    pub richer_capture_requires_change: bool,
    /// Upstream documentation.
    pub documentation_url: Option<String>,
}

/// Detection context.
#[derive(Clone, Debug, Default)]
pub struct DetectContext {
    /// Current user's home directory.
    pub home_dir: Option<PathBuf>,
    /// Executable PATH entries.
    pub path_entries: Vec<PathBuf>,
}

/// One detected installation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedInstallation {
    /// Installation identifier.
    pub id: EntityId,
    /// Adapter ID.
    pub adapter_id: String,
    /// Display name.
    pub display_name: String,
    /// Detected agent version.
    pub agent_version: Option<String>,
    /// Known source roots.
    pub source_roots: Vec<PathBuf>,
    /// Detection diagnostic.
    pub diagnostic_code: Option<String>,
}

/// One importable source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSource {
    /// Source identifier.
    pub id: EntityId,
    /// Adapter ID.
    pub adapter_id: String,
    /// Canonical source path or database location.
    pub canonical_location: PathBuf,
    /// Stable identity used for cursors.
    pub stable_identity: String,
    /// Source kind.
    pub source_kind: String,
}

/// Transactionally persisted import cursor.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCursor {
    /// Cursor schema version.
    pub version: u16,
    /// Byte offset for append-only files.
    pub byte_offset: u64,
    /// Adapter-native cursor.
    pub native_cursor: Option<String>,
    /// Last observed source size.
    pub source_size: Option<u64>,
    /// Last observed modification time.
    pub source_mtime_us: Option<i64>,
    /// Last emitted event.
    pub last_event_id: Option<EntityId>,
}

/// Result of one bounded import batch.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    /// Events emitted.
    pub events_imported: u64,
    /// New cursor after the batch.
    pub cursor: ImportCursor,
    /// Parser version.
    pub parser_version: String,
    /// Quarantined malformed records.
    pub quarantined: u64,
    /// Degraded parser warnings.
    pub warnings: Vec<String>,
}

/// Raw normalized event ready for the daemon pipeline.
#[derive(Clone, Debug)]
pub struct AdapterEvent {
    /// Normalized event envelope.
    pub event: EventEnvelope,
    /// Raw source bytes retained encrypted whenever policy permits.
    pub raw_payload: Option<Vec<u8>>,
    /// Session metadata that the daemon must idempotently provision.
    pub session: Option<AdapterSessionMetadata>,
}

/// Metadata emitted while importing a native session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterSessionMetadata {
    /// Native source session identifier.
    pub native_session_id: String,
    /// Project or working directory.
    pub project_root: Option<PathBuf>,
    /// Redacted title preview.
    pub title_preview: Option<String>,
    /// Agent name.
    pub agent_name: String,
    /// Harness name.
    pub harness_name: Option<String>,
    /// Provider name.
    pub provider_name: Option<String>,
    /// Model name.
    pub model_name: Option<String>,
    /// Historical session flag.
    pub historical: bool,
}

/// Adapter event destination.
#[async_trait]
pub trait EventSink: Send + Sync {
    /// Emits one event and returns after durable acceptance.
    async fn emit(&self, event: AdapterEvent) -> Result<(), AdapterError>;
}

/// Proposed reversible hook change.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookPlan {
    /// Adapter ID.
    pub adapter_id: String,
    /// Configuration file.
    pub config_path: PathBuf,
    /// Current file digest.
    pub before_hash: String,
    /// Plan digest.
    pub plan_digest: String,
    /// Human-readable planned diff.
    pub preview: String,
}

/// Receipt proving a reversible hook install.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookReceipt {
    /// Adapter ID.
    pub adapter_id: String,
    /// Configuration file.
    pub config_path: PathBuf,
    /// Backup file.
    pub backup_path: PathBuf,
    /// Installed plan digest.
    pub plan_digest: String,
}

/// First-party adapter contract.
#[async_trait]
pub trait AgentAdapter: Send + Sync {
    /// Stable adapter descriptor.
    fn descriptor(&self) -> AdapterDescriptor;
    /// Detects known installations and source roots.
    async fn detect(
        &self,
        context: &DetectContext,
    ) -> Result<Vec<DetectedInstallation>, AdapterError>;
    /// Returns honest per-installation capabilities.
    async fn capabilities(
        &self,
        installation: &DetectedInstallation,
    ) -> Result<CapabilitySet, AdapterError>;
    /// Discovers import sources without recursively scanning a home directory.
    async fn discover_sources(
        &self,
        installation: &DetectedInstallation,
    ) -> Result<Vec<ImportSource>, AdapterError>;
    /// Imports bounded records from a source.
    async fn import(
        &self,
        source: &ImportSource,
        cursor: Option<ImportCursor>,
        sink: &dyn EventSink,
    ) -> Result<ImportOutcome, AdapterError>;
    /// Tails appended records until cancellation.
    async fn tail(
        &self,
        source: &ImportSource,
        cursor: ImportCursor,
        sink: &dyn EventSink,
        cancel: CancellationToken,
    ) -> Result<(), AdapterError>;
    /// Plans a reversible hook/config change.
    async fn plan_hook_install(
        &self,
        _installation: &DetectedInstallation,
    ) -> Result<Option<HookPlan>, AdapterError> {
        Ok(None)
    }
    /// Applies an approved hook plan.
    async fn install_hook(&self, _approved_plan: HookPlan) -> Result<HookReceipt, AdapterError> {
        Err(AdapterError::HookUnsupported)
    }
    /// Reverses a previously installed hook.
    async fn uninstall_hook(&self, _receipt: HookReceipt) -> Result<(), AdapterError> {
        Err(AdapterError::HookUnsupported)
    }
}
