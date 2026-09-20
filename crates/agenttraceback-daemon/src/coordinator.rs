use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use agenttraceback_api::{
    CompleteWrapperRequest, PrepareWrapperRequest, PrepareWrapperResponse, RegisterRootPidRequest,
    SessionController, SessionControllerError,
};
use agenttraceback_blobs::BlobStore;
use agenttraceback_events::{IngestionPipeline, Redactor};
use agenttraceback_platform::{
    DebouncedFileEvent, FileAction, FileWatchError, FilesystemObserver, ProcessDelta, ProcessInfo,
    ProcessObserver,
};
use agenttraceback_projects::{
    IgnoreEngine, ProjectScope, capture_git_state, normalize_relative_path,
};
use agenttraceback_snapshots::{
    CaptureStatus, ManifestChangeKind, ManifestCoverage, ManifestLimits, ProjectManifest,
    capture_manifest_entry,
};
use agenttraceback_store::{
    BlobMetadata, FileRecord, FileVersionRecord, GitStateRecord, ProjectRecord, ProjectRootRecord,
    SessionRecord, SnapshotEntryRecord, SnapshotRecord, Store,
};
use agenttraceback_types::{
    AttributionConfidence, EntityId, EventAction, EventEnvelope, EventSource, ResultStatus,
    SourceKind, TargetKind,
};
use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct WrapperCoordinator {
    inner: Arc<CoordinatorInner>,
}

struct CoordinatorInner {
    store: Store,
    blobs: BlobStore,
    pipeline: Arc<IngestionPipeline>,
    redactor: Redactor,
    projects: Mutex<HashMap<String, Arc<ProjectObservation>>>,
    sessions: RwLock<HashMap<EntityId, ActiveSession>>,
    root_pids: RwLock<HashMap<EntityId, u32>>,
    process_cancel: CancellationToken,
}

struct ActiveSession {
    project_key: String,
    project_id: EntityId,
    scope: ProjectScope,
    agent: Option<String>,
}

struct ProjectObservation {
    project_id: EntityId,
    scope: ProjectScope,
    ignore: Arc<IgnoreEngine>,
    sessions: Arc<RwLock<HashSet<EntityId>>>,
    manifest: Arc<RwLock<ProjectManifest>>,
    cancel: CancellationToken,
    flush_sender: tokio::sync::mpsc::Sender<tokio::sync::oneshot::Sender<()>>,
    flush_receiver: Mutex<Option<tokio::sync::mpsc::Receiver<tokio::sync::oneshot::Sender<()>>>>,
}

impl WrapperCoordinator {
    pub fn new(
        store: Store,
        blobs: BlobStore,
        pipeline: Arc<IngestionPipeline>,
        redactor: Redactor,
    ) -> Self {
        let inner = Arc::new(CoordinatorInner {
            store,
            blobs,
            pipeline,
            redactor,
            projects: Mutex::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
            root_pids: RwLock::new(HashMap::new()),
            process_cancel: CancellationToken::new(),
        });
        spawn_process_observer(Arc::clone(&inner));
        Self { inner }
    }

    pub async fn shutdown(&self) {
        self.inner.process_cancel.cancel();
        let projects = self
            .inner
            .projects
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for project in projects {
            project.cancel.cancel();
        }
    }
}

#[async_trait]
impl SessionController for WrapperCoordinator {
    async fn prepare_wrapper(
        &self,
        request: PrepareWrapperRequest,
    ) -> Result<PrepareWrapperResponse, SessionControllerError> {
        let project_path = PathBuf::from(&request.project_path);
        let discovered = tokio::task::spawn_blocking(move || ProjectScope::discover(project_path))
            .await
            .map_err(|error| controller_internal("project_discovery_failed", error))?
            .map_err(|error| controller_internal("project_discovery_failed", error))?;
        let project_key = discovered.comparison_root.clone();
        let session_id = EntityId::new();
        let started_at_us = current_time_us();
        let title_source = request.label.as_deref().unwrap_or(&request.command_preview);
        let title_preview = self
            .inner
            .redactor
            .redact_text(title_source)
            .map_err(|error| controller_internal("redaction_failed", error))?;

        let mut projects = self.inner.projects.lock().await;
        let (observation, project_created) = if let Some(existing) = projects.get(&project_key) {
            (Arc::clone(existing), false)
        } else {
            let manifest = ProjectManifest::empty(Some(session_id), ManifestCoverage::MetadataOnly)
                .map_err(|error| controller_internal("baseline_failed", error))?;
            let project_id = EntityId::new();
            let project = ProjectRecord {
                id: project_id,
                display_name: discovered.display_name.clone(),
                canonical_root: discovered.canonical_root.to_string_lossy().into_owned(),
                comparison_root: discovered.comparison_root.clone(),
                vcs_kind: match discovered.vcs_kind {
                    agenttraceback_projects::VcsKind::Git => "git",
                    agenttraceback_projects::VcsKind::None => "none",
                }
                .to_owned(),
                roots: vec![ProjectRootRecord {
                    id: EntityId::new(),
                    canonical_path: discovered.canonical_root.to_string_lossy().into_owned(),
                    comparison_path: discovered.comparison_root.clone(),
                    root_kind: "project".to_owned(),
                    enabled: true,
                }],
                created_at_us: started_at_us,
                last_seen_at_us: started_at_us,
            };
            let project_id = self
                .inner
                .store
                .upsert_project(project)
                .await
                .map_err(|error| controller_internal("project_store_failed", error))?;
            let ignore = Arc::new(
                discovered
                    .load_ignore_engine()
                    .map_err(|error| controller_internal("ignore_load_failed", error))?,
            );
            let (flush_sender, flush_receiver) = tokio::sync::mpsc::channel(4);
            let observation = Arc::new(ProjectObservation {
                project_id,
                scope: discovered,
                ignore,
                sessions: Arc::new(RwLock::new(HashSet::new())),
                manifest: Arc::new(RwLock::new(manifest)),
                cancel: CancellationToken::new(),
                flush_sender: flush_sender.clone(),
                flush_receiver: Mutex::new(Some(flush_receiver)),
            });
            projects.insert(project_key.clone(), Arc::clone(&observation));
            (observation, true)
        };
        drop(projects);

        let session_manifest = if request.no_recovery_snapshot {
            ProjectManifest::empty(Some(session_id), ManifestCoverage::MetadataOnly)
                .map_err(|error| controller_internal("baseline_failed", error))?
        } else {
            let scope = observation.scope.clone();
            tokio::task::spawn_blocking(move || {
                ProjectManifest::capture(&scope, Some(session_id), ManifestLimits::default())
            })
            .await
            .map_err(|error| controller_internal("baseline_failed", error))?
            .map_err(|error| controller_internal("baseline_failed", error))?
        };
        *observation.manifest.write().await = session_manifest.clone();

        let session = SessionRecord {
            id: session_id,
            project_id: Some(observation.project_id),
            title_preview: Some(title_preview.clone()),
            state: "active".to_owned(),
            started_at_us,
            ended_at_us: None,
            agent_name: request.agent.clone(),
            harness_name: None,
            provider_name: None,
            model_name: None,
            source_session_id: None,
            outcome: "unknown".to_owned(),
            outcome_confidence: "unknown".to_owned(),
            recovery_coverage: session_manifest.coverage.as_str().to_owned(),
            capture_health: "complete".to_owned(),
        };
        self.inner
            .store
            .create_session(session)
            .await
            .map_err(|error| controller_internal("session_store_failed", error))?;

        self.persist_manifest_snapshot(
            &observation.scope,
            &session_manifest,
            observation.project_id,
            "pre_session",
            false,
        )
        .await?;
        self.persist_git_state(&observation.scope, session_id, "before", started_at_us)
            .await?;

        observation.sessions.write().await.insert(session_id);
        if project_created {
            spawn_project_watcher(Arc::clone(&self.inner.pipeline), Arc::clone(&observation));
        }
        self.inner.sessions.write().await.insert(
            session_id,
            ActiveSession {
                project_key,
                project_id: observation.project_id,
                scope: observation.scope.clone(),
                agent: request.agent.clone(),
            },
        );

        let mut event = EventEnvelope::new(
            EventSource {
                kind: SourceKind::Wrapper,
                original_source_kind: None,
                adapter_id: None,
                source_event_id: format!("wrapper:{session_id}:start"),
                raw_blob_id: None,
            },
            EventAction::SessionStart,
            started_at_us,
            started_at_us,
        );
        event.session_id = Some(session_id);
        event.project_id = Some(observation.project_id);
        event.actor.agent = request.agent;
        event.target.kind = TargetKind::Session;
        event.target.display = Some(title_preview);
        event.evidence.attribution = AttributionConfidence::Exact;
        event.content.redacted_preview = Some(request.command_preview);
        persist_event(&self.inner.pipeline, event).await?;

        Ok(PrepareWrapperResponse {
            session_id: session_id.to_string(),
            project_id: observation.project_id.to_string(),
            project_root: observation
                .scope
                .canonical_root
                .to_string_lossy()
                .into_owned(),
            recovery_coverage: observation
                .manifest
                .read()
                .await
                .coverage
                .as_str()
                .to_owned(),
        })
    }

    async fn register_root_pid(
        &self,
        session_id: &str,
        request: RegisterRootPidRequest,
    ) -> Result<(), SessionControllerError> {
        let session_id = parse_session_id(session_id)?;
        if !self.inner.sessions.read().await.contains_key(&session_id) {
            return Err(SessionControllerError::not_found(
                "The wrapper session is not active.",
            ));
        }
        self.inner
            .root_pids
            .write()
            .await
            .insert(session_id, request.pid);
        Ok(())
    }

    async fn append_transcript(
        &self,
        session_id: &str,
        bytes: &[u8],
    ) -> Result<(), SessionControllerError> {
        let session_id = parse_session_id(session_id)?;
        if !self.inner.sessions.read().await.contains_key(&session_id) {
            return Err(SessionControllerError::not_found(
                "The wrapper session is not active.",
            ));
        }
        let stored = self
            .inner
            .blobs
            .put(bytes)
            .map_err(|error| controller_internal("transcript_store_failed", error))?;
        self.inner
            .store
            .record_blob(BlobMetadata {
                id: stored.descriptor.hex_digest.clone(),
                digest: stored.descriptor.hex_digest,
                format_version: stored.descriptor.format_version,
                plaintext_bytes: stored.descriptor.plaintext_bytes,
                encrypted_bytes: stored.descriptor.encrypted_bytes,
                media_category: "terminal_transcript".to_owned(),
                retention_class: "terminal_transcript".to_owned(),
                created_at_us: current_time_us(),
                expires_at_us: None,
            })
            .await
            .map_err(|error| controller_internal("transcript_metadata_failed", error))
    }

    async fn complete_wrapper(
        &self,
        session_id: &str,
        request: CompleteWrapperRequest,
    ) -> Result<(), SessionControllerError> {
        let session_id = parse_session_id(session_id)?;
        let active = self
            .inner
            .sessions
            .write()
            .await
            .remove(&session_id)
            .ok_or_else(|| {
                SessionControllerError::not_found("The wrapper session is not active.")
            })?;
        self.inner.root_pids.write().await.remove(&session_id);
        let project = self
            .inner
            .projects
            .lock()
            .await
            .get(&active.project_key)
            .cloned();
        if let Some(project) = &project {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let (acknowledge, flushed) = tokio::sync::oneshot::channel();
            if project.flush_sender.send(acknowledge).await.is_ok() {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), flushed).await;
            }
            let mut sessions = project.sessions.write().await;
            sessions.remove(&session_id);
            if sessions.is_empty() {
                project.cancel.cancel();
                drop(sessions);
                self.inner.projects.lock().await.remove(&active.project_key);
            }
        }

        let mut end_event = EventEnvelope::new(
            EventSource {
                kind: SourceKind::Wrapper,
                original_source_kind: None,
                adapter_id: None,
                source_event_id: format!("wrapper:{session_id}:end"),
                raw_blob_id: None,
            },
            EventAction::SessionEnd,
            request.ended_at_us,
            request.ended_at_us,
        );
        end_event.session_id = Some(session_id);
        end_event.project_id = Some(active.project_id);
        end_event.actor.agent = active.agent.clone();
        end_event.target.kind = TargetKind::Session;
        end_event.result.exit_code = i32::try_from(request.exit_code).ok();
        end_event.result.status = if request.signal.is_some() {
            ResultStatus::Cancelled
        } else if request.exit_code == 0 {
            ResultStatus::Success
        } else {
            ResultStatus::Failed
        };
        end_event.evidence.attribution = AttributionConfidence::Exact;
        end_event.content.redacted_preview =
            Some(format!("Wrapper exited with status {}", request.exit_code));
        persist_event(&self.inner.pipeline, end_event).await?;
        let scope = active.scope.clone();
        let post_manifest = tokio::task::spawn_blocking(move || {
            ProjectManifest::capture(&scope, Some(session_id), ManifestLimits::default())
        })
        .await
        .map_err(|error| controller_internal("post_snapshot_failed", error))?
        .map_err(|error| controller_internal("post_snapshot_failed", error))?;
        self.persist_manifest_snapshot(
            &active.scope,
            &post_manifest,
            active.project_id,
            "post_session",
            false,
        )
        .await?;
        self.persist_git_state(&active.scope, session_id, "after", request.ended_at_us)
            .await?;
        if !matches!(
            active.agent.as_deref(),
            Some("codex" | "claude" | "claude-code")
        ) {
            self.inner
                .store
                .finalize_chain(Some(session_id))
                .await
                .map_err(|error| controller_internal("chain_finalize_failed", error))?;
        }
        let outcome = if request.signal.is_some() {
            "cancelled"
        } else if request.exit_code == 0 {
            "succeeded_inferred"
        } else {
            "failed_inferred"
        };
        self.inner
            .store
            .complete_session(
                session_id,
                request.ended_at_us,
                outcome.to_owned(),
                "complete".to_owned(),
            )
            .await
            .map_err(|error| controller_internal("session_complete_failed", error))
    }
}

impl WrapperCoordinator {
    async fn persist_manifest_snapshot(
        &self,
        scope: &ProjectScope,
        manifest: &ProjectManifest,
        project_id: EntityId,
        snapshot_kind: &str,
        capture_sensitive_content: bool,
    ) -> Result<(), SessionControllerError> {
        let mut entries = Vec::with_capacity(manifest.entries.len());
        for (index, entry) in manifest.entries.iter().enumerate() {
            let path = scope.canonical_root.join(&entry.display_path);
            let file_id = self
                .inner
                .store
                .upsert_file(FileRecord {
                    id: EntityId::new(),
                    project_id,
                    current_display_path: entry.display_path.clone(),
                    current_comparison_path: entry.comparison_path.clone(),
                    stable_file_identity: stable_file_identity(&path),
                    first_seen_at_us: manifest.captured_at_us,
                    last_seen_at_us: manifest.captured_at_us,
                    sensitive_class: entry.sensitive_class.clone(),
                })
                .await
                .map_err(|error| controller_internal("file_identity_failed", error))?;
            let version_id = EntityId::new();
            let can_capture_content = entry.capture_status == CaptureStatus::Hashed
                && (capture_sensitive_content || entry.sensitive_class.is_none());
            let mut blob_id = None;
            let mut media_kind = entry
                .display_path
                .rsplit_once('.')
                .map(|(_, extension)| media_kind(extension))
                .unwrap_or("unknown")
                .to_owned();
            if can_capture_content && scope.contains(&path).unwrap_or(false) {
                match tokio::fs::read(&path).await {
                    Ok(bytes) => match self.inner.blobs.put(&bytes) {
                        Ok(stored) => {
                            if let Err(error) = self
                                .inner
                                .store
                                .record_blob(BlobMetadata {
                                    id: stored.descriptor.hex_digest.clone(),
                                    digest: stored.descriptor.hex_digest.clone(),
                                    format_version: stored.descriptor.format_version,
                                    plaintext_bytes: stored.descriptor.plaintext_bytes,
                                    encrypted_bytes: stored.descriptor.encrypted_bytes,
                                    media_category: "file_content".to_owned(),
                                    retention_class: "recovery_snapshot".to_owned(),
                                    created_at_us: manifest.captured_at_us,
                                    expires_at_us: None,
                                })
                                .await
                            {
                                tracing::warn!(%error, "file content metadata write failed");
                            }
                            blob_id = Some(stored.descriptor.hex_digest);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "file content encryption failed");
                            media_kind = "capture_error".to_owned();
                        }
                    },
                    Err(error) => {
                        tracing::warn!(%error, path = %path.display(), "file content read failed");
                        media_kind = "capture_error".to_owned();
                    }
                }
            }
            let capture_status = if blob_id.is_some() {
                entry.capture_status.as_str()
            } else if entry.capture_status == CaptureStatus::Hashed {
                "metadata_only"
            } else {
                entry.capture_status.as_str()
            };
            self.inner
                .store
                .insert_file_version(FileVersionRecord {
                    id: version_id,
                    file_id,
                    session_id: manifest.session_id,
                    event_id: None,
                    display_path_at_time: entry.display_path.clone(),
                    content_hash: entry.content_hash.clone(),
                    blob_id,
                    byte_length: entry.byte_length,
                    media_kind: Some(media_kind),
                    executable: Some(entry.executable),
                    symlink_target: entry.symlink_target.clone(),
                    capture_status: capture_status.to_owned(),
                    created_at_us: manifest.captured_at_us.saturating_add(index as i64),
                })
                .await
                .map_err(|error| controller_internal("file_version_failed", error))?;
            entries.push(SnapshotEntryRecord {
                file_id: Some(file_id),
                display_path: entry.display_path.clone(),
                comparison_path: entry.comparison_path.clone(),
                file_version_id: Some(version_id),
                entry_kind: if entry.capture_status == CaptureStatus::Symlink {
                    "symlink"
                } else {
                    "file"
                }
                .to_owned(),
                capture_status: capture_status.to_owned(),
                git_object_id: None,
            });
        }
        self.inner
            .store
            .save_snapshot(SnapshotRecord {
                id: manifest.id,
                project_id,
                session_id: manifest.session_id,
                snapshot_kind: snapshot_kind.to_owned(),
                coverage: manifest.coverage.as_str().to_owned(),
                started_at_us: manifest.captured_at_us,
                completed_at_us: Some(manifest.captured_at_us),
                manifest_hash: Some(manifest.manifest_hash.clone()),
                status: "complete".to_owned(),
                entries,
            })
            .await
            .map_err(|error| controller_internal("snapshot_store_failed", error))
    }

    async fn persist_git_state(
        &self,
        scope: &ProjectScope,
        session_id: EntityId,
        phase: &str,
        captured_at_us: i64,
    ) -> Result<(), SessionControllerError> {
        let repo_root = scope.canonical_root.clone();
        let snapshot = tokio::task::spawn_blocking(move || capture_git_state(&repo_root))
            .await
            .map_err(|error| controller_internal("git_capture_failed", error))?
            .map_err(|error| controller_internal("git_capture_failed", error))?;
        let Some(snapshot) = snapshot else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(&snapshot)
            .map_err(|error| controller_internal("git_capture_failed", error))?;
        let stored = self
            .inner
            .blobs
            .put(&bytes)
            .map_err(|error| controller_internal("git_capture_failed", error))?;
        self.inner
            .store
            .record_blob(BlobMetadata {
                id: stored.descriptor.hex_digest.clone(),
                digest: stored.descriptor.hex_digest.clone(),
                format_version: stored.descriptor.format_version,
                plaintext_bytes: stored.descriptor.plaintext_bytes,
                encrypted_bytes: stored.descriptor.encrypted_bytes,
                media_category: "git_state".to_owned(),
                retention_class: "metadata".to_owned(),
                created_at_us: captured_at_us,
                expires_at_us: None,
            })
            .await
            .map_err(|error| controller_internal("git_metadata_failed", error))?;
        self.inner
            .store
            .save_git_state(GitStateRecord {
                id: EntityId::new(),
                session_id,
                snapshot_phase: phase.to_owned(),
                repo_root: snapshot.repo_root.to_string_lossy().into_owned(),
                head_oid: snapshot.head_oid,
                branch_name: snapshot.branch_name,
                upstream_name: snapshot.upstream_name,
                status_blob_id: Some(stored.descriptor.hex_digest),
                captured_at_us,
            })
            .await
            .map_err(|error| controller_internal("git_metadata_failed", error))
    }
}

fn spawn_project_watcher(pipeline: Arc<IngestionPipeline>, observation: Arc<ProjectObservation>) {
    tokio::spawn(async move {
        let mut watcher = match FilesystemObserver::watch(&observation.scope.canonical_root) {
            Ok(watcher) => watcher,
            Err(error) => {
                tracing::warn!(%error, "filesystem watcher could not start");
                emit_gap(
                    &pipeline,
                    &observation,
                    &[FileWatchError::Backend(error.to_string())],
                )
                .await;
                return;
            }
        };
        let mut flush_receiver = match observation.flush_receiver.lock().await.take() {
            Some(receiver) => receiver,
            None => return,
        };
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            tokio::select! {
                _ = observation.cancel.cancelled() => break,
                flush = flush_receiver.recv() => {
                    let batch = watcher.drain_now();
                    process_file_batch(&pipeline, &observation, batch).await;
                    if let Some(acknowledge) = flush {
                        let _ = acknowledge.send(());
                    }
                }
                _ = interval.tick() => {
                    let batch = watcher.drain_now();
                    process_file_batch(&pipeline, &observation, batch).await;
                }
            }
        }
    });
}

async fn process_file_batch(
    pipeline: &IngestionPipeline,
    observation: &ProjectObservation,
    batch: agenttraceback_platform::FileEventBatch,
) {
    if batch.events.is_empty() && batch.gaps.is_empty() {
        return;
    }
    if !batch.gaps.is_empty() {
        emit_gap(pipeline, observation, &batch.gaps).await;
        reconcile_after_overflow(pipeline, observation).await;
    }
    for event in batch.events {
        emit_file_event(pipeline, observation, event).await;
    }
}

async fn emit_file_event(
    pipeline: &IngestionPipeline,
    observation: &ProjectObservation,
    file_event: DebouncedFileEvent,
) {
    let relative = file_event
        .path
        .strip_prefix(&observation.scope.canonical_root)
        .map(|path| normalize_relative_path(path, observation.scope.case_insensitive))
        .unwrap_or_default();
    if relative.is_empty() || observation.ignore.check(&relative).ignored {
        return;
    }
    let action = match file_event.action {
        FileAction::Created => EventAction::FileCreate,
        FileAction::Modified => EventAction::FileWrite,
        FileAction::Deleted => EventAction::FileDelete,
        FileAction::RenameFrom | FileAction::RenameTo | FileAction::Rename => {
            EventAction::FileRename
        }
    };
    let mut manifest = observation.manifest.write().await;
    let before = manifest
        .entries
        .iter()
        .find(|entry| entry.comparison_path == relative)
        .and_then(|entry| entry.content_hash.clone());
    let after_entry = if file_event.path.exists()
        && std::fs::symlink_metadata(&file_event.path).is_ok_and(|metadata| !metadata.is_dir())
    {
        capture_manifest_entry(&observation.scope, &file_event.path, manifest.limits).ok()
    } else {
        None
    };
    match file_event.action {
        FileAction::Deleted | FileAction::RenameFrom => {
            manifest
                .entries
                .retain(|entry| entry.comparison_path != relative);
        }
        FileAction::Created | FileAction::Modified | FileAction::RenameTo | FileAction::Rename => {
            if let Some(entry) = &after_entry {
                manifest
                    .entries
                    .retain(|existing| existing.comparison_path != relative);
                manifest.entries.push(entry.clone());
                manifest
                    .entries
                    .sort_by(|left, right| left.comparison_path.cmp(&right.comparison_path));
            }
        }
    }
    drop(manifest);

    let sessions = observation.sessions.read().await;
    let session_id = if sessions.len() == 1 {
        sessions.iter().next().copied()
    } else {
        None
    };
    drop(sessions);
    let attribution = if session_id.is_some() {
        AttributionConfidence::High
    } else {
        AttributionConfidence::Unattributed
    };
    let mut event = EventEnvelope::new(
        EventSource {
            kind: SourceKind::FilesystemObserver,
            original_source_kind: None,
            adapter_id: None,
            source_event_id: format!(
                "fs:{}:{}:{relative}",
                file_event.observed_at_us,
                action.as_str()
            ),
            raw_blob_id: None,
        },
        action,
        file_event.observed_at_us,
        file_event.observed_at_us,
    );
    event.session_id = session_id;
    event.project_id = Some(observation.project_id);
    event.target.kind = TargetKind::File;
    event.target.display = Some(relative.clone());
    event.target.normalized_path = Some(relative.clone());
    event.content.before_hash = before;
    event.content.after_hash = after_entry.and_then(|entry| entry.content_hash);
    event.content.redacted_preview = Some(format!("Observed {}: {relative}", action.as_str()));
    event.result.status = ResultStatus::Success;
    event.evidence.attribution = attribution;
    if let Err(error) = pipeline.ingest(event, None).await {
        tracing::warn!(%error, "filesystem event ingestion failed");
    }
}

async fn emit_gap(
    pipeline: &IngestionPipeline,
    observation: &ProjectObservation,
    gaps: &[FileWatchError],
) {
    let sessions = observation.sessions.read().await;
    let session_id = if sessions.len() == 1 {
        sessions.iter().next().copied()
    } else {
        None
    };
    drop(sessions);
    let mut event = EventEnvelope::new(
        EventSource {
            kind: SourceKind::FilesystemObserver,
            original_source_kind: None,
            adapter_id: None,
            source_event_id: format!("fs-gap:{}:{}", current_time_us(), gaps.len()),
            raw_blob_id: None,
        },
        EventAction::RecorderGap,
        current_time_us(),
        current_time_us(),
    );
    event.session_id = session_id;
    event.project_id = Some(observation.project_id);
    event.target.kind = TargetKind::File;
    event.target.display = Some(
        observation
            .scope
            .canonical_root
            .to_string_lossy()
            .into_owned(),
    );
    event.content.redacted_preview = Some(format!(
        "Filesystem watcher gap: {}",
        gaps.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    ));
    event.result.status = ResultStatus::Failed;
    event.evidence.attribution = if session_id.is_some() {
        AttributionConfidence::High
    } else {
        AttributionConfidence::Unattributed
    };
    if let Err(error) = pipeline.ingest(event, None).await {
        tracing::warn!(%error, "recorder gap event ingestion failed");
    }
}

async fn reconcile_after_overflow(pipeline: &IngestionPipeline, observation: &ProjectObservation) {
    let scope = observation.scope.clone();
    let previous = observation.manifest.read().await.clone();
    let current = match tokio::task::spawn_blocking(move || {
        ProjectManifest::capture(&scope, previous.session_id, previous.limits)
    })
    .await
    {
        Ok(Ok(manifest)) => manifest,
        _ => return,
    };
    let changes = previous.diff(&current);
    *observation.manifest.write().await = current;
    let sessions = observation.sessions.read().await;
    let session_id = if sessions.len() == 1 {
        sessions.iter().next().copied()
    } else {
        None
    };
    drop(sessions);
    for change in changes {
        let path = change
            .after
            .as_ref()
            .or(change.before.as_ref())
            .map(|entry| entry.display_path.clone());
        let Some(path) = path else { continue };
        let action = match change.kind {
            ManifestChangeKind::Created => EventAction::FileCreate,
            ManifestChangeKind::Modified => EventAction::FileWrite,
            ManifestChangeKind::Deleted => EventAction::FileDelete,
        };
        let mut event = EventEnvelope::new(
            EventSource {
                kind: SourceKind::FilesystemObserver,
                original_source_kind: None,
                adapter_id: None,
                source_event_id: format!("fs-rescan:{}:{path}", current_time_us()),
                raw_blob_id: None,
            },
            action,
            current_time_us(),
            current_time_us(),
        );
        event.project_id = Some(observation.project_id);
        event.session_id = session_id;
        event.target.kind = TargetKind::File;
        event.target.display = Some(path.clone());
        event.target.normalized_path = Some(path);
        event.content.redacted_preview = Some("Observed after watcher reconciliation".to_owned());
        event.result.status = ResultStatus::Success;
        event.evidence.attribution = if session_id.is_some() {
            AttributionConfidence::High
        } else {
            AttributionConfidence::Unattributed
        };
        if let Err(error) = pipeline.ingest(event, None).await {
            tracing::warn!(%error, "overflow reconciliation event ingestion failed");
        }
    }
}

fn spawn_process_observer(inner: Arc<CoordinatorInner>) {
    tokio::spawn(async move {
        let mut observer = ProcessObserver::new();
        observer.baseline();
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
        let mut emitted_roots = HashSet::<(EntityId, u32)>::new();
        loop {
            tokio::select! {
                _ = inner.process_cancel.cancelled() => break,
                _ = interval.tick() => {
                    let delta = observer.poll();
                    emit_root_starts(&inner, &observer, &mut emitted_roots).await;
                    emit_process_delta(&inner, &observer, delta).await;
                }
            }
        }
    });
}

async fn emit_root_starts(
    inner: &CoordinatorInner,
    observer: &ProcessObserver,
    emitted_roots: &mut HashSet<(EntityId, u32)>,
) {
    let roots: Vec<_> = inner
        .root_pids
        .read()
        .await
        .iter()
        .map(|(session, pid)| (*session, *pid))
        .collect();
    for (session_id, pid) in roots {
        if !emitted_roots.insert((session_id, pid)) {
            continue;
        }
        if let Some(process) = observer
            .processes()
            .find(|process| process.key.pid == pid)
            .cloned()
        {
            emit_process_event(inner, session_id, EventAction::ProcessStart, &process).await;
        }
    }
}

async fn emit_process_delta(
    inner: &CoordinatorInner,
    observer: &ProcessObserver,
    delta: ProcessDelta,
) {
    let roots: Vec<_> = inner
        .root_pids
        .read()
        .await
        .iter()
        .map(|(session, pid)| (*session, *pid))
        .collect();
    for process in delta.starts {
        for (session_id, root_pid) in &roots {
            if observer.is_descendant(*root_pid, process.key.pid) {
                emit_process_event(inner, *session_id, EventAction::ProcessStart, &process).await;
            }
        }
    }
    for process in delta.exits {
        for (session_id, root_pid) in &roots {
            if observer.is_descendant(*root_pid, process.key.pid) {
                emit_process_event(inner, *session_id, EventAction::ProcessExit, &process).await;
            }
        }
    }
}

async fn emit_process_event(
    inner: &CoordinatorInner,
    session_id: EntityId,
    action: EventAction,
    process: &ProcessInfo,
) {
    let active = inner.sessions.read().await;
    let Some(session) = active.get(&session_id) else {
        return;
    };
    let mut event = EventEnvelope::new(
        EventSource {
            kind: SourceKind::ProcessObserver,
            original_source_kind: None,
            adapter_id: None,
            source_event_id: format!(
                "process:{}:{}:{}",
                process.key.pid,
                process.key.start_time,
                action.as_str()
            ),
            raw_blob_id: None,
        },
        action,
        process.observed_at_us,
        process.observed_at_us,
    );
    event.session_id = Some(session_id);
    event.project_id = Some(session.project_id);
    event.actor.agent = session.agent.clone();
    event.actor.pid = Some(process.key.pid);
    event.actor.parent_pid = process.parent_pid;
    event.target.kind = TargetKind::Process;
    event.target.display = process
        .executable
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    event.result.status = if action == EventAction::ProcessStart {
        ResultStatus::Running
    } else {
        ResultStatus::Unknown
    };
    event.content.redacted_preview = process.argv_preview.clone();
    event.evidence.attribution = AttributionConfidence::Exact;
    drop(active);
    if let Err(error) = inner.pipeline.ingest(event, None).await {
        tracing::warn!(%error, "process event ingestion failed");
    }
}

async fn persist_event(
    pipeline: &IngestionPipeline,
    event: EventEnvelope,
) -> Result<(), SessionControllerError> {
    pipeline
        .ingest(event, None)
        .await
        .map(|_| ())
        .map_err(|error| controller_internal("event_store_failed", error))
}

fn stable_file_identity(path: &std::path::Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path).ok()?;
        Some(format!("{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

fn media_kind(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "c" | "h" | "cpp" | "hpp"
        | "toml" | "json" | "yaml" | "yml" | "md" | "txt" | "css" | "html" | "sh" | "ps1" => "text",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" => "image",
        "zip" | "gz" | "zst" | "tar" | "7z" => "archive",
        _ => "binary",
    }
}

fn parse_session_id(value: &str) -> Result<EntityId, SessionControllerError> {
    value
        .parse()
        .map_err(|_| SessionControllerError::not_found("The session ID is invalid."))
}

fn controller_internal(
    code: &'static str,
    error: impl std::fmt::Display,
) -> SessionControllerError {
    SessionControllerError::internal(code, error.to_string())
}

fn current_time_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use agenttraceback_api::{PrepareWrapperRequest, SessionController};
    use agenttraceback_blobs::BlobStore;
    use agenttraceback_crypto::{BlobCipher, MasterKey};
    use agenttraceback_events::{IngestionConfig, IngestionPipeline, Redactor};
    use agenttraceback_platform::FileWatchError;
    use agenttraceback_search::SearchQuery;
    use agenttraceback_store::Store;
    use agenttraceback_types::EventAction;

    use super::{WrapperCoordinator, emit_gap, reconcile_after_overflow};

    #[tokio::test]
    async fn watcher_overflow_emits_gap_and_rescans_changes() {
        let data = tempfile::tempdir().expect("data directory");
        let project = tempfile::tempdir().expect("project directory");
        let master_key = MasterKey::generate();
        let store = Store::open(
            data.path().join("agenttraceback.db"),
            data.path().join("backups"),
        )
        .await
        .expect("store");
        let cipher = BlobCipher::from_master_key(&master_key).expect("cipher");
        let blobs = BlobStore::open(data.path().join("blobs"), cipher).expect("blobs");
        let pipeline = Arc::new(IngestionPipeline::start(
            store.clone(),
            blobs.clone(),
            master_key.clone(),
            IngestionConfig::default(),
        ));
        let coordinator = WrapperCoordinator::new(
            store.clone(),
            blobs,
            Arc::clone(&pipeline),
            Redactor::new(master_key),
        );
        let prepared = coordinator
            .prepare_wrapper(PrepareWrapperRequest {
                project_path: project.path().to_string_lossy().into_owned(),
                label: Some("overflow test".to_owned()),
                agent: Some("fixture".to_owned()),
                command_preview: "fixture".to_owned(),
                no_recovery_snapshot: false,
            })
            .await
            .expect("prepare");
        let observation = coordinator
            .inner
            .projects
            .lock()
            .await
            .values()
            .next()
            .cloned()
            .expect("project observation");
        fs::write(project.path().join("after-overflow.txt"), "created").expect("fixture");
        reconcile_after_overflow(&pipeline, &observation).await;
        emit_gap(&pipeline, &observation, &[FileWatchError::Overflow]).await;

        let gap_page = store
            .search(
                SearchQuery::parse("Filesystem watcher gap").expect("query"),
                50,
            )
            .await
            .expect("gap search");
        assert!(
            gap_page
                .items
                .iter()
                .any(|item| item.action == EventAction::RecorderGap.as_str())
        );
        let file_page = store
            .search(
                SearchQuery::parse("path:after-overflow.txt").expect("query"),
                50,
            )
            .await
            .expect("file search");
        assert!(
            file_page
                .items
                .iter()
                .any(|item| item.action == EventAction::FileCreate.as_str())
        );
        assert_eq!(
            prepared.session_id,
            observation
                .sessions
                .read()
                .await
                .iter()
                .next()
                .expect("active session")
                .to_string()
        );

        coordinator.shutdown().await;
        pipeline.close().await.expect("close pipeline");
        store.close().await.expect("close store");
    }
}
