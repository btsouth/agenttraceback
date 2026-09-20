//! Raw event validation, normalization, redaction, and canonicalization.

mod redaction;

pub use redaction::{RedactionError, Redactor};

use agenttraceback_blobs::BlobStore;
use agenttraceback_correlation::{CorrelationDecision, CorrelationEngine};
use agenttraceback_crypto::{KeyError, MasterKey};
use agenttraceback_risk::{RiskEngine, RiskFindingDraft};
use agenttraceback_store::{BlobMetadata, Store, StoreError};
use agenttraceback_store::{CorrelationConflictInsert, CorrelationInsert, RiskFindingRecord};
use agenttraceback_types::EventEnvelope;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

const DEFAULT_MAX_RAW_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_QUEUE_CAPACITY: usize = 1_024;

/// Configuration for the bounded normalized-event ingestion pipeline.
#[derive(Clone, Debug)]
pub struct IngestionConfig {
    /// Maximum accepted raw source payload before quota handling is implemented.
    pub max_raw_payload_bytes: usize,
    /// Maximum queued events waiting for persistence.
    pub queue_capacity: usize,
    /// Media category recorded for raw source payloads.
    pub raw_media_category: String,
    /// Retention class recorded for raw source payloads.
    pub raw_retention_class: String,
}

impl Default for IngestionConfig {
    fn default() -> Self {
        Self {
            max_raw_payload_bytes: DEFAULT_MAX_RAW_PAYLOAD_BYTES,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            raw_media_category: "raw_event".to_owned(),
            raw_retention_class: "raw_payload".to_owned(),
        }
    }
}

/// Event ingestion errors.
#[derive(Debug, Error)]
pub enum IngestionError {
    /// Raw input exceeded the configured parser bound.
    #[error("raw event payload exceeds the configured bound")]
    RawPayloadTooLarge,
    /// The bounded ingestion queue is closed.
    #[error("event ingestion queue is unavailable")]
    QueueUnavailable,
    /// A persistence worker stopped before replying.
    #[error("event ingestion worker stopped before replying")]
    WorkerStopped,
    /// The payload could not be sealed or decoded.
    #[error(transparent)]
    Blob(#[from] agenttraceback_blobs::BlobError),
    /// Metadata persistence failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Redaction failed.
    #[error(transparent)]
    Redaction(#[from] RedactionError),
    /// Keyed redaction digest derivation failed.
    #[error(transparent)]
    Key(#[from] KeyError),
    /// The worker task could not be joined.
    #[error("event ingestion worker failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

/// Bounded asynchronous event ingestion backed by the encrypted store and blob store.
#[derive(Debug)]
pub struct IngestionPipeline {
    redactor: Redactor,
    sender: mpsc::Sender<IngestionCommand>,
    worker: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    max_raw_payload_bytes: usize,
}

enum IngestionCommand {
    Event {
        event: Box<EventEnvelope>,
        raw_payload: Option<Vec<u8>>,
        reply: oneshot::Sender<Result<EventEnvelope, IngestionError>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

impl IngestionPipeline {
    /// Starts the bounded ingestion worker.
    #[must_use]
    pub fn start(
        store: Store,
        blobs: BlobStore,
        master_key: MasterKey,
        config: IngestionConfig,
    ) -> Self {
        let max_raw_payload_bytes = config.max_raw_payload_bytes;
        let (sender, receiver) = mpsc::channel(config.queue_capacity.max(1));
        let redactor = Redactor::new(master_key);
        let worker = tokio::spawn(run_worker(
            store,
            blobs,
            redactor.clone(),
            RiskEngine::default(),
            config,
            receiver,
        ));
        Self {
            redactor,
            sender,
            worker: tokio::sync::Mutex::new(Some(worker)),
            max_raw_payload_bytes,
        }
    }

    /// Redacts session metadata before persistence and full-text indexing.
    pub fn redact_preview(&self, text: &str) -> Result<String, RedactionError> {
        self.redactor.redact_text(text)
    }

    /// Queues one normalized event and optional raw source payload.
    pub async fn ingest(
        &self,
        event: EventEnvelope,
        raw_payload: Option<Vec<u8>>,
    ) -> Result<EventEnvelope, IngestionError> {
        if raw_payload
            .as_ref()
            .is_some_and(|payload| payload.len() > self.max_raw_payload_bytes)
        {
            return Err(IngestionError::RawPayloadTooLarge);
        }
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(IngestionCommand::Event {
                event: Box::new(event),
                raw_payload,
                reply,
            })
            .await
            .map_err(|_| IngestionError::QueueUnavailable)?;
        receiver.await.map_err(|_| IngestionError::WorkerStopped)?
    }

    /// Flushes queued events and stops the worker.
    pub async fn close(&self) -> Result<(), IngestionError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(IngestionCommand::Shutdown { reply })
            .await
            .map_err(|_| IngestionError::QueueUnavailable)?;
        receiver.await.map_err(|_| IngestionError::WorkerStopped)?;
        if let Some(worker) = self.worker.lock().await.take() {
            worker.await.map_err(IngestionError::Task)?;
        }
        Ok(())
    }
}

async fn run_worker(
    store: Store,
    blobs: BlobStore,
    redactor: Redactor,
    risk_engine: RiskEngine,
    config: IngestionConfig,
    mut receiver: mpsc::Receiver<IngestionCommand>,
) {
    while let Some(command) = receiver.recv().await {
        match command {
            IngestionCommand::Event {
                event,
                raw_payload,
                reply,
            } => {
                let result = process_event(
                    &store,
                    &blobs,
                    &redactor,
                    &risk_engine,
                    &config,
                    *event,
                    raw_payload,
                )
                .await;
                let _ = reply.send(result);
            }
            IngestionCommand::Shutdown { reply } => {
                let _ = reply.send(());
                break;
            }
        }
    }
}

async fn process_event(
    store: &Store,
    blobs: &BlobStore,
    redactor: &Redactor,
    risk_engine: &RiskEngine,
    config: &IngestionConfig,
    mut event: EventEnvelope,
    raw_payload: Option<Vec<u8>>,
) -> Result<EventEnvelope, IngestionError> {
    if let Some(raw_payload) = raw_payload {
        if raw_payload.len() > config.max_raw_payload_bytes {
            return Err(IngestionError::RawPayloadTooLarge);
        }
        let stored = blobs.put(&raw_payload)?;
        let descriptor = stored.descriptor;
        store
            .record_blob(BlobMetadata {
                id: descriptor.hex_digest.clone(),
                digest: descriptor.hex_digest.clone(),
                format_version: descriptor.format_version,
                plaintext_bytes: descriptor.plaintext_bytes,
                encrypted_bytes: descriptor.encrypted_bytes,
                media_category: config.raw_media_category.clone(),
                retention_class: config.raw_retention_class.clone(),
                created_at_us: current_time_us(),
                expires_at_us: None,
            })
            .await?;
        event.source.raw_blob_id = Some(descriptor.hex_digest);
    }
    if let Some(preview) = event.content.redacted_preview.clone() {
        event.content.redacted_preview = Some(redactor.redact_text(&preview)?);
    }
    if let Some(display) = event.target.display.clone() {
        event.target.display = Some(redactor.redact_text(&display)?);
    }
    let findings = risk_engine.evaluate(&event);
    if let Some(max_severity) = findings.iter().map(|finding| finding.severity).max() {
        event.risk.severity = max_severity;
        event.risk.finding_ids = findings
            .iter()
            .map(|finding| finding.id.to_string())
            .collect();
    }
    let persisted = store.append_events(vec![event]).await?;
    let persisted = persisted
        .into_iter()
        .next()
        .ok_or(IngestionError::WorkerStopped)?;
    if !findings.is_empty() {
        store
            .save_risk_findings(
                findings
                    .into_iter()
                    .map(|finding| risk_finding_record(finding, persisted.session_id))
                    .collect(),
            )
            .await?;
    }
    correlate_event(store, &persisted).await;
    Ok(persisted)
}

async fn correlate_event(store: &Store, event: &EventEnvelope) {
    let candidates = match store.correlation_candidates(event).await {
        Ok(candidates) => candidates,
        Err(error) => {
            tracing::warn!(%error, "correlation candidate query failed");
            return;
        }
    };
    let engine = CorrelationEngine::default();
    for candidate in candidates {
        match engine.evaluate(event, &candidate) {
            CorrelationDecision::Verified(record) => {
                let result = store
                    .save_correlation(
                        Some(CorrelationInsert {
                            id: record.id,
                            session_id: record.session_id,
                            logical_event_id: record.logical_event_id,
                            reported_event_id: record.reported_event_id,
                            observed_event_id: record.observed_event_id,
                            correlation_kind: record.correlation_kind,
                            confidence: record.confidence.as_str().to_owned(),
                            algorithm_version: record.algorithm_version,
                        }),
                        None,
                    )
                    .await;
                if let Err(error) = result {
                    tracing::warn!(%error, "correlation persistence failed");
                }
                break;
            }
            CorrelationDecision::Conflict(conflict) => {
                let result = store
                    .save_correlation(
                        None,
                        Some(CorrelationConflictInsert {
                            id: conflict.id,
                            session_id: conflict.session_id,
                            left_event_id: conflict.left_event_id,
                            right_event_id: conflict.right_event_id,
                            reason_code: conflict.reason_code,
                            detail: conflict.detail,
                        }),
                    )
                    .await;
                if let Err(error) = result {
                    tracing::warn!(%error, "correlation conflict persistence failed");
                }
                break;
            }
            CorrelationDecision::NoMatch => {}
        }
    }
}

fn risk_finding_record(
    finding: RiskFindingDraft,
    session_id: Option<agenttraceback_types::EntityId>,
) -> RiskFindingRecord {
    RiskFindingRecord {
        id: finding.id,
        session_id,
        event_id: finding.event_id,
        rule_id: finding.rule_id,
        rule_version: finding.rule_version,
        severity: finding.severity.as_str().to_owned(),
        initial_status: "open".to_owned(),
        title: finding.title,
        explanation: finding.explanation,
        matched_preview: finding.matched_preview,
        created_at_us: current_time_us(),
    }
}

fn current_time_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}

/// Convenience constructor for an event pipeline.
pub fn ingestion_pipeline(
    store: Store,
    blobs: BlobStore,
    master_key: MasterKey,
    config: IngestionConfig,
) -> IngestionPipeline {
    IngestionPipeline::start(store, blobs, master_key, config)
}

#[cfg(test)]
mod tests {
    use agenttraceback_blobs::BlobStore;
    use agenttraceback_crypto::{BlobCipher, MasterKey};
    use agenttraceback_store::Store;
    use agenttraceback_types::{EventAction, EventEnvelope, EventSource, SourceKind};

    use super::{IngestionConfig, IngestionError, IngestionPipeline};

    fn event(preview: &str) -> EventEnvelope {
        let source = EventSource {
            kind: SourceKind::AgentLog,
            original_source_kind: None,
            adapter_id: Some("codex".to_owned()),
            source_event_id: "raw-1".to_owned(),
            raw_blob_id: None,
        };
        let mut event = EventEnvelope::new(
            source,
            EventAction::Prompt,
            1_799_000_000_000_000,
            1_799_000_000_000_001,
        );
        event.content.redacted_preview = Some(preview.to_owned());
        event
    }

    #[tokio::test]
    async fn raw_payload_is_encrypted_and_preview_is_redacted() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let master_key = MasterKey::generate();
        let store = Store::open(
            directory.path().join("agenttraceback.db"),
            directory.path().join("backups"),
        )
        .await
        .expect("store");
        let cipher = BlobCipher::from_master_key(&master_key).expect("cipher");
        let blobs = BlobStore::open(directory.path().join("blobs"), cipher).expect("blobs");
        let pipeline = IngestionPipeline::start(
            store.clone(),
            blobs.clone(),
            master_key,
            IngestionConfig::default(),
        );
        let raw = b"OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwxyz0123456789".to_vec();
        let persisted = pipeline
            .ingest(
                event("using sk-abcdefghijklmnopqrstuvwxyz0123456789"),
                Some(raw.clone()),
            )
            .await
            .expect("ingest");
        let raw_digest = persisted.source.raw_blob_id.clone().expect("raw digest");
        let digest_bytes = hex::decode(raw_digest).expect("digest hex");
        let digest: [u8; 32] = digest_bytes.try_into().expect("digest length");
        assert_eq!(blobs.get(&digest).expect("raw blob"), raw);
        assert!(
            persisted
                .content
                .redacted_preview
                .as_deref()
                .is_some_and(|preview| preview.contains("[REDACTED:openai_key:"))
        );
        assert_eq!(store.event_count().await.expect("count"), 1);
        pipeline.close().await.expect("close pipeline");
        store.close().await.expect("close store");
    }

    #[tokio::test]
    async fn oversized_raw_payload_is_rejected_before_queueing() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let master_key = MasterKey::generate();
        let store = Store::open(
            directory.path().join("agenttraceback.db"),
            directory.path().join("backups"),
        )
        .await
        .expect("store");
        let cipher = BlobCipher::from_master_key(&master_key).expect("cipher");
        let blobs = BlobStore::open(directory.path().join("blobs"), cipher).expect("blobs");
        let pipeline = IngestionPipeline::start(
            store.clone(),
            blobs,
            master_key,
            IngestionConfig {
                max_raw_payload_bytes: 4,
                ..IngestionConfig::default()
            },
        );
        let result = pipeline.ingest(event("safe"), Some(vec![0_u8; 5])).await;
        assert!(matches!(result, Err(IngestionError::RawPayloadTooLarge)));
        pipeline.close().await.expect("close pipeline");
        store.close().await.expect("close store");
    }

    #[tokio::test]
    async fn risk_findings_are_attached_and_persisted() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let master_key = MasterKey::generate();
        let store = Store::open(
            directory.path().join("agenttraceback.db"),
            directory.path().join("backups"),
        )
        .await
        .expect("store");
        let cipher = BlobCipher::from_master_key(&master_key).expect("cipher");
        let blobs = BlobStore::open(directory.path().join("blobs"), cipher).expect("blobs");
        let pipeline =
            IngestionPipeline::start(store.clone(), blobs, master_key, IngestionConfig::default());
        let mut event = event("git reset --hard HEAD~1");
        event.action = EventAction::CommandExecute;
        let persisted = pipeline.ingest(event, None).await.expect("ingest");
        assert!(!persisted.risk.finding_ids.is_empty());
        assert_eq!(
            persisted.risk.severity,
            agenttraceback_types::RiskSeverity::High
        );
        let connection = rusqlite::Connection::open(store.database_path()).expect("risk database");
        let count = connection
            .query_row("SELECT COUNT(*) FROM risk_findings", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("risk count");
        assert!(count >= 1);
        pipeline.close().await.expect("close pipeline");
        store.close().await.expect("close store");
    }

    #[tokio::test]
    async fn independent_file_events_are_correlated_as_verified() {
        use agenttraceback_store::{ProjectRecord, ProjectRootRecord, SessionRecord};
        use agenttraceback_types::{AttributionConfidence, EntityId, TargetKind};

        let directory = tempfile::tempdir().expect("temporary directory");
        let master_key = MasterKey::generate();
        let store = Store::open(
            directory.path().join("agenttraceback.db"),
            directory.path().join("backups"),
        )
        .await
        .expect("store");
        let cipher = BlobCipher::from_master_key(&master_key).expect("cipher");
        let blobs = BlobStore::open(directory.path().join("blobs"), cipher).expect("blobs");
        let project_id = EntityId::new();
        let session_id = EntityId::new();
        store
            .upsert_project(ProjectRecord {
                id: project_id,
                display_name: "fixture".to_owned(),
                canonical_root: directory.path().to_string_lossy().into_owned(),
                comparison_root: directory.path().to_string_lossy().into_owned(),
                vcs_kind: "none".to_owned(),
                roots: vec![ProjectRootRecord {
                    id: EntityId::new(),
                    canonical_path: directory.path().to_string_lossy().into_owned(),
                    comparison_path: directory.path().to_string_lossy().into_owned(),
                    root_kind: "project".to_owned(),
                    enabled: true,
                }],
                created_at_us: 1,
                last_seen_at_us: 1,
            })
            .await
            .expect("project");
        store
            .create_session(SessionRecord {
                id: session_id,
                project_id: Some(project_id),
                title_preview: Some("correlation".to_owned()),
                state: "active".to_owned(),
                started_at_us: 1,
                ended_at_us: None,
                agent_name: Some("fixture".to_owned()),
                harness_name: None,
                provider_name: None,
                model_name: None,
                source_session_id: None,
                outcome: "unknown".to_owned(),
                outcome_confidence: "unknown".to_owned(),
                recovery_coverage: "exact".to_owned(),
                capture_health: "complete".to_owned(),
            })
            .await
            .expect("session");
        let pipeline =
            IngestionPipeline::start(store.clone(), blobs, master_key, IngestionConfig::default());
        let mut observed = event("write src/auth.ts");
        observed.source.kind = SourceKind::FilesystemObserver;
        observed.evidence.class = agenttraceback_types::EvidenceClass::Observed;
        observed.action = EventAction::FileWrite;
        observed.session_id = Some(session_id);
        observed.project_id = Some(project_id);
        observed.target.kind = TargetKind::File;
        observed.target.normalized_path = Some("src/auth.ts".to_owned());
        observed.content.after_hash = Some("same-hash".to_owned());
        observed.evidence.attribution = AttributionConfidence::High;
        let mut reported = event("write src/auth.ts");
        reported.action = EventAction::FileWrite;
        reported.session_id = Some(session_id);
        reported.project_id = Some(project_id);
        reported.target.kind = TargetKind::File;
        reported.target.normalized_path = Some("src/auth.ts".to_owned());
        reported.content.after_hash = Some("same-hash".to_owned());
        reported.occurred_at_us = observed.occurred_at_us.saturating_add(500_000);
        reported.observed_at_us = reported.occurred_at_us;
        pipeline.ingest(observed, None).await.expect("observed");
        pipeline.ingest(reported, None).await.expect("reported");

        let connection = rusqlite::Connection::open(store.database_path()).expect("database");
        let correlations = connection
            .query_row("SELECT COUNT(*) FROM correlations", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("correlation count");
        assert_eq!(correlations, 1);
        let page = store
            .search(
                agenttraceback_search::SearchQuery::parse("evidence:verified path:src/auth.ts")
                    .expect("query"),
                50,
            )
            .await
            .expect("search");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].evidence, "verified");
        pipeline.close().await.expect("close pipeline");
        store.close().await.expect("close store");
    }
}
