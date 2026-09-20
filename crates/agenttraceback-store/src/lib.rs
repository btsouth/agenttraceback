//! SQLite migrations, repositories, full-text indexing, and append-only integrity chains.

mod migration;
mod writer;

use std::{
    io,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
};

use agenttraceback_crypto::{
    ChainEntry, ChainError, ChainHash, ChainVerificationReport, ChainVerifier, canonical_digest,
};
use agenttraceback_search::{FilterValue, SearchQuery};
use agenttraceback_types::{EntityId, EventEnvelope, EventValidationError};
use rusqlite::{Connection, OptionalExtension, types::Value};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use writer::{WriteCommand, hash_array, spawn_writer};

/// Convenient result alias for storage operations.
pub type StoreResult<T> = Result<T, StoreError>;

/// Metadata persisted for one encrypted content-addressed blob.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobMetadata {
    /// Plaintext digest in lowercase hexadecimal.
    pub id: String,
    /// Plaintext digest, duplicated for indexed integrity checks.
    pub digest: String,
    /// On-disk format version.
    pub format_version: u16,
    /// Plaintext byte count.
    pub plaintext_bytes: u64,
    /// Encrypted file byte count.
    pub encrypted_bytes: u64,
    /// Media category such as `raw_event`, `preview`, or `file_content`.
    pub media_category: String,
    /// Retention class such as `raw_payload` or `recovery_snapshot`.
    pub retention_class: String,
    /// Creation time in UTC microseconds.
    pub created_at_us: i64,
    /// Optional expiration time in UTC microseconds.
    pub expires_at_us: Option<i64>,
}

/// One ranked search result.
#[derive(Clone, Debug)]
pub struct SearchHit {
    /// Event identifier.
    pub event_id: EntityId,
    /// Session identifier when linked.
    pub session_id: Option<EntityId>,
    /// Event occurrence time in UTC microseconds.
    pub occurred_at_us: i64,
    /// Normalized action.
    pub action: String,
    /// Agent name.
    pub agent: Option<String>,
    /// Model name.
    pub model: Option<String>,
    /// Project display name.
    pub project_name: Option<String>,
    /// Target display.
    pub target_display: Option<String>,
    /// Redacted preview.
    pub redacted_preview: Option<String>,
    /// Evidence class.
    pub evidence: String,
    /// Result status.
    pub status: String,
    /// Risk severity.
    pub risk: String,
    /// FTS rank; lower is better.
    pub rank: Option<f64>,
}

/// A cursor-free first-page search response.
#[derive(Clone, Debug)]
pub struct SearchPage {
    /// Result rows.
    pub items: Vec<SearchHit>,
    /// Whether at least one more row exists.
    pub has_more: bool,
}

/// Persisted project identity and root metadata.
#[derive(Clone, Debug)]
pub struct ProjectRecord {
    /// Project identifier.
    pub id: EntityId,
    /// Human-readable project name.
    pub display_name: String,
    /// Canonical root path.
    pub canonical_root: String,
    /// Normalized comparison root.
    pub comparison_root: String,
    /// Version-control kind.
    pub vcs_kind: String,
    /// Additional roots associated with the project.
    pub roots: Vec<ProjectRootRecord>,
    /// Creation time in UTC microseconds.
    pub created_at_us: i64,
    /// Last observation time in UTC microseconds.
    pub last_seen_at_us: i64,
}

/// One project root.
#[derive(Clone, Debug)]
pub struct ProjectRootRecord {
    /// Root identifier.
    pub id: EntityId,
    /// Canonical root path.
    pub canonical_path: String,
    /// Normalized comparison path.
    pub comparison_path: String,
    /// Root category.
    pub root_kind: String,
    /// Whether the root participates in capture.
    pub enabled: bool,
}

/// Persisted session header.
#[derive(Clone, Debug)]
pub struct SessionRecord {
    /// Session identifier.
    pub id: EntityId,
    /// Project identifier when attached.
    pub project_id: Option<EntityId>,
    /// Redacted title preview.
    pub title_preview: Option<String>,
    /// Lifecycle state.
    pub state: String,
    /// Start time in UTC microseconds.
    pub started_at_us: i64,
    /// End time in UTC microseconds.
    pub ended_at_us: Option<i64>,
    /// Agent name.
    pub agent_name: Option<String>,
    /// Harness name.
    pub harness_name: Option<String>,
    /// Provider name.
    pub provider_name: Option<String>,
    /// Model name.
    pub model_name: Option<String>,
    /// Native source session ID.
    pub source_session_id: Option<String>,
    /// Outcome value.
    pub outcome: String,
    /// Outcome confidence.
    pub outcome_confidence: String,
    /// Recovery coverage.
    pub recovery_coverage: String,
    /// Capture health.
    pub capture_health: String,
}

/// Dashboard/list projection for one project.
#[derive(Clone, Debug)]
pub struct ProjectSummaryRecord {
    /// Project identifier.
    pub id: EntityId,
    /// Display name.
    pub display_name: String,
    /// Canonical root.
    pub canonical_root: String,
    /// Version-control kind.
    pub vcs_kind: String,
    /// Last observation time.
    pub last_seen_at_us: i64,
    /// Number of sessions.
    pub session_count: u64,
    /// Number of linked events.
    pub event_count: u64,
}

/// Session list/detail projection with deterministic aggregate counts.
#[derive(Clone, Debug)]
pub struct SessionSummaryRecord {
    /// Session identifier.
    pub id: EntityId,
    /// Project identifier.
    pub project_id: Option<EntityId>,
    /// Project display name.
    pub project_name: Option<String>,
    /// Redacted title or task preview.
    pub title_preview: Option<String>,
    /// Lifecycle state.
    pub state: String,
    /// Start time.
    pub started_at_us: i64,
    /// End time.
    pub ended_at_us: Option<i64>,
    /// Agent name.
    pub agent_name: Option<String>,
    /// Harness name.
    pub harness_name: Option<String>,
    /// Provider name.
    pub provider_name: Option<String>,
    /// Model name.
    pub model_name: Option<String>,
    /// Outcome.
    pub outcome: String,
    /// Outcome confidence.
    pub outcome_confidence: String,
    /// Recovery coverage.
    pub recovery_coverage: String,
    /// Capture health.
    pub capture_health: String,
    /// Highest risk severity.
    pub risk_max_severity: String,
    /// Linked event count.
    pub event_count: u64,
    /// Distinct changed file count.
    pub file_count: u64,
    /// Finding count.
    pub finding_count: u64,
}

/// Cross-session finding projection for the Findings screen.
#[derive(Clone, Debug)]
pub struct FindingSummaryRecord {
    /// Finding identifier.
    pub id: EntityId,
    /// Session identifier when linked.
    pub session_id: Option<EntityId>,
    /// Event identifier.
    pub event_id: EntityId,
    /// Project name.
    pub project_name: Option<String>,
    /// Agent name.
    pub agent_name: Option<String>,
    /// Rule identifier.
    pub rule_id: String,
    /// Rule version.
    pub rule_version: String,
    /// Severity.
    pub severity: String,
    /// Status.
    pub status: String,
    /// Title.
    pub title: String,
    /// Explanation.
    pub explanation: String,
    /// Redacted match.
    pub matched_preview: Option<String>,
    /// Creation time.
    pub created_at_us: i64,
}

/// Cross-session file history projection.
#[derive(Clone, Debug)]
pub struct FileSummaryRecord {
    /// File identity.
    pub id: EntityId,
    /// Project name.
    pub project_name: String,
    /// Current display path.
    pub path: String,
    /// Sensitive content class.
    pub sensitive_class: Option<String>,
    /// First observation time.
    pub first_seen_at_us: i64,
    /// Last observation time.
    pub last_seen_at_us: i64,
    /// Number of versions.
    pub version_count: u64,
    /// Number of sessions.
    pub session_count: u64,
    /// Versions with encrypted content.
    pub recoverable_count: u64,
}

/// Persisted adapter installation.
#[derive(Clone, Debug)]
pub struct AdapterInstallationRecord {
    /// Installation identifier.
    pub id: EntityId,
    /// Adapter ID.
    pub adapter_id: String,
    /// Display name.
    pub display_name: String,
    /// Agent version.
    pub agent_version: Option<String>,
    /// Detection time.
    pub detected_at_us: i64,
    /// Last observation time.
    pub last_seen_at_us: i64,
    /// Status.
    pub status: String,
    /// Diagnostic code.
    pub diagnostic_code: Option<String>,
}

/// Persisted import source.
#[derive(Clone, Debug)]
pub struct ImportSourceRecord {
    /// Source identifier.
    pub id: EntityId,
    /// Adapter installation.
    pub adapter_installation_id: EntityId,
    /// Source kind.
    pub source_kind: String,
    /// Canonical location.
    pub canonical_location: String,
    /// Stable identity.
    pub stable_identity: String,
    /// Source status.
    pub status: String,
    /// Last error code.
    pub last_error_code: Option<String>,
}

/// Persisted import cursor.
#[derive(Clone, Debug)]
pub struct ImportCursorRecord {
    /// Source identifier.
    pub import_source_id: EntityId,
    /// Cursor version.
    pub cursor_version: u16,
    /// Byte offset.
    pub byte_offset: u64,
    /// Native cursor.
    pub native_cursor: Option<String>,
    /// Source size.
    pub source_size: Option<u64>,
    /// Source modification time.
    pub source_mtime_us: Option<i64>,
    /// Last event.
    pub last_event_id: Option<EntityId>,
    /// Update time.
    pub updated_at_us: i64,
}

/// Persisted receipt for one installed adapter hook.
#[derive(Clone, Debug)]
pub struct AdapterHookRecord {
    /// Receipt identifier.
    pub id: EntityId,
    /// Stable adapter identifier.
    pub adapter_id: String,
    /// Detection installation associated with the install.
    pub installation_id: Option<EntityId>,
    /// Modified configuration path.
    pub config_path: PathBuf,
    /// Timestamped backup path.
    pub backup_path: PathBuf,
    /// Approved plan digest.
    pub plan_digest: String,
    /// Installation time in UTC microseconds.
    pub installed_at_us: i64,
}

/// Persisted baseline or recovery snapshot header.
#[derive(Clone, Debug)]
pub struct SnapshotRecord {
    /// Snapshot identifier.
    pub id: EntityId,
    /// Project identifier.
    pub project_id: EntityId,
    /// Session identifier.
    pub session_id: Option<EntityId>,
    /// Snapshot kind, such as `pre_session`.
    pub snapshot_kind: String,
    /// Snapshot coverage.
    pub coverage: String,
    /// Start time in UTC microseconds.
    pub started_at_us: i64,
    /// Completion time in UTC microseconds.
    pub completed_at_us: Option<i64>,
    /// Canonical manifest hash.
    pub manifest_hash: Option<String>,
    /// Snapshot status.
    pub status: String,
    /// Manifest entries.
    pub entries: Vec<SnapshotEntryRecord>,
}

/// One persisted snapshot manifest entry.
#[derive(Clone, Debug)]
pub struct SnapshotEntryRecord {
    /// File identifier when a file identity row exists.
    pub file_id: Option<EntityId>,
    /// Display path.
    pub display_path: String,
    /// Normalized comparison path.
    pub comparison_path: String,
    /// File version identifier when content metadata exists.
    pub file_version_id: Option<EntityId>,
    /// Entry kind.
    pub entry_kind: String,
    /// Capture status.
    pub capture_status: String,
    /// Git object identifier when available.
    pub git_object_id: Option<String>,
}

/// Persisted file identity within a project.
#[derive(Clone, Debug)]
pub struct FileRecord {
    /// File identifier.
    pub id: EntityId,
    /// Project identifier.
    pub project_id: EntityId,
    /// Current display path.
    pub current_display_path: String,
    /// Current normalized comparison path.
    pub current_comparison_path: String,
    /// Stable filesystem identity when available.
    pub stable_file_identity: Option<String>,
    /// First observation time in UTC microseconds.
    pub first_seen_at_us: i64,
    /// Last observation time in UTC microseconds.
    pub last_seen_at_us: i64,
    /// Sensitive content class when matched.
    pub sensitive_class: Option<String>,
}

/// Persisted version or metadata snapshot for one file.
#[derive(Clone, Debug)]
pub struct FileVersionRecord {
    /// Version identifier.
    pub id: EntityId,
    /// File identifier.
    pub file_id: EntityId,
    /// Session identifier.
    pub session_id: Option<EntityId>,
    /// Source event identifier.
    pub event_id: Option<EntityId>,
    /// Display path at capture time.
    pub display_path_at_time: String,
    /// BLAKE3 content digest.
    pub content_hash: Option<String>,
    /// Encrypted content blob identifier.
    pub blob_id: Option<String>,
    /// Content byte length.
    pub byte_length: Option<u64>,
    /// Media category.
    pub media_kind: Option<String>,
    /// Executable-bit state.
    pub executable: Option<bool>,
    /// Symlink target, never followed.
    pub symlink_target: Option<String>,
    /// Capture status.
    pub capture_status: String,
    /// Creation time in UTC microseconds.
    pub created_at_us: i64,
}

/// Persisted explainable risk finding.
#[derive(Clone, Debug)]
pub struct RiskFindingRecord {
    /// Finding identifier.
    pub id: EntityId,
    /// Session identifier.
    pub session_id: Option<EntityId>,
    /// Event identifier.
    pub event_id: EntityId,
    /// Stable rule ID.
    pub rule_id: String,
    /// Rule version.
    pub rule_version: String,
    /// Severity value.
    pub severity: String,
    /// Initial status.
    pub initial_status: String,
    /// Short title.
    pub title: String,
    /// Rule explanation.
    pub explanation: String,
    /// Redacted matched preview.
    pub matched_preview: Option<String>,
    /// Creation time in UTC microseconds.
    pub created_at_us: i64,
}

/// Joined snapshot/file-version row used by recovery planning.
#[derive(Clone, Debug)]
pub struct SnapshotFileVersion {
    /// File version identifier.
    pub file_version_id: EntityId,
    /// File identity.
    pub file_id: EntityId,
    /// Display path at capture time.
    pub display_path: String,
    /// Normalized comparison path.
    pub comparison_path: String,
    /// BLAKE3 content digest when available.
    pub content_hash: Option<String>,
    /// Encrypted content blob identifier.
    pub blob_id: Option<String>,
    /// Content byte length.
    pub byte_length: Option<u64>,
    /// Symlink target when applicable.
    pub symlink_target: Option<String>,
    /// Capture status.
    pub capture_status: String,
    /// Executable-bit state.
    pub executable: bool,
}

/// Persisted immutable recovery plan.
#[derive(Clone, Debug)]
pub struct RecoveryPlanRecord {
    /// Plan identifier.
    pub id: EntityId,
    /// Session identifier.
    pub session_id: EntityId,
    /// Recovery action.
    pub action: String,
    /// Destination path.
    pub destination: String,
    /// Plan status.
    pub status: String,
    /// Digest of the immutable plan payload.
    pub plan_digest: String,
    /// Encrypted full plan blob.
    pub plan_blob_id: String,
    /// Creation time.
    pub created_at_us: i64,
    /// Execution time.
    pub executed_at_us: Option<i64>,
    /// Result blob.
    pub result_blob_id: Option<String>,
}

/// Persisted recovery execution run.
#[derive(Clone, Debug)]
pub struct RecoveryRunRecord {
    /// Run identifier.
    pub id: EntityId,
    /// Plan identifier.
    pub plan_id: EntityId,
    /// Encrypted pre-restore backup plan, retained on failure.
    pub backup_plan_id: Option<EntityId>,
    /// Run state.
    pub state: String,
    /// Restored file count.
    pub restored_files: u64,
    /// Skipped-file count.
    pub skipped_files: u64,
    /// Conflict count.
    pub conflict_files: u64,
    /// Result blob.
    pub result_blob_id: Option<String>,
    /// Creation time.
    pub created_at_us: i64,
    /// Completion time.
    pub finished_at_us: Option<i64>,
    /// Error code.
    pub error_code: Option<String>,
}

/// Query projection for one risk finding.
#[derive(Clone, Debug)]
pub struct RiskFindingView {
    /// Finding identifier.
    pub id: EntityId,
    /// Event identifier.
    pub event_id: EntityId,
    /// Rule ID.
    pub rule_id: String,
    /// Rule version.
    pub rule_version: String,
    /// Severity.
    pub severity: String,
    /// Current status.
    pub status: String,
    /// Title.
    pub title: String,
    /// Explanation.
    pub explanation: String,
    /// Redacted matched preview.
    pub matched_preview: Option<String>,
    /// Creation time.
    pub created_at_us: i64,
}

/// One snapshot file version with its snapshot phase.
#[derive(Clone, Debug)]
pub struct SessionFileVersion {
    /// Snapshot phase (`pre_session` or `post_session`).
    pub snapshot_kind: String,
    /// File version data.
    pub version: SnapshotFileVersion,
}

/// Correlation record to persist.
#[derive(Clone, Debug)]
pub struct CorrelationInsert {
    /// Correlation identifier.
    pub id: EntityId,
    /// Session identifier.
    pub session_id: Option<EntityId>,
    /// Logical event identifier.
    pub logical_event_id: EntityId,
    /// Reported source event.
    pub reported_event_id: EntityId,
    /// Observed source event.
    pub observed_event_id: EntityId,
    /// Correlation family.
    pub correlation_kind: String,
    /// Attribution confidence.
    pub confidence: String,
    /// Algorithm version.
    pub algorithm_version: u16,
}

/// Correlation conflict record to persist.
#[derive(Clone, Debug)]
pub struct CorrelationConflictInsert {
    /// Conflict identifier.
    pub id: EntityId,
    /// Session identifier.
    pub session_id: Option<EntityId>,
    /// Left source event.
    pub left_event_id: EntityId,
    /// Right source event.
    pub right_event_id: EntityId,
    /// Stable reason code.
    pub reason_code: String,
    /// Explainable detail.
    pub detail: String,
}

/// Persisted Git boundary state.
#[derive(Clone, Debug)]
pub struct GitStateRecord {
    /// Git state identifier.
    pub id: EntityId,
    /// Session identifier.
    pub session_id: EntityId,
    /// Boundary phase, such as `before` or `after`.
    pub snapshot_phase: String,
    /// Repository root.
    pub repo_root: String,
    /// HEAD object ID.
    pub head_oid: Option<String>,
    /// Branch name.
    pub branch_name: Option<String>,
    /// Upstream branch.
    pub upstream_name: Option<String>,
    /// Encrypted status payload blob ID.
    pub status_blob_id: Option<String>,
    /// Capture time in UTC microseconds.
    pub captured_at_us: i64,
}

/// Errors returned by the event store.
#[derive(Debug, Error)]
pub enum StoreError {
    /// A filesystem operation failed.
    #[error("store filesystem operation failed at {path}: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// SQLite rejected or failed an operation.
    #[error("SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The database was created by a newer AgentTraceback version.
    #[error("database schema {found} is newer than supported schema {supported}")]
    UnsupportedSchema {
        /// Schema found on disk.
        found: i64,
        /// Schema supported by this build.
        supported: i64,
    },
    /// A normalized event failed validation.
    #[error(transparent)]
    Validation(#[from] EventValidationError),
    /// Chain canonicalization or hashing failed.
    #[error(transparent)]
    Chain(#[from] ChainError),
    /// A persisted envelope could not be encoded or decoded.
    #[error("stored event envelope is invalid: {0}")]
    EnvelopeJson(#[source] serde_json::Error),
    /// A stored value violated an expected type or range.
    #[error("stored value is invalid: {0}")]
    InvalidStoredValue(&'static str),
    /// The writer actor stopped accepting commands.
    #[error("event writer is unavailable")]
    WriterUnavailable,
    /// A session or global chain has already been finalized.
    #[error("the requested integrity chain is already finalized")]
    ChainFinalized,
    /// A chain cannot be finalized before it has any entries.
    #[error("the requested integrity chain is empty")]
    EmptyChain,
    /// A blocking storage task could not be joined.
    #[error("storage task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    /// An in-process reader lock was poisoned.
    #[error("storage reader lock was poisoned")]
    ReaderPoisoned,
    /// The writer thread could not be started.
    #[error("event writer thread could not start: {0}")]
    WriterThread(#[source] io::Error),
}

/// A cloneable handle to the single-writer, multi-reader event store.
#[derive(Clone)]
pub struct Store {
    inner: Arc<StoreInner>,
}

struct StoreInner {
    writes: mpsc::Sender<WriteCommand>,
    readers: ReaderPool,
}

impl Store {
    /// Opens or creates the SQLite store and starts the dedicated writer actor.
    pub async fn open(path: impl AsRef<Path>, backup_root: impl AsRef<Path>) -> StoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        let backup_root = backup_root.as_ref().to_path_buf();
        let (writer_connection, readers) = tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            let writer = migration::open_writer(&path, &backup_root)?;
            let readers = ReaderPool::open(&path)?;
            Ok::<_, StoreError>((writer, readers))
        })
        .await??;

        let (writes, receiver) = mpsc::channel(512);
        spawn_writer(writer_connection, receiver).map_err(StoreError::WriterThread)?;
        Ok(Self {
            inner: Arc::new(StoreInner { writes, readers }),
        })
    }

    /// Returns the database path used by this store.
    #[must_use]
    pub fn database_path(&self) -> &Path {
        self.inner.readers.path()
    }

    /// Appends events and advances their session/global integrity chains atomically.
    pub async fn append_events(
        &self,
        events: Vec<EventEnvelope>,
    ) -> StoreResult<Vec<EventEnvelope>> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::AppendEvents { events, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Records or refreshes metadata for an encrypted blob.
    pub async fn record_blob(&self, metadata: BlobMetadata) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::RecordBlob { metadata, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Executes structured FTS search with indexed filters.
    pub async fn search(&self, query: SearchQuery, limit: u16) -> StoreResult<SearchPage> {
        let readers = self.inner.readers.clone();
        let limit = limit.clamp(1, 500);
        tokio::task::spawn_blocking(move || search_sync(&readers, &query, limit)).await?
    }

    /// Finalizes a session or global chain and records its root.
    pub async fn finalize_chain(&self, session_id: Option<EntityId>) -> StoreResult<EntityId> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::FinalizeChain { session_id, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Inserts or updates a project and its roots.
    pub async fn upsert_project(&self, project: ProjectRecord) -> StoreResult<EntityId> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::UpsertProject { project, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Inserts a discovered or active session.
    pub async fn create_session(&self, session: SessionRecord) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::CreateSession { session, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Inserts or refreshes a session header without overwriting a completed state.
    pub async fn upsert_session(&self, session: SessionRecord) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::UpsertSession { session, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Upserts an adapter installation.
    pub async fn upsert_adapter_installation(
        &self,
        installation: AdapterInstallationRecord,
    ) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::UpsertAdapterInstallation {
                installation,
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Upserts an import source.
    pub async fn upsert_import_source(&self, source: ImportSourceRecord) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::UpsertImportSource { source, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Loads an import cursor.
    pub async fn get_import_cursor(
        &self,
        import_source_id: EntityId,
    ) -> StoreResult<Option<ImportCursorRecord>> {
        let readers = self.inner.readers.clone();
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let row = connection
                .query_row(
                    "SELECT import_source_id, cursor_version, byte_offset, native_cursor,
                            source_size, source_mtime_us, last_event_id, updated_at_us
                     FROM import_cursors WHERE import_source_id = ?1",
                    [import_source_id.as_uuid().as_bytes().as_slice()],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<i64>>(4)?,
                            row.get::<_, Option<i64>>(5)?,
                            row.get::<_, Option<Vec<u8>>>(6)?,
                            row.get::<_, i64>(7)?,
                        ))
                    },
                )
                .optional()
                .map_err(StoreError::Sqlite)?;
            row.map(|row| {
                Ok(ImportCursorRecord {
                    import_source_id: writer::uuid_from_blob(&row.0)?,
                    cursor_version: u16::try_from(row.1)
                        .map_err(|_| StoreError::InvalidStoredValue("cursor version"))?,
                    byte_offset: u64::try_from(row.2)
                        .map_err(|_| StoreError::InvalidStoredValue("byte offset"))?,
                    native_cursor: row.3,
                    source_size: row
                        .4
                        .map(|value| {
                            u64::try_from(value)
                                .map_err(|_| StoreError::InvalidStoredValue("source size"))
                        })
                        .transpose()?,
                    source_mtime_us: row.5,
                    last_event_id: row.6.as_deref().map(writer::uuid_from_blob).transpose()?,
                    updated_at_us: row.7,
                })
            })
            .transpose()
        })
        .await?
    }

    /// Saves or replaces the receipt for an installed adapter hook.
    pub async fn save_adapter_hook(&self, hook: AdapterHookRecord) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::SaveAdapterHook { hook, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Loads the persisted receipt for one adapter hook.
    pub async fn get_adapter_hook(
        &self,
        adapter_id: &str,
    ) -> StoreResult<Option<AdapterHookRecord>> {
        let readers = self.inner.readers.clone();
        let adapter_id = adapter_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let row = connection
                .query_row(
                    "SELECT id, adapter_id, installation_id, config_path, backup_path,
                            plan_digest, installed_at_us
                     FROM adapter_hooks WHERE adapter_id = ?1",
                    [adapter_id],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<Vec<u8>>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, i64>(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(StoreError::Sqlite)?;
            row.map(|row| {
                Ok(AdapterHookRecord {
                    id: writer::uuid_from_blob(&row.0)?,
                    adapter_id: row.1,
                    installation_id: row.2.as_deref().map(writer::uuid_from_blob).transpose()?,
                    config_path: PathBuf::from(row.3),
                    backup_path: PathBuf::from(row.4),
                    plan_digest: row.5,
                    installed_at_us: row.6,
                })
            })
            .transpose()
        })
        .await?
    }

    /// Removes a persisted hook receipt after successful uninstall.
    pub async fn delete_adapter_hook(&self, adapter_id: &str) -> StoreResult<bool> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::DeleteAdapterHook {
                adapter_id: adapter_id.to_owned(),
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Persists an import cursor.
    pub async fn save_import_cursor(&self, cursor: ImportCursorRecord) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::SaveImportCursor { cursor, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Finalizes a session header.
    pub async fn complete_session(
        &self,
        session_id: EntityId,
        ended_at_us: i64,
        outcome: String,
        capture_health: String,
    ) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::CompleteSession {
                session_id,
                ended_at_us,
                outcome,
                capture_health,
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Persists a snapshot manifest and entries atomically.
    pub async fn save_snapshot(&self, snapshot: SnapshotRecord) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::SaveSnapshot { snapshot, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Upserts a project file identity and returns its persistent ID.
    pub async fn upsert_file(&self, file: FileRecord) -> StoreResult<EntityId> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::UpsertFile { file, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Inserts one file version or metadata-only version row.
    pub async fn insert_file_version(&self, version: FileVersionRecord) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::InsertFileVersion { version, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Persists findings attached to already-appended events.
    pub async fn save_risk_findings(&self, findings: Vec<RiskFindingRecord>) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::SaveRiskFindings { findings, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Loads file versions from a session's matching snapshot.
    pub async fn load_snapshot_versions(
        &self,
        session_id: EntityId,
        snapshot_kind: &str,
    ) -> StoreResult<Vec<SnapshotFileVersion>> {
        let readers = self.inner.readers.clone();
        let snapshot_kind = snapshot_kind.to_owned();
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let mut statement = connection
                .prepare(
                    "SELECT fv.id, fv.file_id, fv.display_path_at_time, se.comparison_path,
                            fv.content_hash, fv.blob_id, fv.byte_length, fv.symlink_target,
                            fv.capture_status, COALESCE(fv.executable, 0)
                     FROM snapshots s
                     JOIN snapshot_entries se ON se.snapshot_id = s.id
                     JOIN file_versions fv ON fv.id = se.file_version_id
                     WHERE s.session_id = ?1 AND s.snapshot_kind = ?2
                     ORDER BY se.comparison_path ASC",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map(
                    rusqlite::params![session_id.as_uuid().as_bytes().as_slice(), snapshot_kind],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, Vec<u8>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, Option<i64>>(6)?,
                            row.get::<_, Option<String>>(7)?,
                            row.get::<_, String>(8)?,
                            row.get::<_, i64>(9)?,
                        ))
                    },
                )
                .map_err(StoreError::Sqlite)?;
            let mut versions = Vec::new();
            for row in rows {
                let (
                    file_version_id,
                    file_id,
                    display_path,
                    comparison_path,
                    content_hash,
                    blob_id,
                    byte_length,
                    symlink_target,
                    capture_status,
                    executable,
                ) = row.map_err(StoreError::Sqlite)?;
                versions.push(SnapshotFileVersion {
                    file_version_id: writer::uuid_from_blob(&file_version_id)?,
                    file_id: writer::uuid_from_blob(&file_id)?,
                    display_path,
                    comparison_path,
                    content_hash,
                    blob_id,
                    byte_length: byte_length
                        .map(|value| {
                            u64::try_from(value)
                                .map_err(|_| StoreError::InvalidStoredValue("negative file size"))
                        })
                        .transpose()?,
                    symlink_target,
                    capture_status,
                    executable: executable != 0,
                });
            }
            Ok(versions)
        })
        .await?
    }

    /// Persists an immutable recovery plan.
    pub async fn save_recovery_plan(&self, plan: RecoveryPlanRecord) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::SaveRecoveryPlan { plan, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Loads a persisted recovery plan.
    pub async fn get_recovery_plan(
        &self,
        plan_id: EntityId,
    ) -> StoreResult<Option<RecoveryPlanRecord>> {
        let readers = self.inner.readers.clone();
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let row = connection
                .query_row(
                    "SELECT id, session_id, action, destination, status, plan_digest,
                            plan_blob_id, created_at_us, executed_at_us, result_blob_id
                     FROM recovery_plans WHERE id = ?1",
                    [plan_id.as_uuid().as_bytes().as_slice()],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, Vec<u8>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, String>(6)?,
                            row.get::<_, i64>(7)?,
                            row.get::<_, Option<i64>>(8)?,
                            row.get::<_, Option<String>>(9)?,
                        ))
                    },
                )
                .optional()
                .map_err(StoreError::Sqlite)?;
            row.map(|row| {
                Ok(RecoveryPlanRecord {
                    id: writer::uuid_from_blob(&row.0)?,
                    session_id: writer::uuid_from_blob(&row.1)?,
                    action: row.2,
                    destination: row.3,
                    status: row.4,
                    plan_digest: row.5,
                    plan_blob_id: row.6,
                    created_at_us: row.7,
                    executed_at_us: row.8,
                    result_blob_id: row.9,
                })
            })
            .transpose()
        })
        .await?
    }

    /// Persists a recovery execution run.
    pub async fn save_recovery_run(&self, run: RecoveryRunRecord) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::SaveRecoveryRun { run, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Loads a persisted recovery run.
    pub async fn get_recovery_run(
        &self,
        run_id: EntityId,
    ) -> StoreResult<Option<RecoveryRunRecord>> {
        let readers = self.inner.readers.clone();
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let row = connection
                .query_row(
                    "SELECT id, plan_id, state, restored_files, skipped_files,
                            conflict_files, result_blob_id, created_at_us,
                            finished_at_us, error_code, backup_plan_id
                     FROM recovery_runs WHERE id = ?1",
                    [run_id.as_uuid().as_bytes().as_slice()],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, Vec<u8>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, Option<String>>(6)?,
                            row.get::<_, i64>(7)?,
                            row.get::<_, Option<i64>>(8)?,
                            row.get::<_, Option<String>>(9)?,
                            row.get::<_, Option<Vec<u8>>>(10)?,
                        ))
                    },
                )
                .optional()
                .map_err(StoreError::Sqlite)?;
            row.map(|row| {
                Ok(RecoveryRunRecord {
                    id: writer::uuid_from_blob(&row.0)?,
                    plan_id: writer::uuid_from_blob(&row.1)?,
                    state: row.2,
                    restored_files: u64::try_from(row.3)
                        .map_err(|_| StoreError::InvalidStoredValue("negative restored count"))?,
                    skipped_files: u64::try_from(row.4)
                        .map_err(|_| StoreError::InvalidStoredValue("negative skipped count"))?,
                    conflict_files: u64::try_from(row.5)
                        .map_err(|_| StoreError::InvalidStoredValue("negative conflict count"))?,
                    result_blob_id: row.6,
                    created_at_us: row.7,
                    finished_at_us: row.8,
                    error_code: row.9,
                    backup_plan_id: row.10.as_deref().map(writer::uuid_from_blob).transpose()?,
                })
            })
            .transpose()
        })
        .await?
    }

    /// Loads risk findings for one session.
    pub async fn load_risk_findings(
        &self,
        session_id: EntityId,
    ) -> StoreResult<Vec<RiskFindingView>> {
        let readers = self.inner.readers.clone();
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let mut statement = connection
                .prepare(
                    "SELECT id, event_id, rule_id, rule_version, severity, initial_status,
                            title, explanation, matched_preview, created_at_us
                     FROM risk_findings WHERE session_id = ?1
                     ORDER BY created_at_us DESC",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map([session_id.as_uuid().as_bytes().as_slice()], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, i64>(9)?,
                    ))
                })
                .map_err(StoreError::Sqlite)?;
            let mut findings = Vec::new();
            for row in rows {
                let row = row.map_err(StoreError::Sqlite)?;
                findings.push(RiskFindingView {
                    id: writer::uuid_from_blob(&row.0)?,
                    event_id: writer::uuid_from_blob(&row.1)?,
                    rule_id: row.2,
                    rule_version: row.3,
                    severity: row.4,
                    status: row.5,
                    title: row.6,
                    explanation: row.7,
                    matched_preview: row.8,
                    created_at_us: row.9,
                });
            }
            Ok(findings)
        })
        .await?
    }

    /// Loads events that may independently correlate with the supplied event.
    pub async fn correlation_candidates(
        &self,
        event: &EventEnvelope,
    ) -> StoreResult<Vec<EventEnvelope>> {
        let Some(project_id) = event.project_id else {
            return Ok(Vec::new());
        };
        let Some(target) = event
            .target
            .normalized_path
            .as_deref()
            .or(event.target.display.as_deref())
        else {
            return Ok(Vec::new());
        };
        let opposite_class = match event.evidence.class {
            agenttraceback_types::EvidenceClass::Reported => "observed",
            agenttraceback_types::EvidenceClass::Observed => "reported",
            agenttraceback_types::EvidenceClass::Verified => return Ok(Vec::new()),
        };
        let readers = self.inner.readers.clone();
        let target = target.to_owned();
        let event_id = event.id;
        let occurred_at_us = event.occurred_at_us;
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let mut statement = connection
                .prepare(
                    "SELECT envelope_json FROM events
                     WHERE project_id = ?1 AND target_comparison = ?2
                       AND source_evidence_class = ?3
                       AND occurred_at_us BETWEEN ?4 AND ?5
                       AND id != ?6
                     ORDER BY abs(occurred_at_us - ?7) ASC LIMIT 20",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map(
                    rusqlite::params![
                        project_id.as_uuid().as_bytes().as_slice(),
                        target,
                        opposite_class,
                        occurred_at_us.saturating_sub(10_000_000),
                        occurred_at_us.saturating_add(10_000_000),
                        event_id.as_uuid().as_bytes().as_slice(),
                        occurred_at_us,
                    ],
                    |row| row.get::<_, String>(0),
                )
                .map_err(StoreError::Sqlite)?;
            let mut events = Vec::new();
            for row in rows {
                let json = row.map_err(StoreError::Sqlite)?;
                events.push(serde_json::from_str(&json).map_err(StoreError::EnvelopeJson)?);
            }
            Ok(events)
        })
        .await?
    }

    /// Finds a recent wrapper session that likely owns a native agent session.
    pub async fn find_session_for_native_import(
        &self,
        project_id: EntityId,
        agent_name: &str,
        occurred_at_us: i64,
    ) -> StoreResult<Option<EntityId>> {
        let readers = self.inner.readers.clone();
        let agent_name = agent_name.to_owned();
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let row = connection
                .query_row(
                    "SELECT id FROM sessions
                     WHERE project_id = ?1
                       AND lower(COALESCE(agent_name, '')) = lower(?2)
                       AND started_at_us <= ?3
                       AND COALESCE(ended_at_us, ?3) >= ?4
                     ORDER BY started_at_us DESC LIMIT 1",
                    rusqlite::params![
                        project_id.as_uuid().as_bytes().as_slice(),
                        agent_name,
                        occurred_at_us.saturating_add(3_600_000_000),
                        occurred_at_us.saturating_sub(3_600_000_000),
                    ],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
                .map_err(StoreError::Sqlite)?;
            row.as_deref().map(writer::uuid_from_blob).transpose()
        })
        .await?
    }

    /// Persists a verified correlation or conflict with chain entries.
    pub async fn save_correlation(
        &self,
        correlation: Option<CorrelationInsert>,
        conflict: Option<CorrelationConflictInsert>,
    ) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::SaveCorrelation {
                correlation,
                conflict,
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Loads pre/post file versions for one session.
    pub async fn load_session_file_versions(
        &self,
        session_id: EntityId,
    ) -> StoreResult<Vec<SessionFileVersion>> {
        let readers = self.inner.readers.clone();
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let mut statement = connection
                .prepare(
                    "SELECT s.snapshot_kind, fv.id, fv.file_id, fv.display_path_at_time,
                            se.comparison_path, fv.content_hash, fv.blob_id, fv.byte_length,
                            fv.symlink_target, fv.capture_status, COALESCE(fv.executable, 0)
                     FROM snapshots s
                     JOIN snapshot_entries se ON se.snapshot_id = s.id
                     JOIN file_versions fv ON fv.id = se.file_version_id
                     WHERE s.session_id = ?1
                     ORDER BY se.comparison_path, s.snapshot_kind",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map([session_id.as_uuid().as_bytes().as_slice()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, i64>(10)?,
                    ))
                })
                .map_err(StoreError::Sqlite)?;
            let mut versions = Vec::new();
            for row in rows {
                let row = row.map_err(StoreError::Sqlite)?;
                versions.push(SessionFileVersion {
                    snapshot_kind: row.0,
                    version: SnapshotFileVersion {
                        file_version_id: writer::uuid_from_blob(&row.1)?,
                        file_id: writer::uuid_from_blob(&row.2)?,
                        display_path: row.3,
                        comparison_path: row.4,
                        content_hash: row.5,
                        blob_id: row.6,
                        byte_length: row
                            .7
                            .map(|value| {
                                u64::try_from(value).map_err(|_| {
                                    StoreError::InvalidStoredValue("negative file size")
                                })
                            })
                            .transpose()?,
                        symlink_target: row.8,
                        capture_status: row.9,
                        executable: row.10 != 0,
                    },
                });
            }
            Ok(versions)
        })
        .await?
    }

    /// Persists a Git boundary state.
    pub async fn save_git_state(&self, state: GitStateRecord) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::SaveGitState { state, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Loads a Git boundary state for a session phase.
    pub async fn get_git_state(
        &self,
        session_id: EntityId,
        phase: &str,
    ) -> StoreResult<Option<GitStateRecord>> {
        let readers = self.inner.readers.clone();
        let phase = phase.to_owned();
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let row = connection
                .query_row(
                    "SELECT id, session_id, snapshot_phase, repo_root, head_oid,
                            branch_name, upstream_name, status_blob_id, captured_at_us
                     FROM git_states
                     WHERE session_id = ?1 AND snapshot_phase = ?2
                     ORDER BY captured_at_us DESC LIMIT 1",
                    rusqlite::params![session_id.as_uuid().as_bytes().as_slice(), phase],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, Vec<u8>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, Option<String>>(6)?,
                            row.get::<_, Option<String>>(7)?,
                            row.get::<_, i64>(8)?,
                        ))
                    },
                )
                .optional()
                .map_err(StoreError::Sqlite)?;
            row.map(|row| {
                Ok(GitStateRecord {
                    id: writer::uuid_from_blob(&row.0)?,
                    session_id: writer::uuid_from_blob(&row.1)?,
                    snapshot_phase: row.2,
                    repo_root: row.3,
                    head_oid: row.4,
                    branch_name: row.5,
                    upstream_name: row.6,
                    status_blob_id: row.7,
                    captured_at_us: row.8,
                })
            })
            .transpose()
        })
        .await?
    }

    /// Loads one normalized event.
    pub async fn get_event(&self, id: EntityId) -> StoreResult<Option<EventEnvelope>> {
        let readers = self.inner.readers.clone();
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let envelope_json = connection
                .query_row(
                    "SELECT envelope_json FROM events WHERE id = ?1",
                    [id.as_uuid().as_bytes().as_slice()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(StoreError::Sqlite)?;
            envelope_json
                .map(|json| serde_json::from_str(&json).map_err(StoreError::EnvelopeJson))
                .transpose()
        })
        .await?
    }

    /// Counts persisted normalized events.
    pub async fn event_count(&self) -> StoreResult<u64> {
        let readers = self.inner.readers.clone();
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let count = connection
                .query_row("SELECT COUNT(*) FROM events", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(StoreError::Sqlite)?;
            u64::try_from(count).map_err(|_| StoreError::InvalidStoredValue("negative event count"))
        })
        .await?
    }

    /// Lists projects ordered by most recent activity.
    pub async fn list_projects(&self, limit: u16) -> StoreResult<Vec<ProjectSummaryRecord>> {
        let readers = self.inner.readers.clone();
        let limit = i64::from(limit.clamp(1, 500));
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let mut statement = connection
                .prepare(
                    "SELECT p.id, p.display_name, p.canonical_root, p.vcs_kind,
                            p.last_seen_at_us,
                            (SELECT COUNT(*) FROM sessions s WHERE s.project_id = p.id),
                            (SELECT COUNT(*) FROM events e
                             WHERE e.project_id = p.id OR EXISTS (
                                 SELECT 1 FROM event_session_links l
                                 JOIN sessions s ON s.id = l.session_id
                                 WHERE l.event_id = e.id AND s.project_id = p.id
                             ))
                     FROM projects p
                     ORDER BY p.last_seen_at_us DESC, lower(hex(p.id)) ASC LIMIT ?1",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map([limit], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                })
                .map_err(StoreError::Sqlite)?;
            let mut projects = Vec::new();
            for row in rows {
                let row = row.map_err(StoreError::Sqlite)?;
                projects.push(ProjectSummaryRecord {
                    id: writer::uuid_from_blob(&row.0)?,
                    display_name: row.1,
                    canonical_root: row.2,
                    vcs_kind: row.3,
                    last_seen_at_us: row.4,
                    session_count: u64::try_from(row.5)
                        .map_err(|_| StoreError::InvalidStoredValue("session count"))?,
                    event_count: u64::try_from(row.6)
                        .map_err(|_| StoreError::InvalidStoredValue("event count"))?,
                });
            }
            Ok(projects)
        })
        .await?
    }

    /// Lists sessions ordered by most recent start time.
    pub async fn list_sessions(&self, limit: u16) -> StoreResult<Vec<SessionSummaryRecord>> {
        let readers = self.inner.readers.clone();
        let limit = i64::from(limit.clamp(1, 500));
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let mut statement = connection
                .prepare(&session_summary_sql(
                    "ORDER BY s.started_at_us DESC, lower(hex(s.id)) ASC LIMIT ?1",
                ))
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map([limit], session_summary_from_row)
                .map_err(StoreError::Sqlite)?;
            let mut sessions = Vec::new();
            for row in rows {
                sessions.push(session_summary_from_values(
                    row.map_err(StoreError::Sqlite)?,
                )?);
            }
            Ok(sessions)
        })
        .await?
    }

    /// Loads one session summary.
    pub async fn get_session_summary(
        &self,
        session_id: EntityId,
    ) -> StoreResult<Option<SessionSummaryRecord>> {
        let readers = self.inner.readers.clone();
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let row = connection
                .query_row(
                    &session_summary_sql("WHERE s.id = ?1"),
                    [session_id.as_uuid().as_bytes().as_slice()],
                    session_summary_from_row,
                )
                .optional()
                .map_err(StoreError::Sqlite)?;
            row.map(session_summary_from_values).transpose()
        })
        .await?
    }

    /// Loads one session timeline in chronological order.
    pub async fn load_session_events(
        &self,
        session_id: EntityId,
        limit: u16,
    ) -> StoreResult<Vec<EventEnvelope>> {
        self.load_session_events_page(session_id, limit, 0).await
    }

    /// Loads one offset page of a session timeline in chronological order.
    pub async fn load_session_events_page(
        &self,
        session_id: EntityId,
        limit: u16,
        offset: u64,
    ) -> StoreResult<Vec<EventEnvelope>> {
        let readers = self.inner.readers.clone();
        let limit = i64::from(limit.clamp(1, 500));
        let offset = i64::try_from(offset)
            .map_err(|_| StoreError::InvalidStoredValue("event page offset"))?;
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let mut statement = connection
                .prepare(
                    "SELECT e.envelope_json,
                            EXISTS(SELECT 1 FROM correlations c
                                   WHERE c.observed_event_id = e.id)
                     FROM event_session_links l
                     JOIN events e ON e.id = l.event_id
                     WHERE l.session_id = ?1
                     ORDER BY e.occurred_at_us ASC, lower(hex(e.id)) ASC
                     LIMIT ?2 OFFSET ?3",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map(
                    rusqlite::params![session_id.as_uuid().as_bytes().as_slice(), limit, offset],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0)),
                )
                .map_err(StoreError::Sqlite)?;
            let mut events = Vec::new();
            for row in rows {
                let (json, verified) = row.map_err(StoreError::Sqlite)?;
                let mut event: EventEnvelope =
                    serde_json::from_str(&json).map_err(StoreError::EnvelopeJson)?;
                if verified {
                    event.evidence.class = agenttraceback_types::EvidenceClass::Verified;
                    event.evidence.correlation_version = Some(1);
                }
                events.push(event);
            }
            Ok(events)
        })
        .await?
    }

    /// Loads recent events across all projects for the live dashboard.
    pub async fn load_recent_events(&self, limit: u16) -> StoreResult<Vec<EventEnvelope>> {
        let readers = self.inner.readers.clone();
        let limit = i64::from(limit.clamp(1, 500));
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let mut statement = connection
                .prepare(
                    "SELECT envelope_json,
                            EXISTS(SELECT 1 FROM correlations c
                                   WHERE c.observed_event_id = events.id)
                     FROM events
                     ORDER BY occurred_at_us DESC, lower(hex(id)) ASC LIMIT ?1",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map([limit], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0))
                })
                .map_err(StoreError::Sqlite)?;
            let mut events = Vec::new();
            for row in rows {
                let (json, verified) = row.map_err(StoreError::Sqlite)?;
                let mut event: EventEnvelope =
                    serde_json::from_str(&json).map_err(StoreError::EnvelopeJson)?;
                if verified {
                    event.evidence.class = agenttraceback_types::EvidenceClass::Verified;
                    event.evidence.correlation_version = Some(1);
                }
                events.push(event);
            }
            Ok(events)
        })
        .await?
    }

    /// Lists findings across all sessions ordered by severity signal and time.
    pub async fn list_findings(&self, limit: u16) -> StoreResult<Vec<FindingSummaryRecord>> {
        let readers = self.inner.readers.clone();
        let limit = i64::from(limit.clamp(1, 500));
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let mut statement = connection
                .prepare(
                    "SELECT rf.id, rf.session_id, rf.event_id, p.display_name, s.agent_name,
                            rf.rule_id, rf.rule_version, rf.severity, rf.initial_status,
                            rf.title, rf.explanation, rf.matched_preview, rf.created_at_us
                     FROM risk_findings rf
                     LEFT JOIN sessions s ON s.id = rf.session_id
                     LEFT JOIN projects p ON p.id = s.project_id
                     ORDER BY CASE rf.severity
                                WHEN 'critical' THEN 0 WHEN 'high' THEN 1
                                WHEN 'medium' THEN 2 WHEN 'low' THEN 3
                                WHEN 'info' THEN 4 ELSE 5 END,
                              rf.created_at_us DESC, lower(hex(rf.id)) ASC
                     LIMIT ?1",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map([limit], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, i64>(12)?,
                    ))
                })
                .map_err(StoreError::Sqlite)?;
            let mut findings = Vec::new();
            for row in rows {
                let row = row.map_err(StoreError::Sqlite)?;
                findings.push(FindingSummaryRecord {
                    id: writer::uuid_from_blob(&row.0)?,
                    session_id: row.1.as_deref().map(writer::uuid_from_blob).transpose()?,
                    event_id: writer::uuid_from_blob(&row.2)?,
                    project_name: row.3,
                    agent_name: row.4,
                    rule_id: row.5,
                    rule_version: row.6,
                    severity: row.7,
                    status: row.8,
                    title: row.9,
                    explanation: row.10,
                    matched_preview: row.11,
                    created_at_us: row.12,
                });
            }
            Ok(findings)
        })
        .await?
    }

    /// Lists files ordered by most recent session activity.
    pub async fn list_files(&self, limit: u16) -> StoreResult<Vec<FileSummaryRecord>> {
        let readers = self.inner.readers.clone();
        let limit = i64::from(limit.clamp(1, 500));
        tokio::task::spawn_blocking(move || {
            let connection = readers.connection()?;
            let connection = lock_reader(&connection)?;
            let mut statement = connection
                .prepare(
                    "SELECT f.id, p.display_name, f.current_display_path, f.sensitive_class,
                            f.first_seen_at_us, f.last_seen_at_us,
                            COUNT(DISTINCT fv.id), COUNT(DISTINCT fv.session_id),
                            SUM(CASE WHEN fv.blob_id IS NOT NULL
                                      AND fv.capture_status = 'captured' THEN 1 ELSE 0 END)
                     FROM files f
                     JOIN projects p ON p.id = f.project_id
                     LEFT JOIN file_versions fv ON fv.file_id = f.id
                     GROUP BY f.id
                     ORDER BY f.last_seen_at_us DESC, lower(hex(f.id)) ASC LIMIT ?1",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map([limit], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                    ))
                })
                .map_err(StoreError::Sqlite)?;
            let mut files = Vec::new();
            for row in rows {
                let row = row.map_err(StoreError::Sqlite)?;
                files.push(FileSummaryRecord {
                    id: writer::uuid_from_blob(&row.0)?,
                    project_name: row.1,
                    path: row.2,
                    sensitive_class: row.3,
                    first_seen_at_us: row.4,
                    last_seen_at_us: row.5,
                    version_count: u64::try_from(row.6)
                        .map_err(|_| StoreError::InvalidStoredValue("version count"))?,
                    session_count: u64::try_from(row.7)
                        .map_err(|_| StoreError::InvalidStoredValue("session count"))?,
                    recoverable_count: u64::try_from(row.8.unwrap_or(0))
                        .map_err(|_| StoreError::InvalidStoredValue("recoverable count"))?,
                });
            }
            Ok(files)
        })
        .await?
    }

    /// Deletes all demo project history while preserving global chain integrity records
    /// outside the removed project scope.
    pub async fn delete_project_data(&self, project_id: EntityId) -> StoreResult<u64> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::DeleteProjectData { project_id, reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)?
    }

    /// Verifies a session chain or the daemon-global chain.
    pub async fn verify_chain(
        &self,
        session_id: Option<EntityId>,
    ) -> StoreResult<ChainVerificationReport> {
        let readers = self.inner.readers.clone();
        let chain_scope = session_id.map_or_else(|| "global".to_owned(), |id| id.to_string());
        tokio::task::spawn_blocking(move || verify_chain_sync(&readers, &chain_scope)).await?
    }

    /// Stops the writer actor after queued commands complete.
    pub async fn close(self) -> StoreResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .writes
            .send(WriteCommand::Close { reply })
            .await
            .map_err(|_| StoreError::WriterUnavailable)?;
        receiver.await.map_err(|_| StoreError::WriterUnavailable)
    }
}

type SessionSummaryRow = (
    Vec<u8>,
    Option<Vec<u8>>,
    Option<String>,
    Option<String>,
    String,
    i64,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
);

fn session_summary_sql(suffix: &str) -> String {
    format!(
        "SELECT s.id, s.project_id, p.display_name, s.title_preview, s.state,
                s.started_at_us, s.ended_at_us, s.agent_name, s.harness_name,
                s.provider_name, s.model_name, s.outcome, s.outcome_confidence,
                s.recovery_coverage, s.capture_health, s.risk_max_severity,
                (SELECT COUNT(*) FROM event_session_links l WHERE l.session_id = s.id),
                (SELECT COUNT(DISTINCT fv.file_id) FROM file_versions fv
                 WHERE fv.session_id = s.id),
                (SELECT COUNT(*) FROM risk_findings rf WHERE rf.session_id = s.id)
         FROM sessions s
         LEFT JOIN projects p ON p.id = s.project_id
         {suffix}"
    )
}

fn session_summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSummaryRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
    ))
}

fn session_summary_from_values(row: SessionSummaryRow) -> StoreResult<SessionSummaryRecord> {
    Ok(SessionSummaryRecord {
        id: writer::uuid_from_blob(&row.0)?,
        project_id: row.1.as_deref().map(writer::uuid_from_blob).transpose()?,
        project_name: row.2,
        title_preview: row.3,
        state: row.4,
        started_at_us: row.5,
        ended_at_us: row.6,
        agent_name: row.7,
        harness_name: row.8,
        provider_name: row.9,
        model_name: row.10,
        outcome: row.11,
        outcome_confidence: row.12,
        recovery_coverage: row.13,
        capture_health: row.14,
        risk_max_severity: row.15,
        event_count: u64::try_from(row.16)
            .map_err(|_| StoreError::InvalidStoredValue("event count"))?,
        file_count: u64::try_from(row.17)
            .map_err(|_| StoreError::InvalidStoredValue("file count"))?,
        finding_count: u64::try_from(row.18)
            .map_err(|_| StoreError::InvalidStoredValue("finding count"))?,
    })
}

#[derive(Clone)]
struct ReaderPool {
    connections: Arc<Vec<Arc<Mutex<Connection>>>>,
    next: Arc<AtomicUsize>,
    path: Arc<PathBuf>,
}

impl ReaderPool {
    fn open(path: &Path) -> StoreResult<Self> {
        let mut connections = Vec::with_capacity(4);
        for _ in 0..4 {
            connections.push(Arc::new(Mutex::new(migration::open_reader(path)?)));
        }
        Ok(Self {
            connections: Arc::new(connections),
            next: Arc::new(AtomicUsize::new(0)),
            path: Arc::new(path.to_path_buf()),
        })
    }

    fn connection(&self) -> StoreResult<Arc<Mutex<Connection>>> {
        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.connections.len();
        Ok(Arc::clone(&self.connections[index]))
    }

    fn path(&self) -> &Path {
        self.path.as_path()
    }
}

fn lock_reader(connection: &Arc<Mutex<Connection>>) -> StoreResult<MutexGuard<'_, Connection>> {
    connection.lock().map_err(|_| StoreError::ReaderPoisoned)
}

fn verify_chain_sync(
    readers: &ReaderPool,
    chain_scope: &str,
) -> StoreResult<ChainVerificationReport> {
    let connection = readers.connection()?;
    let connection = lock_reader(&connection)?;
    let mut statement = connection
        .prepare(
            "SELECT sequence, entry_kind, target_id, canonical_digest, raw_payload_digest,
                    previous_hash, entry_hash
             FROM chain_entries
             WHERE chain_scope = ?1
             ORDER BY sequence ASC",
        )
        .map_err(StoreError::Sqlite)?;
    let rows = statement
        .query_map([chain_scope], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, Option<Vec<u8>>>(4)?,
                row.get::<_, Vec<u8>>(5)?,
                row.get::<_, Vec<u8>>(6)?,
            ))
        })
        .map_err(StoreError::Sqlite)?;

    let mut entries = Vec::new();
    for row in rows {
        let (sequence, entry_kind, target_id, canonical, raw, previous, entry_hash) =
            row.map_err(StoreError::Sqlite)?;
        entries.push(ChainEntry {
            sequence: u64::try_from(sequence)
                .map_err(|_| StoreError::InvalidStoredValue("chain sequence"))?,
            entry_kind,
            target_id,
            canonical_digest: ChainHash(hash_array(&canonical)?),
            raw_payload_digest: raw.as_deref().map(hash_array).transpose()?.map(ChainHash),
            previous_hash: ChainHash(hash_array(&previous)?),
            entry_hash: ChainHash(hash_array(&entry_hash)?),
        });
    }
    let mut report = ChainVerifier::verify(&entries);
    for entry in &entries {
        if entry.entry_kind != "source_event" {
            continue;
        }
        let event_id = EntityId::from_str(&entry.target_id)
            .map_err(|_| StoreError::InvalidStoredValue("chain event UUID"))?;
        let envelope_json = connection
            .query_row(
                "SELECT envelope_json FROM events WHERE id = ?1",
                [event_id.as_uuid().as_bytes().as_slice()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Sqlite)?;
        let Some(envelope_json) = envelope_json else {
            let deleted = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM deleted_chain_targets WHERE target_id = ?1)",
                    [&entry.target_id],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(StoreError::Sqlite)?;
            if !deleted {
                report.missing_sequences.push(entry.sequence);
            }
            continue;
        };
        let mut envelope: EventEnvelope =
            serde_json::from_str(&envelope_json).map_err(StoreError::EnvelopeJson)?;
        envelope.sequence = None;
        envelope.integrity = None;
        let digest = canonical_digest(&envelope)?;
        if digest != entry.canonical_digest {
            report.changed_entries.push(entry.sequence);
        }
        if let Some(raw_digest) = entry.raw_payload_digest {
            let blob_id = raw_digest.to_hex();
            let active = connection
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM blobs WHERE id = ?1 AND state = 'active'
                     )",
                    [blob_id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(StoreError::Sqlite)?;
            if active == 0 {
                report.missing_payloads.push(entry.sequence);
            }
        }
    }
    let session_id = if chain_scope == "global" {
        None
    } else {
        Some(
            EntityId::from_str(chain_scope)
                .map_err(|_| StoreError::InvalidStoredValue("chain scope UUID"))?,
        )
    };
    let root = connection
        .query_row(
            "SELECT entry_count, root_hash FROM chain_roots
             WHERE (?1 IS NULL AND session_id IS NULL) OR session_id = ?1
             ORDER BY finalized_at_us DESC LIMIT 1",
            [session_id.map(|id| *id.as_uuid().as_bytes())],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .map_err(StoreError::Sqlite)?;
    if let Some((entry_count, root_hash)) = root {
        let expected_count = entries.last().map_or(0, |entry| entry.sequence);
        let expected_hash = entries.last().map(|entry| entry.entry_hash.0);
        if u64::try_from(entry_count).ok() != Some(expected_count)
            || expected_hash != Some(hash_array(&root_hash)?)
        {
            report.root_mismatch = true;
        }
    }
    report.missing_sequences.sort_unstable();
    report.missing_sequences.dedup();
    report.reordered_sequences.sort_unstable();
    report.reordered_sequences.dedup();
    report.changed_entries.sort_unstable();
    report.changed_entries.dedup();
    report.missing_payloads.sort_unstable();
    report.missing_payloads.dedup();
    Ok(report)
}

fn search_sync(readers: &ReaderPool, query: &SearchQuery, limit: u16) -> StoreResult<SearchPage> {
    let connection = readers.connection()?;
    let connection = lock_reader(&connection)?;
    let has_text = query.terms.iter().any(|term| !term.negated);
    const EVIDENCE_EXPRESSION: &str = "CASE WHEN EXISTS(
        SELECT 1 FROM correlations c
        WHERE c.logical_event_id = e.id OR c.reported_event_id = e.id OR c.observed_event_id = e.id
    ) THEN 'verified' ELSE e.source_evidence_class END";
    let mut parameters = Vec::<Value>::new();
    let mut sql = if has_text {
        format!(
            "SELECT e.id, l.session_id, e.occurred_at_us, e.action, e.actor_agent,
                e.actor_model, p.display_name, e.target_display, e.redacted_preview,
                {EVIDENCE_EXPRESSION}, e.result_status, e.risk_max_severity,
                bm25(events_fts)
         FROM events_fts
         JOIN events e ON e.rowid = events_fts.rowid
         LEFT JOIN (
             SELECT event_id, MIN(session_id) AS session_id
             FROM event_session_links GROUP BY event_id
         ) l ON l.event_id = e.id
         LEFT JOIN projects p ON p.id = COALESCE(e.project_id, (
             SELECT project_id FROM sessions WHERE id = l.session_id
         ))"
        )
    } else {
        format!(
            "SELECT e.id, l.session_id, e.occurred_at_us, e.action, e.actor_agent,
                e.actor_model, p.display_name, e.target_display, e.redacted_preview,
                {EVIDENCE_EXPRESSION}, e.result_status, e.risk_max_severity,
                CAST(0.0 AS REAL)
         FROM events e
         LEFT JOIN (
             SELECT event_id, MIN(session_id) AS session_id
             FROM event_session_links GROUP BY event_id
         ) l ON l.event_id = e.id
         LEFT JOIN projects p ON p.id = COALESCE(e.project_id, (
             SELECT project_id FROM sessions WHERE id = l.session_id
         ))"
        )
    };
    sql.push_str(" WHERE 1 = 1");
    sql.push_str(
        " AND NOT EXISTS (
            SELECT 1 FROM correlations correlated
            WHERE correlated.observed_event_id = e.id
        )",
    );
    if has_text {
        sql.push_str(" AND events_fts MATCH ?");
        parameters.push(Value::Text(query.fts_match_expression()));
    }
    for term in query.negative_terms() {
        sql.push_str(
            " AND NOT EXISTS (SELECT 1 FROM events_fts
                              WHERE events_fts.rowid = e.rowid
                                AND events_fts MATCH ?)",
        );
        let escaped = term.text.replace('"', "\"\"");
        parameters.push(Value::Text(format!("\"{escaped}\"")));
    }
    add_filter(
        &mut sql,
        &mut parameters,
        "e.actor_agent",
        &query.filters.agents,
        FilterMode::Equals,
    );
    add_filter(
        &mut sql,
        &mut parameters,
        "e.actor_model",
        &query.filters.models,
        FilterMode::Equals,
    );
    add_filter(
        &mut sql,
        &mut parameters,
        "p.display_name",
        &query.filters.projects,
        FilterMode::Contains,
    );
    add_filter(
        &mut sql,
        &mut parameters,
        "e.action",
        &query.filters.actions,
        FilterMode::Equals,
    );
    add_filter(
        &mut sql,
        &mut parameters,
        "COALESCE(e.target_comparison, e.target_display)",
        &query.filters.paths,
        FilterMode::Contains,
    );
    add_filter(
        &mut sql,
        &mut parameters,
        "e.risk_max_severity",
        &query.filters.risks,
        FilterMode::Equals,
    );
    add_filter(
        &mut sql,
        &mut parameters,
        EVIDENCE_EXPRESSION,
        &query.filters.evidence,
        FilterMode::Equals,
    );
    add_filter(
        &mut sql,
        &mut parameters,
        "e.result_status",
        &query.filters.statuses,
        FilterMode::Equals,
    );
    add_filter(
        &mut sql,
        &mut parameters,
        "lower(hex(l.session_id))",
        &query.filters.sessions,
        FilterMode::Prefix,
    );
    if let Some(after_us) = query.filters.after_us {
        sql.push_str(" AND e.occurred_at_us >= ?");
        parameters.push(Value::Integer(after_us));
    }
    if let Some(before_us) = query.filters.before_us {
        sql.push_str(" AND e.occurred_at_us <= ?");
        parameters.push(Value::Integer(before_us));
    }
    sql.push_str(" ORDER BY 13 ASC, e.occurred_at_us DESC, lower(hex(e.id)) ASC LIMIT ?");
    parameters.push(Value::Integer(i64::from(limit) + 1));

    let mut statement = connection.prepare(&sql).map_err(StoreError::Sqlite)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(parameters), |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Option<Vec<u8>>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, f64>(12)?,
            ))
        })
        .map_err(StoreError::Sqlite)?;

    let mut items = Vec::new();
    for row in rows {
        let (
            event_id,
            session_id,
            occurred_at_us,
            action,
            agent,
            model,
            project_name,
            target_display,
            redacted_preview,
            evidence,
            status,
            risk,
            rank,
        ) = row.map_err(StoreError::Sqlite)?;
        items.push(SearchHit {
            event_id: writer::uuid_from_blob(&event_id)?,
            session_id: session_id
                .as_deref()
                .map(writer::uuid_from_blob)
                .transpose()?,
            occurred_at_us,
            action,
            agent,
            model,
            project_name,
            target_display,
            redacted_preview,
            evidence,
            status,
            risk,
            rank: has_text.then_some(rank),
        });
    }
    let has_more = items.len() > usize::from(limit);
    items.truncate(usize::from(limit));
    Ok(SearchPage { items, has_more })
}

enum FilterMode {
    Equals,
    Contains,
    Prefix,
}

fn add_filter(
    sql: &mut String,
    parameters: &mut Vec<Value>,
    expression: &str,
    filters: &[FilterValue],
    mode: FilterMode,
) {
    for filter in filters {
        let operator = match mode {
            FilterMode::Equals => "=",
            FilterMode::Contains | FilterMode::Prefix => "LIKE",
        };
        let clause = format!("{expression} {operator} ?");
        if filter.negated {
            sql.push_str(" AND (");
            sql.push_str(expression);
            sql.push_str(" IS NULL OR NOT (");
            sql.push_str(&clause);
            sql.push_str("))");
        } else {
            sql.push_str(" AND (");
            sql.push_str(&clause);
            sql.push(')');
        }
        let value = match mode {
            FilterMode::Equals => filter.value.clone(),
            FilterMode::Contains => format!("%{}%", filter.value),
            FilterMode::Prefix => format!("{}%", filter.value.to_lowercase()),
        };
        parameters.push(Value::Text(value));
    }
}

#[cfg(test)]
mod tests {
    use agenttraceback_search::SearchQuery;
    use agenttraceback_types::{
        AttributionConfidence, EntityId, EventAction, EventEnvelope, EventSource, SourceKind,
    };

    use super::{Store, StoreError};

    fn test_event(source_event_id: &str) -> EventEnvelope {
        EventEnvelope::new(
            EventSource {
                kind: SourceKind::AgentLog,
                original_source_kind: None,
                adapter_id: Some("codex".to_owned()),
                source_event_id: source_event_id.to_owned(),
                raw_blob_id: None,
            },
            EventAction::FileWrite,
            1_799_000_000_000_000,
            1_799_000_000_000_001,
        )
    }

    async fn test_store() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = Store::open(
            directory.path().join("agenttraceback.db"),
            directory.path().join("backups"),
        )
        .await
        .expect("store");
        (directory, store)
    }

    #[tokio::test]
    async fn append_retrieve_and_verify() {
        let (_directory, store) = test_store().await;
        let mut event = test_event("event-1");
        let session_id = EntityId::new();
        event.session_id = Some(session_id);
        event.evidence.attribution = AttributionConfidence::Exact;
        insert_test_session(store.database_path(), session_id);
        let appended = store
            .append_events(vec![event.clone()])
            .await
            .expect("append");
        assert_eq!(appended.len(), 1);
        assert_eq!(appended[0].sequence, Some(1));
        assert!(appended[0].integrity.is_some());
        assert_eq!(store.event_count().await.expect("count"), 1);
        let loaded = store
            .get_event(event.id)
            .await
            .expect("get")
            .expect("event");
        assert_eq!(loaded.id, event.id);
        assert_eq!(loaded.action, EventAction::FileWrite);
        assert!(
            store
                .verify_chain(Some(session_id))
                .await
                .expect("verify")
                .is_valid()
        );
        store
            .finalize_chain(Some(session_id))
            .await
            .expect("finalize");
        assert!(
            store
                .verify_chain(Some(session_id))
                .await
                .expect("verify finalized")
                .is_valid()
        );
        let mut after_finalize = test_event("event-after-finalize");
        after_finalize.session_id = Some(session_id);
        let append_after_finalize = store.append_events(vec![after_finalize]).await;
        assert!(matches!(
            append_after_finalize,
            Err(StoreError::ChainFinalized)
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn database_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, store) = test_store().await;
        let mode = std::fs::metadata(store.database_path())
            .expect("database metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn deleting_finalized_project_preserves_global_chain() {
        let (_directory, store) = test_store().await;
        let project_id = EntityId::new();
        let session_id = EntityId::new();
        insert_test_session(store.database_path(), session_id);
        let connection = rusqlite::Connection::open(store.database_path()).expect("connection");
        connection.execute(
            "INSERT INTO projects(id, display_name, canonical_root, comparison_root, vcs_kind, created_at_us, last_seen_at_us)
             VALUES (?1, 'project', '/test-project', '/test-project', 'none', 1, 1)",
            [project_id.as_uuid().as_bytes().as_slice()],
        ).expect("project");
        connection
            .execute(
                "UPDATE sessions SET project_id = ?1 WHERE id = ?2",
                rusqlite::params![
                    project_id.as_uuid().as_bytes().as_slice(),
                    session_id.as_uuid().as_bytes().as_slice()
                ],
            )
            .expect("attach session");
        let mut session_event = test_event("session");
        session_event.project_id = Some(project_id);
        session_event.session_id = Some(session_id);
        let mut global_event = test_event("global-project");
        global_event.project_id = Some(project_id);
        store
            .append_events(vec![
                test_event("global-before"),
                session_event,
                global_event,
            ])
            .await
            .expect("append");
        store
            .finalize_chain(Some(session_id))
            .await
            .expect("finalize");
        assert_eq!(
            store.delete_project_data(project_id).await.expect("delete"),
            2
        );
        store
            .append_events(vec![test_event("global-after")])
            .await
            .expect("global remains writable");
        assert!(
            store
                .verify_chain(None)
                .await
                .expect("verify global")
                .is_valid()
        );
        assert_eq!(store.event_count().await.expect("count"), 2);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM chain_roots", [], |row| row
                    .get::<_, i64>(0))
                .expect("roots"),
            0
        );
        store.close().await.expect("close");
    }

    #[tokio::test]
    async fn migrates_version_one_database_with_backup() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let database = directory.path().join("agenttraceback.db");
        let connection = rusqlite::Connection::open(&database).expect("database");
        connection
            .execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../migrations/0001_initial.sql"
            )))
            .expect("version one schema");
        connection
            .execute(
                "INSERT INTO schema_migrations(version, name, applied_at_us)
                 VALUES (1, '0001_initial', 1)",
                [],
            )
            .expect("migration row");
        connection
            .pragma_update(None, "user_version", 1)
            .expect("schema version");
        drop(connection);

        let store = Store::open(&database, directory.path().join("backups"))
            .await
            .expect("migrated store");
        let connection = rusqlite::Connection::open(store.database_path()).expect("database");
        let version = connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version");
        assert_eq!(version, 5);
        let table_count = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'recovery_plans'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("recovery table");
        assert_eq!(table_count, 1);
        assert!(
            std::fs::read_dir(directory.path().join("backups"))
                .expect("backups")
                .next()
                .is_some()
        );
        store.close().await.expect("close store");
    }

    fn insert_test_session(path: &std::path::Path, session_id: EntityId) {
        let connection = rusqlite::Connection::open(path).expect("session connection");
        connection
            .execute(
                "INSERT INTO sessions (
                    id, project_id, title_preview, state, started_at_us, ended_at_us,
                    agent_name, harness_name, provider_name, model_name, source_session_id,
                    outcome, outcome_confidence, recovery_coverage, capture_health,
                    risk_max_severity, chain_root_id
                 ) VALUES (?1, NULL, 'test', 'active', ?2, NULL, 'test-agent', NULL, NULL,
                           'test-model', ?3, 'unknown', 'unknown', 'unavailable', 'complete',
                           'none', NULL)",
                rusqlite::params![
                    session_id.as_uuid().as_bytes().as_slice(),
                    1_799_000_000_000_000_i64,
                    session_id.to_string(),
                ],
            )
            .expect("insert session");
    }

    #[tokio::test]
    async fn repeated_source_event_is_idempotent() {
        let (_directory, store) = test_store().await;
        let event = test_event("event-1");
        let first = store
            .append_events(vec![event.clone()])
            .await
            .expect("first");
        let second = store.append_events(vec![event]).await.expect("second");
        assert_eq!(first[0].id, second[0].id);
        assert_eq!(store.event_count().await.expect("count"), 1);
    }

    #[tokio::test]
    async fn changing_a_chain_row_is_detected() {
        let (directory, store) = test_store().await;
        let event = test_event("event-1");
        let appended = store.append_events(vec![event]).await.expect("append");
        let event_id = appended[0].id;
        let database_path = store.database_path().to_path_buf();
        let connection = rusqlite::Connection::open(database_path).expect("tampering connection");
        connection
            .execute_batch(
                "DROP TRIGGER events_no_update;
                 UPDATE events SET redacted_preview = 'tampered' WHERE id = X'00000000000000000000000000000000';",
            )
            .expect("drop trigger");
        connection
            .execute(
                "UPDATE events SET envelope_json = replace(envelope_json, '\"redactedPreview\":null', '\"redactedPreview\":\"tampered\"') WHERE id = ?1",
                [event_id.as_uuid().as_bytes().as_slice()],
            )
            .expect("tamper");
        let report = store.verify_chain(None).await.expect("verify after tamper");
        assert!(!report.is_valid());
        drop(directory);
    }

    #[tokio::test]
    async fn search_uses_redacted_fts_fields_and_structured_filters() {
        let (_directory, store) = test_store().await;
        let mut event = test_event("event-search-1");
        event.actor.agent = Some("codex".to_owned());
        event.target.display = Some("src/auth.ts".to_owned());
        event.target.normalized_path = Some("src/auth.ts".to_owned());
        event.content.redacted_preview = Some("oauth migration updated".to_owned());
        store.append_events(vec![event]).await.expect("append");

        let query = SearchQuery::parse("oauth agent:codex path:src/auth").expect("query");
        let page = store.search(query, 50).await.expect("search");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].agent.as_deref(), Some("codex"));
        assert!(
            page.items[0]
                .redacted_preview
                .as_deref()
                .is_some_and(|preview| preview.contains("oauth"))
        );
        assert!(!page.has_more);
    }

    #[tokio::test]
    async fn negative_terms_and_filters_exclude_only_matches() {
        let (_directory, store) = test_store().await;
        let mut excluded = test_event("event-negated-1");
        excluded.actor.agent = Some("codex".to_owned());
        excluded.content.redacted_preview = Some("draft token".to_owned());
        let mut included = test_event("event-negated-2");
        included.content.redacted_preview = Some("final token".to_owned());
        store
            .append_events(vec![excluded, included])
            .await
            .expect("append");

        let term_query = SearchQuery::parse("-draft").expect("term query");
        let page = store.search(term_query, 50).await.expect("term search");
        assert_eq!(page.items.len(), 1);
        assert_eq!(
            page.items[0].redacted_preview.as_deref(),
            Some("final token")
        );

        let filter_query = SearchQuery::parse("-agent:codex").expect("filter query");
        let page = store.search(filter_query, 50).await.expect("filter search");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].agent, None);
    }

    #[tokio::test]
    async fn missing_encrypted_payload_is_reported_separately() {
        let (_directory, store) = test_store().await;
        let mut event = test_event("event-missing-payload");
        event.source.raw_blob_id = Some("11".repeat(32));
        store.append_events(vec![event]).await.expect("append");
        let report = store.verify_chain(None).await.expect("verify");
        assert_eq!(report.missing_payloads, vec![1]);
        assert!(!report.is_valid());
    }

    #[ignore = "million-event acceptance and benchmark; run explicitly"]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stores_one_million_events_and_searches_under_budget() {
        use std::time::{Duration, Instant};

        const BATCH_SIZE: usize = 1_000;
        let event_count = std::env::var("AGENTTRACEBACK_BENCH_EVENTS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1_000_000);
        let (_directory, store) = test_store().await;
        let insert_started = Instant::now();
        for batch_start in (0..event_count).step_by(BATCH_SIZE) {
            let mut batch = Vec::with_capacity(BATCH_SIZE);
            for offset in 0..BATCH_SIZE.min(event_count - batch_start) {
                let index = batch_start + offset;
                let mut event = test_event(&format!("bulk-event-{index}"));
                event.actor.agent = Some("codex".to_owned());
                event.target.display = Some(format!("src/generated/file-{index}.rs"));
                event.target.normalized_path = Some(format!("src/generated/file-{index}.rs"));
                event.content.redacted_preview = Some(if index % 1_000 == 0 {
                    "needle acceptance marker".to_owned()
                } else {
                    format!("routine fixture event {index}")
                });
                batch.push(event);
            }
            store.append_events(batch).await.expect("bulk append");
        }
        let insert_duration = insert_started.elapsed();
        assert_eq!(
            store.event_count().await.expect("count"),
            event_count as u64
        );

        let query = SearchQuery::parse("needle").expect("query");
        let expected_matches = event_count.div_ceil(1_000).min(50);
        let mut durations = Vec::with_capacity(10);
        for _ in 0..10 {
            let started = Instant::now();
            let page = store.search(query.clone(), 50).await.expect("search");
            durations.push(started.elapsed());
            assert_eq!(page.items.len(), expected_matches);
        }
        durations.sort_unstable();
        let p95 = durations[9];
        eprintln!(
            "indexed {event_count} events in {:.2}s ({:.0} events/s); search p95 {:.2}ms",
            insert_duration.as_secs_f64(),
            event_count as f64 / insert_duration.as_secs_f64(),
            p95.as_secs_f64() * 1_000.0
        );
        assert!(
            p95 < Duration::from_millis(400),
            "search p95 exceeded 400 ms: {p95:?}"
        );
    }
}
