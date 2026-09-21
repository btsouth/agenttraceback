use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{EVENT_SCHEMA_VERSION, EntityId};

/// Maximum UTF-8 bytes in a persisted, search-safe event preview.
pub const MAX_PREVIEW_BYTES: usize = 64 * 1024;
const MAX_ID_BYTES: usize = 512;

/// A normalized event action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventAction {
    /// A session begins.
    SessionStart,
    /// A session ends.
    SessionEnd,
    /// A user or harness prompt.
    Prompt,
    /// An assistant response.
    Response,
    /// An agent tool call.
    ToolCall,
    /// An agent tool result.
    ToolResult,
    /// A subagent begins.
    SubagentStart,
    /// A subagent ends.
    SubagentEnd,
    /// A process starts.
    ProcessStart,
    /// A process exits.
    ProcessExit,
    /// A command is executed.
    CommandExecute,
    /// A command result is observed.
    CommandResult,
    /// A file is read.
    FileRead,
    /// A file is created.
    FileCreate,
    /// A file is written.
    FileWrite,
    /// A file is deleted.
    FileDelete,
    /// A file is renamed.
    FileRename,
    /// Git worktree or index state is captured.
    GitStatus,
    /// A commit is observed or reported.
    GitCommit,
    /// A branch operation is observed or reported.
    GitBranch,
    /// A checkout operation is observed or reported.
    GitCheckout,
    /// A test starts.
    TestStart,
    /// A test result is known.
    TestResult,
    /// A network request is reported or deeply observed.
    NetworkRequest,
    /// An agent asks for permission.
    PermissionRequest,
    /// A permission decision is known.
    PermissionResult,
    /// A risk rule matched.
    RiskDetected,
    /// A recovery point is created.
    RecoveryPoint,
    /// A capture gap is recorded.
    RecorderGap,
    /// An adapter reports an error.
    AdapterError,
    /// A newer action value that this build does not understand.
    Unknown,
}

impl EventAction {
    /// Returns the stable snake_case value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::SessionEnd => "session_end",
            Self::Prompt => "prompt",
            Self::Response => "response",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::SubagentStart => "subagent_start",
            Self::SubagentEnd => "subagent_end",
            Self::ProcessStart => "process_start",
            Self::ProcessExit => "process_exit",
            Self::CommandExecute => "command_execute",
            Self::CommandResult => "command_result",
            Self::FileRead => "file_read",
            Self::FileCreate => "file_create",
            Self::FileWrite => "file_write",
            Self::FileDelete => "file_delete",
            Self::FileRename => "file_rename",
            Self::GitStatus => "git_status",
            Self::GitCommit => "git_commit",
            Self::GitBranch => "git_branch",
            Self::GitCheckout => "git_checkout",
            Self::TestStart => "test_start",
            Self::TestResult => "test_result",
            Self::NetworkRequest => "network_request",
            Self::PermissionRequest => "permission_request",
            Self::PermissionResult => "permission_result",
            Self::RiskDetected => "risk_detected",
            Self::RecoveryPoint => "recovery_point",
            Self::RecorderGap => "recorder_gap",
            Self::AdapterError => "adapter_error",
            Self::Unknown => "unknown",
        }
    }

    /// Parses a wire value while retaining an unrecognized original action.
    #[must_use]
    pub fn from_wire(value: &str) -> (Self, Option<String>) {
        let action = match value {
            "session_start" => Self::SessionStart,
            "session_end" => Self::SessionEnd,
            "prompt" => Self::Prompt,
            "response" => Self::Response,
            "tool_call" => Self::ToolCall,
            "tool_result" => Self::ToolResult,
            "subagent_start" => Self::SubagentStart,
            "subagent_end" => Self::SubagentEnd,
            "process_start" => Self::ProcessStart,
            "process_exit" => Self::ProcessExit,
            "command_execute" => Self::CommandExecute,
            "command_result" => Self::CommandResult,
            "file_read" => Self::FileRead,
            "file_create" => Self::FileCreate,
            "file_write" => Self::FileWrite,
            "file_delete" => Self::FileDelete,
            "file_rename" => Self::FileRename,
            "git_status" => Self::GitStatus,
            "git_commit" => Self::GitCommit,
            "git_branch" => Self::GitBranch,
            "git_checkout" => Self::GitCheckout,
            "test_start" => Self::TestStart,
            "test_result" => Self::TestResult,
            "network_request" => Self::NetworkRequest,
            "permission_request" => Self::PermissionRequest,
            "permission_result" => Self::PermissionResult,
            "risk_detected" => Self::RiskDetected,
            "recovery_point" => Self::RecoveryPoint,
            "recorder_gap" => Self::RecorderGap,
            "adapter_error" => Self::AdapterError,
            "unknown" => Self::Unknown,
            _ => return (Self::Unknown, Some(value.to_owned())),
        };
        (action, None)
    }
}

/// The authority that produced a source event.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A local agent session log.
    AgentLog,
    /// An official agent hook.
    AgentHook,
    /// The generic wrapper and PTY stream.
    Wrapper,
    /// A host filesystem observer.
    FilesystemObserver,
    /// A host process observer.
    ProcessObserver,
    /// A read-only Git observer.
    GitObserver,
    /// An action taken inside AgentTraceback.
    UserAction,
    /// The observe-only policy engine.
    PolicyEngine,
    /// An ingestion path that inherits its original source.
    Importer,
    /// A source value unknown to this build.
    Unknown,
}

impl SourceKind {
    /// Returns the stable snake_case value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentLog => "agent_log",
            Self::AgentHook => "agent_hook",
            Self::Wrapper => "wrapper",
            Self::FilesystemObserver => "filesystem_observer",
            Self::ProcessObserver => "process_observer",
            Self::GitObserver => "git_observer",
            Self::UserAction => "user_action",
            Self::PolicyEngine => "policy_engine",
            Self::Importer => "importer",
            Self::Unknown => "unknown",
        }
    }
}

/// User-facing source evidence class.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    /// A source associated with an agent says the event occurred.
    Reported,
    /// AgentTraceback independently observed an effect or execution.
    Observed,
    /// A logical projection backed by immutable independent correlation.
    Verified,
}

impl EvidenceClass {
    /// Returns the stable snake_case value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::Observed => "observed",
            Self::Verified => "verified",
        }
    }
}

/// Confidence that an observed event belongs to a session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionConfidence {
    /// Heuristic association only.
    Low,
    /// Time and project scope agree but multiple writers are possible.
    Medium,
    /// One active session owns the scope and path/time/hash evidence agrees.
    High,
    /// Stable session ID, process ancestry, or adapter event directly links it.
    Exact,
    /// Real evidence that cannot safely be assigned to a session.
    Unattributed,
}

impl AttributionConfidence {
    /// Returns the stable snake_case value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Exact => "exact",
            Self::Unattributed => "unattributed",
        }
    }
}

/// A normalized action result status.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    /// The action succeeded.
    Success,
    /// The action failed.
    Failed,
    /// The action is still running.
    Running,
    /// The action was cancelled.
    Cancelled,
    /// The source did not provide a status.
    Unknown,
}

impl ResultStatus {
    /// Returns the stable snake_case value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Running => "running",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
}

/// Risk severity assigned by an explainable rule.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskSeverity {
    /// No finding is attached.
    None,
    /// Notable but expected.
    Info,
    /// Worth awareness.
    Low,
    /// Potentially consequential.
    Medium,
    /// Destructive, credential-sensitive, or outside expected scope.
    High,
    /// Evidence of broad destructive behavior or confirmed exfiltration.
    Critical,
}

impl RiskSeverity {
    /// Returns the stable snake_case value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Info => "info",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// Kind of target addressed by an event.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// No concrete target.
    None,
    /// A file or directory path.
    File,
    /// A process.
    Process,
    /// A command.
    Command,
    /// A Git repository or reference.
    Git,
    /// A network endpoint.
    Network,
    /// A model or provider.
    Model,
    /// A session.
    Session,
    /// A test.
    Test,
    /// A source unknown to this build.
    Unknown,
}

impl TargetKind {
    /// Returns the stable snake_case value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::File => "file",
            Self::Process => "process",
            Self::Command => "command",
            Self::Git => "git",
            Self::Network => "network",
            Self::Model => "model",
            Self::Session => "session",
            Self::Test => "test",
            Self::Unknown => "unknown",
        }
    }
}

/// Source metadata for one normalized event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSource {
    /// Source authority.
    pub kind: SourceKind,
    /// Original authority retained when this event passes through an importer.
    pub original_source_kind: Option<SourceKind>,
    /// Detected adapter installation when applicable.
    pub adapter_id: Option<String>,
    /// Native or generated source-event identifier.
    pub source_event_id: String,
    /// BLAKE3 digest of the encrypted raw payload.
    pub raw_blob_id: Option<String>,
}

impl EventSource {
    /// Resolves the immutable evidence class represented by this source.
    pub fn effective_evidence_class(&self) -> Result<EvidenceClass, EventValidationError> {
        evidence_class_for(self.kind, self.original_source_kind)
    }
}

fn evidence_class_for(
    kind: SourceKind,
    original_source_kind: Option<SourceKind>,
) -> Result<EvidenceClass, EventValidationError> {
    match kind {
        SourceKind::AgentLog | SourceKind::AgentHook => Ok(EvidenceClass::Reported),
        SourceKind::Wrapper
        | SourceKind::FilesystemObserver
        | SourceKind::ProcessObserver
        | SourceKind::GitObserver
        | SourceKind::UserAction => Ok(EvidenceClass::Observed),
        SourceKind::Importer => match original_source_kind {
            Some(SourceKind::Importer) | None => {
                Err(EventValidationError::MissingOriginalSourceKind)
            }
            Some(original) => evidence_class_for(original, None),
        },
        SourceKind::PolicyEngine => Err(EventValidationError::PolicyEngineIsNotEvidence),
        // Newer source kinds keep the raw payload and use the class supplied by
        // the adapter, which validation still constrains to reported/observed.
        SourceKind::Unknown => Ok(original_source_kind
            .map(|kind| match kind {
                SourceKind::AgentLog | SourceKind::AgentHook => EvidenceClass::Reported,
                _ => EvidenceClass::Observed,
            })
            .unwrap_or(EvidenceClass::Observed)),
    }
}

/// Actor metadata for one event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventActor {
    /// Agent display or stable name.
    pub agent: Option<String>,
    /// Model name.
    pub model: Option<String>,
    /// Process identifier.
    pub pid: Option<u32>,
    /// Parent process identifier.
    pub parent_pid: Option<u32>,
    /// Subagent identifier.
    pub subagent_id: Option<String>,
}

/// Target metadata for one event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventTarget {
    /// Target category.
    pub kind: TargetKind,
    /// Human-readable target.
    pub display: Option<String>,
    /// Project-relative normalized path when applicable.
    pub normalized_path: Option<String>,
    /// Whether the target is outside the registered project root.
    pub external: bool,
}

/// Result metadata for one event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventResult {
    /// Normalized result.
    pub status: ResultStatus,
    /// Process or tool exit code.
    pub exit_code: Option<i32>,
    /// Duration in milliseconds.
    pub duration_ms: Option<u64>,
}

/// Redacted content and content identity for one event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventContent {
    /// Search-safe redacted preview.
    pub redacted_preview: Option<String>,
    /// Encrypted content or result payload digest.
    pub payload_blob_id: Option<String>,
    /// Before-state hash.
    pub before_hash: Option<String>,
    /// After-state hash.
    pub after_hash: Option<String>,
    /// Byte delta when known.
    pub bytes_changed: Option<i64>,
}

/// Evidence provenance for one source event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEvidence {
    /// Immutable stored evidence class.
    pub class: EvidenceClass,
    /// Attribution confidence.
    pub attribution: AttributionConfidence,
    /// Correlation algorithm version when this is a logical projection.
    pub correlation_version: Option<u16>,
}

/// Risk summary for one event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRisk {
    /// Highest attached finding severity.
    pub severity: RiskSeverity,
    /// Attached finding IDs.
    pub finding_ids: Vec<String>,
}

/// Integrity hashes for a persisted event projection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventIntegrity {
    /// Previous session-chain hash.
    pub previous_hash: String,
    /// Hash of this chain entry.
    pub event_hash: String,
}

/// Versioned normalized event envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope {
    /// Normalized event schema version.
    pub schema_version: u16,
    /// UUIDv7 event identifier.
    pub id: EntityId,
    /// Session identifier, when attributed.
    pub session_id: Option<EntityId>,
    /// Project identifier, when known.
    pub project_id: Option<EntityId>,
    /// Source occurrence time in UTC microseconds.
    pub occurred_at_us: i64,
    /// Recorder observation time in UTC microseconds.
    pub observed_at_us: i64,
    /// Optional monotonic clock reading in nanoseconds.
    pub monotonic_ns: Option<u64>,
    /// Per-session sequence when assigned.
    pub sequence: Option<u64>,
    /// Source metadata.
    pub source: EventSource,
    /// Normalized action.
    pub action: EventAction,
    /// Original action value when `action` is `unknown`.
    pub raw_action: Option<String>,
    /// Actor metadata.
    pub actor: EventActor,
    /// Target metadata.
    pub target: EventTarget,
    /// Result metadata.
    pub result: EventResult,
    /// Redacted content metadata.
    pub content: EventContent,
    /// Evidence provenance.
    pub evidence: EventEvidence,
    /// Risk classification.
    pub risk: EventRisk,
    /// Integrity hashes once persisted.
    pub integrity: Option<EventIntegrity>,
}

impl EventEnvelope {
    /// Creates a minimally valid event with unknown optional metadata.
    #[must_use]
    pub fn new(
        source: EventSource,
        action: EventAction,
        occurred_at_us: i64,
        observed_at_us: i64,
    ) -> Self {
        let evidence_class = source
            .effective_evidence_class()
            .unwrap_or(EvidenceClass::Observed);
        Self {
            schema_version: u16::try_from(EVENT_SCHEMA_VERSION).unwrap_or(u16::MAX),
            id: EntityId::new(),
            session_id: None,
            project_id: None,
            occurred_at_us,
            observed_at_us,
            monotonic_ns: None,
            sequence: None,
            source,
            action,
            raw_action: None,
            actor: EventActor {
                agent: None,
                model: None,
                pid: None,
                parent_pid: None,
                subagent_id: None,
            },
            target: EventTarget {
                kind: TargetKind::None,
                display: None,
                normalized_path: None,
                external: false,
            },
            result: EventResult {
                status: ResultStatus::Unknown,
                exit_code: None,
                duration_ms: None,
            },
            content: EventContent {
                redacted_preview: None,
                payload_blob_id: None,
                before_hash: None,
                after_hash: None,
                bytes_changed: None,
            },
            evidence: EventEvidence {
                class: evidence_class,
                attribution: AttributionConfidence::Unattributed,
                correlation_version: None,
            },
            risk: EventRisk {
                severity: RiskSeverity::None,
                finding_ids: Vec::new(),
            },
            integrity: None,
        }
    }

    /// Validates invariants before the event enters persistence.
    pub fn validate(&self) -> Result<(), EventValidationError> {
        if self.schema_version != u16::try_from(EVENT_SCHEMA_VERSION).unwrap_or(u16::MAX) {
            return Err(EventValidationError::SchemaVersion(self.schema_version));
        }
        if self.occurred_at_us <= 0 || self.observed_at_us <= 0 {
            return Err(EventValidationError::Timestamp);
        }
        if self.observed_at_us.saturating_add(86_400_000_000) < self.occurred_at_us {
            return Err(EventValidationError::TimestampSkew);
        }
        if self.source.source_event_id.is_empty()
            || self.source.source_event_id.len() > MAX_ID_BYTES
        {
            return Err(EventValidationError::SourceEventId);
        }
        if self.action == EventAction::Unknown && self.raw_action.is_none() {
            return Err(EventValidationError::MissingRawAction);
        }
        if self.evidence.class == EvidenceClass::Verified {
            return Err(EventValidationError::SourceCannotBeVerified);
        }
        let expected_evidence = self.source.effective_evidence_class()?;
        if self.evidence.class != expected_evidence {
            return Err(EventValidationError::EvidenceClassMismatch);
        }
        if let Some(preview) = &self.content.redacted_preview
            && preview.len() > MAX_PREVIEW_BYTES
        {
            return Err(EventValidationError::PreviewTooLarge);
        }
        if self.content.bytes_changed.is_some_and(|value| value < 0) {
            return Err(EventValidationError::NegativeByteCount);
        }
        Ok(())
    }
}

/// Event validation error.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum EventValidationError {
    /// The event schema is not supported by this build.
    #[error("unsupported event schema version {0}")]
    SchemaVersion(u16),
    /// A required timestamp is zero or negative.
    #[error("event timestamp is invalid")]
    Timestamp,
    /// Source time is implausibly ahead of observation time.
    #[error("event timestamp skew exceeds the capture bound")]
    TimestampSkew,
    /// A source event identifier is empty or excessively long.
    #[error("source event identifier is invalid")]
    SourceEventId,
    /// An unknown action did not retain its raw value.
    #[error("unknown action is missing its original value")]
    MissingRawAction,
    /// A source event attempted to claim verified status.
    #[error("a source event cannot be stored as verified")]
    SourceCannotBeVerified,
    /// The evidence class does not match the source authority.
    #[error("event evidence class does not match its source kind")]
    EvidenceClassMismatch,
    /// An importer event did not retain its original authority.
    #[error("importer event is missing its original source kind")]
    MissingOriginalSourceKind,
    /// A policy-engine record is not independent evidence.
    #[error("policy-engine output must cite an evidence event instead of claiming one")]
    PolicyEngineIsNotEvidence,
    /// A redacted preview exceeded the persistence bound.
    #[error("event preview exceeds the persistence bound")]
    PreviewTooLarge,
    /// A byte count was negative.
    #[error("byte counts must be non-negative")]
    NegativeByteCount,
}

#[cfg(test)]
mod tests {
    use super::{
        EventAction, EventEnvelope, EventSource, EventValidationError, EvidenceClass, SourceKind,
    };

    fn event(source_kind: SourceKind) -> EventEnvelope {
        EventEnvelope::new(
            EventSource {
                kind: source_kind,
                original_source_kind: None,
                adapter_id: None,
                source_event_id: "source-1".to_owned(),
                raw_blob_id: None,
            },
            EventAction::FileWrite,
            1_700_000_000_000_000,
            1_700_000_000_000_001,
        )
    }

    #[test]
    fn source_evidence_class_is_immutable_by_mapping() {
        assert_eq!(
            event(SourceKind::AgentLog).evidence.class,
            EvidenceClass::Reported
        );
        assert_eq!(
            event(SourceKind::FilesystemObserver).evidence.class,
            EvidenceClass::Observed
        );
    }

    #[test]
    fn source_event_cannot_be_verified() {
        let mut event = event(SourceKind::AgentLog);
        event.evidence.class = EvidenceClass::Verified;
        assert_eq!(
            event.validate(),
            Err(EventValidationError::SourceCannotBeVerified)
        );
    }

    #[test]
    fn valid_event_passes() {
        event(SourceKind::AgentLog).validate().expect("valid event");
    }

    #[test]
    fn newer_action_is_preserved_as_raw_value() {
        let (action, raw) = EventAction::from_wire("future_action");
        assert_eq!(action, EventAction::Unknown);
        assert_eq!(raw.as_deref(), Some("future_action"));
    }
}
