use std::{collections::HashMap, env, path::PathBuf, sync::Arc};

use agenttraceback_adapter_sdk::{
    AdapterError, AdapterEvent, AdapterSessionMetadata, AgentAdapter, DetectContext,
    DetectedInstallation, EventSink, HookPlan, HookReceipt, ImportCursor,
};
use agenttraceback_adapters::AdapterRegistry;
use agenttraceback_api::{
    AdapterCapabilityView, AdapterController, AdapterImportResponse, AdapterScanResponse,
    AdapterView, HistoryImportAgent, HistoryImportStatus, InstallAdapterHookRequest,
    SessionControllerError, UninstallAdapterHookRequest,
};
use agenttraceback_events::IngestionPipeline;
use agenttraceback_projects::ProjectScope;
use agenttraceback_store::{
    AdapterHookRecord, AdapterInstallationRecord, ImportCursorRecord, ImportSourceRecord,
    ProjectRecord, ProjectRootRecord, SessionRecord, Store,
};
use agenttraceback_types::EntityId;
use async_trait::async_trait;
use tokio::sync::Mutex;

/// Daemon-owned adapter detection and persistent import.
#[derive(Clone)]
pub struct AdapterService {
    store: Store,
    pipeline: Arc<IngestionPipeline>,
    registry: Arc<AdapterRegistry>,
    context: DetectContext,
    import_gate: Arc<Mutex<()>>,
    history: Arc<Mutex<HistoryImportStatus>>,
}

impl AdapterService {
    /// Creates an adapter service with the built-in registry.
    #[must_use]
    pub fn new(store: Store, pipeline: Arc<IngestionPipeline>) -> Self {
        Self {
            store,
            pipeline,
            registry: Arc::new(AdapterRegistry::built_in()),
            context: detect_context(),
            import_gate: Arc::new(Mutex::new(())),
            history: Arc::new(Mutex::new(HistoryImportStatus::default())),
        }
    }

    async fn detect_all(
        &self,
    ) -> Result<Vec<(Arc<dyn AgentAdapter>, DetectedInstallation)>, SessionControllerError> {
        let context = self.context.clone();
        let mut detected = Vec::new();
        for adapter in self.registry.adapters() {
            let installations = adapter.detect(&context).await.map_err(adapter_error)?;
            for installation in installations {
                self.store
                    .upsert_adapter_installation(AdapterInstallationRecord {
                        id: installation.id,
                        adapter_id: installation.adapter_id.clone(),
                        display_name: installation.display_name.clone(),
                        agent_version: installation.agent_version.clone(),
                        detected_at_us: current_time_us(),
                        last_seen_at_us: current_time_us(),
                        status: if installation.diagnostic_code.is_some() {
                            "degraded"
                        } else {
                            "available"
                        }
                        .to_owned(),
                        diagnostic_code: installation.diagnostic_code.clone(),
                    })
                    .await
                    .map_err(store_error)?;
                detected.push((Arc::clone(adapter), installation));
            }
        }
        Ok(detected)
    }

    async fn adapter_views(&self) -> Result<AdapterScanResponse, SessionControllerError> {
        let detected = self.detect_all().await?;
        let mut adapters = Vec::new();
        for (adapter, installation) in detected {
            let capabilities = adapter
                .capabilities(&installation)
                .await
                .map_err(adapter_error)?;
            adapters.push(AdapterView {
                id: installation.adapter_id.clone(),
                display_name: installation.display_name.clone(),
                installation_id: installation.id.to_string(),
                agent_version: installation.agent_version.clone(),
                source_roots: installation
                    .source_roots
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                status: if installation.diagnostic_code.is_some() {
                    "degraded"
                } else {
                    "available"
                }
                .to_owned(),
                diagnostic_code: installation.diagnostic_code,
                capabilities: capabilities
                    .capabilities
                    .into_iter()
                    .map(|capability| AdapterCapabilityView {
                        name: capability.name,
                        availability: format!("{:?}", capability.availability).to_lowercase(),
                        detail: capability.detail,
                    })
                    .collect(),
            });
        }
        Ok(AdapterScanResponse { adapters })
    }

    async fn detected_installation(
        &self,
        adapter_id: &str,
    ) -> Result<(Arc<dyn AgentAdapter>, DetectedInstallation), SessionControllerError> {
        let adapter = self.registry.get(adapter_id).ok_or_else(|| {
            SessionControllerError::not_found("The requested adapter is not built in.")
        })?;
        let installations = adapter.detect(&self.context).await.map_err(adapter_error)?;
        installations
            .into_iter()
            .next()
            .map(|installation| (adapter, installation))
            .ok_or_else(|| {
                SessionControllerError::not_found("No installation was detected for this adapter.")
            })
    }
}

impl AdapterService {
    async fn update_progress(&self, id: &str, update: impl FnOnce(&mut HistoryImportAgent)) {
        if let Some(progress) = self
            .history
            .lock()
            .await
            .agents
            .iter_mut()
            .find(|agent| agent.adapter_id == id)
        {
            update(progress);
        }
    }

    async fn import_detected_history(&self) {
        let views = match self.adapter_views().await {
            Ok(views) => views,
            Err(error) => {
                let mut history = self.history.lock().await;
                history.status = "completed_with_errors".into();
                history.error = Some(error.message);
                return;
            }
        };
        let mut agents = Vec::new();
        for view in views.adapters {
            if view
                .capabilities
                .iter()
                .any(|cap| cap.name == "historical_sessions" && cap.availability == "available")
                && !agents
                    .iter()
                    .any(|agent: &HistoryImportAgent| agent.adapter_id == view.id)
            {
                agents.push(HistoryImportAgent {
                    adapter_id: view.id,
                    display_name: view.display_name,
                    status: "queued".into(),
                    ..HistoryImportAgent::default()
                });
            }
        }
        self.history.lock().await.agents = agents.clone();
        for agent in agents {
            self.update_progress(&agent.adapter_id, |progress| {
                progress.status = "running".into()
            })
            .await;
            let result = self.run_import(&agent.adapter_id, true).await;
            self.update_progress(&agent.adapter_id, |progress| match result {
                Ok(_) => progress.status = "completed".into(),
                Err(error) => {
                    progress.status = "failed".into();
                    progress.error = Some(error.message);
                }
            })
            .await;
        }
        let mut history = self.history.lock().await;
        history.status = if history.agents.iter().any(|agent| agent.status == "failed") {
            "completed_with_errors"
        } else {
            "completed"
        }
        .into();
    }

    async fn run_import(
        &self,
        adapter_id: &str,
        report: bool,
    ) -> Result<AdapterImportResponse, SessionControllerError> {
        let _guard = self.import_gate.lock().await;
        let adapter = self.registry.get(adapter_id).ok_or_else(|| {
            SessionControllerError::not_found("The requested adapter is not built in.")
        })?;
        let context = self.context.clone();
        let installations = adapter.detect(&context).await.map_err(adapter_error)?;
        let mut sources_processed = 0_u64;
        let mut events_imported = 0_u64;
        let mut quarantined = 0_u64;
        let mut warnings = Vec::new();
        let mut source_errors = Vec::new();
        for installation in installations {
            self.store
                .upsert_adapter_installation(AdapterInstallationRecord {
                    id: installation.id,
                    adapter_id: installation.adapter_id.clone(),
                    display_name: installation.display_name.clone(),
                    agent_version: installation.agent_version.clone(),
                    detected_at_us: current_time_us(),
                    last_seen_at_us: current_time_us(),
                    status: "available".to_owned(),
                    diagnostic_code: installation.diagnostic_code.clone(),
                })
                .await
                .map_err(store_error)?;
            let sources = adapter
                .discover_sources(&installation)
                .await
                .map_err(adapter_error)?;
            if report {
                self.update_progress(adapter_id, |progress| {
                    progress.total_sources += sources.len() as u64
                })
                .await;
            }
            for source in sources {
                let result: Result<(), SessionControllerError> = async {
                    self.store
                        .upsert_import_source(ImportSourceRecord {
                            id: source.id,
                            adapter_installation_id: installation.id,
                            source_kind: source.source_kind.clone(),
                            canonical_location: source
                                .canonical_location
                                .to_string_lossy()
                                .into_owned(),
                            stable_identity: source.stable_identity.clone(),
                            status: "active".to_owned(),
                            last_error_code: None,
                        })
                        .await
                        .map_err(store_error)?;
                    let mut cursor = self
                        .store
                        .get_import_cursor(source.id)
                        .await
                        .map_err(store_error)?
                        .map(store_cursor_to_sdk);
                    let source_size = std::fs::metadata(&source.canonical_location)
                        .ok()
                        .map(|metadata| metadata.len());
                    if let (Some(offset), Some(size)) = (
                        cursor.as_ref().map(|cursor| cursor.byte_offset),
                        source_size,
                    ) && matches!(adapter_id, "claude-code" | "codex")
                        && offset > size
                    {
                        warnings.push(format!(
                            "{} was truncated; restarting import from byte zero",
                            source.canonical_location.display()
                        ));
                        cursor = Some(ImportCursor {
                            version: 1,
                            byte_offset: 0,
                            ..ImportCursor::default()
                        });
                    }
                    let sink =
                        PersistentAdapterSink::new(self.store.clone(), Arc::clone(&self.pipeline));
                    loop {
                        let previous_offset = cursor.as_ref().map_or(0, |value| value.byte_offset);
                        let mut outcome = adapter
                            .import(&source, cursor.clone(), &sink)
                            .await
                            .map_err(adapter_error)?;
                        // Deduplication may return an older persisted event ID.
                        // Adapter-generated IDs are not valid cursor foreign keys
                        // when replaying a partially committed batch.
                        outcome.cursor.last_event_id = (*sink.last_persisted_event.lock().await)
                            .or_else(|| {
                                cursor.as_ref().and_then(|previous| previous.last_event_id)
                            });
                        events_imported += outcome.events_imported;
                        quarantined += outcome.quarantined;
                        warnings.extend(outcome.warnings.clone());
                        self.store
                            .save_import_cursor(ImportCursorRecord {
                                import_source_id: source.id,
                                cursor_version: outcome.cursor.version,
                                byte_offset: outcome.cursor.byte_offset,
                                native_cursor: outcome.cursor.native_cursor.clone(),
                                source_size: outcome.cursor.source_size,
                                source_mtime_us: outcome.cursor.source_mtime_us,
                                last_event_id: outcome.cursor.last_event_id,
                                updated_at_us: current_time_us(),
                            })
                            .await
                            .map_err(store_error)?;
                        if report {
                            self.update_progress(adapter_id, |progress| {
                                progress.events_imported += outcome.events_imported;
                                progress.quarantined += outcome.quarantined;
                            })
                            .await;
                        }
                        // JSONL adapters return bounded batches. Drain the size seen at
                        // discovery, without chasing a continuously growing live log.
                        let more = if matches!(adapter_id, "claude-code" | "codex") {
                            outcome.cursor.byte_offset > previous_offset
                                && source_size.is_some_and(|size| outcome.cursor.byte_offset < size)
                        } else {
                            outcome.cursor.native_cursor.as_ref()
                                != cursor
                                    .as_ref()
                                    .and_then(|previous| previous.native_cursor.as_ref())
                        };
                        cursor = Some(outcome.cursor);
                        if !more {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    sources_processed += 1;
                    if report {
                        self.update_progress(adapter_id, |progress| progress.sources += 1)
                            .await;
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    source_errors.push(format!(
                        "{}: {}",
                        source.canonical_location.display(),
                        error.message
                    ));
                }
            }
        }
        if !source_errors.is_empty() {
            return Err(SessionControllerError::internal(
                "import_sources_failed",
                format!(
                    "{} source(s) could not be imported; other sources were processed. First problem: {}",
                    source_errors.len(),
                    source_errors[0]
                ),
            ));
        }
        Ok(AdapterImportResponse {
            adapter_id: adapter_id.to_owned(),
            sources: sources_processed,
            events_imported,
            quarantined,
            warnings,
        })
    }
}

#[async_trait]
impl AdapterController for AdapterService {
    async fn list_adapters(&self) -> Result<AdapterScanResponse, SessionControllerError> {
        self.adapter_views().await
    }

    async fn scan_adapters(&self) -> Result<AdapterScanResponse, SessionControllerError> {
        self.adapter_views().await
    }

    async fn import_adapter(
        &self,
        adapter_id: &str,
    ) -> Result<AdapterImportResponse, SessionControllerError> {
        self.run_import(adapter_id, false).await
    }

    async fn start_history_import(&self) -> Result<HistoryImportStatus, SessionControllerError> {
        let mut history = self.history.lock().await;
        if history.status == "running" {
            return Ok(history.clone());
        }
        *history = HistoryImportStatus {
            id: Some(EntityId::new().to_string()),
            status: "running".into(),
            agents: Vec::new(),
            error: None,
        };
        let initial = history.clone();
        let service = self.clone();
        tokio::spawn(async move {
            let worker = service.clone();
            if let Err(error) =
                tokio::spawn(async move { worker.import_detected_history().await }).await
            {
                let mut status = service.history.lock().await;
                status.status = "completed_with_errors".into();
                status.error = Some(format!(
                    "Import worker stopped: {error}. Retry to resume saved progress."
                ));
            }
        });
        Ok(initial)
    }

    async fn history_import_status(&self) -> Result<HistoryImportStatus, SessionControllerError> {
        Ok(self.history.lock().await.clone())
    }

    async fn plan_adapter_hook(
        &self,
        adapter_id: &str,
    ) -> Result<Option<HookPlan>, SessionControllerError> {
        let (adapter, installation) = self.detected_installation(adapter_id).await?;
        adapter
            .plan_hook_install(&installation)
            .await
            .map_err(adapter_error)
    }

    async fn install_adapter_hook(
        &self,
        adapter_id: &str,
        request: InstallAdapterHookRequest,
    ) -> Result<HookReceipt, SessionControllerError> {
        if request.plan.adapter_id != adapter_id {
            return Err(SessionControllerError::internal(
                "hook_adapter_mismatch",
                "The approved hook plan belongs to a different adapter.",
            ));
        }
        let (adapter, installation) = self.detected_installation(adapter_id).await?;
        let current = adapter
            .plan_hook_install(&installation)
            .await
            .map_err(adapter_error)?
            .ok_or_else(|| {
                SessionControllerError::not_found("This adapter does not expose a hook plan.")
            })?;
        if current.before_hash != request.plan.before_hash
            || current.plan_digest != request.plan.plan_digest
            || current.config_path != request.plan.config_path
        {
            return Err(SessionControllerError::internal(
                "hook_plan_stale",
                "The hook configuration changed after the plan was generated.",
            ));
        }
        let receipt = adapter.install_hook(current).await.map_err(adapter_error)?;
        self.store
            .save_adapter_hook(AdapterHookRecord {
                id: EntityId::new(),
                adapter_id: receipt.adapter_id.clone(),
                installation_id: Some(installation.id),
                config_path: receipt.config_path.clone(),
                backup_path: receipt.backup_path.clone(),
                plan_digest: receipt.plan_digest.clone(),
                installed_at_us: current_time_us(),
            })
            .await
            .map_err(store_error)?;
        Ok(receipt)
    }

    async fn uninstall_adapter_hook(
        &self,
        adapter_id: &str,
        _request: UninstallAdapterHookRequest,
    ) -> Result<(), SessionControllerError> {
        let adapter = self.registry.get(adapter_id).ok_or_else(|| {
            SessionControllerError::not_found("The requested adapter is not built in.")
        })?;
        let hook = self
            .store
            .get_adapter_hook(adapter_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| {
                SessionControllerError::not_found("No AgentTraceback hook receipt is installed.")
            })?;
        adapter
            .uninstall_hook(HookReceipt {
                adapter_id: hook.adapter_id,
                config_path: hook.config_path,
                backup_path: hook.backup_path,
                plan_digest: hook.plan_digest,
            })
            .await
            .map_err(adapter_error)?;
        self.store
            .delete_adapter_hook(adapter_id)
            .await
            .map_err(store_error)?;
        Ok(())
    }
}

struct PersistentAdapterSink {
    store: Store,
    pipeline: Arc<IngestionPipeline>,
    ensured: Mutex<HashMap<EntityId, EnsuredSession>>,
    last_persisted_event: Mutex<Option<EntityId>>,
}

struct EnsuredSession {
    project_id: Option<EntityId>,
    metadata: AdapterSessionMetadata,
}

impl PersistentAdapterSink {
    fn new(store: Store, pipeline: Arc<IngestionPipeline>) -> Self {
        Self {
            store,
            pipeline,
            ensured: Mutex::new(HashMap::new()),
            last_persisted_event: Mutex::new(None),
        }
    }

    async fn ensure_session(
        &self,
        fallback_session_id: EntityId,
        metadata: &AdapterSessionMetadata,
        occurred_at_us: i64,
    ) -> Result<EntityId, AdapterError> {
        if let Some((session_id, _)) = self.ensured.lock().await.iter().find(|(_, session)| {
            session.metadata.native_session_id == metadata.native_session_id
                && session.metadata.agent_name == metadata.agent_name
        }) {
            return Ok(*session_id);
        }
        let project_id = if let Some(root) = &metadata.project_root {
            match ProjectScope::discover(root) {
                Ok(scope) => {
                    let project_id = self
                        .store
                        .upsert_project(ProjectRecord {
                            id: scope.id,
                            display_name: scope.display_name,
                            canonical_root: scope.canonical_root.to_string_lossy().into_owned(),
                            comparison_root: scope.comparison_root,
                            vcs_kind: match scope.vcs_kind {
                                agenttraceback_projects::VcsKind::Git => "git",
                                agenttraceback_projects::VcsKind::None => "none",
                            }
                            .to_owned(),
                            roots: vec![ProjectRootRecord {
                                id: EntityId::new(),
                                canonical_path: scope.canonical_root.to_string_lossy().into_owned(),
                                comparison_path: scope
                                    .canonical_root
                                    .to_string_lossy()
                                    .into_owned(),
                                root_kind: "project".to_owned(),
                                enabled: true,
                            }],
                            created_at_us: current_time_us(),
                            last_seen_at_us: current_time_us(),
                        })
                        .await
                        .map_err(store_to_adapter_error)?;
                    Some(project_id)
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        path = %root.display(),
                        "historical project path is unavailable"
                    );
                    None
                }
            }
        } else {
            None
        };
        let session_id = if let Some(project_id) = project_id {
            self.store
                .find_session_for_native_import(project_id, &metadata.agent_name, occurred_at_us)
                .await
                .map_err(store_to_adapter_error)?
                .unwrap_or(fallback_session_id)
        } else {
            fallback_session_id
        };
        self.store
            .upsert_session(SessionRecord {
                id: session_id,
                project_id,
                title_preview: metadata
                    .title_preview
                    .as_deref()
                    .map(|title| self.pipeline.redact_preview(title))
                    .transpose()
                    .map_err(|error| AdapterError::Sink(error.to_string()))?,
                state: "imported".to_owned(),
                started_at_us: occurred_at_us,
                ended_at_us: None,
                agent_name: Some(metadata.agent_name.clone()),
                harness_name: metadata.harness_name.clone(),
                provider_name: metadata.provider_name.clone(),
                model_name: metadata.model_name.clone(),
                source_session_id: Some(metadata.native_session_id.clone()),
                outcome: "unknown".to_owned(),
                outcome_confidence: "unknown".to_owned(),
                recovery_coverage: "unavailable".to_owned(),
                capture_health: "historical_import".to_owned(),
            })
            .await
            .map_err(store_to_adapter_error)?;
        self.ensured.lock().await.insert(
            session_id,
            EnsuredSession {
                project_id,
                metadata: metadata.clone(),
            },
        );
        Ok(session_id)
    }
}

#[async_trait]
impl EventSink for PersistentAdapterSink {
    async fn emit(&self, mut event: AdapterEvent) -> Result<(), AdapterError> {
        if let Some(metadata) = event.session.take() {
            let session_id = event.event.session_id.ok_or_else(|| {
                AdapterError::Sink("adapter event is missing a session ID".to_owned())
            })?;
            let session_id = self
                .ensure_session(session_id, &metadata, event.event.occurred_at_us)
                .await?;
            event.event.session_id = Some(session_id);
            if event.event.project_id.is_none() {
                event.event.project_id = self
                    .ensured
                    .lock()
                    .await
                    .get(&session_id)
                    .and_then(|session| session.project_id);
            }
        }
        if event.event.session_id.is_none() {
            return Err(AdapterError::Sink(
                "adapter event is missing a session ID".to_owned(),
            ));
        }
        let persisted = self
            .pipeline
            .ingest(event.event, event.raw_payload)
            .await
            .map_err(|error| AdapterError::Sink(error.to_string()))?;
        *self.last_persisted_event.lock().await = Some(persisted.id);
        Ok(())
    }
}

fn detect_context() -> DetectContext {
    let home_dir = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let path_entries = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    DetectContext {
        home_dir,
        path_entries,
    }
}

fn store_cursor_to_sdk(cursor: ImportCursorRecord) -> ImportCursor {
    ImportCursor {
        version: cursor.cursor_version,
        byte_offset: cursor.byte_offset,
        native_cursor: cursor.native_cursor,
        source_size: cursor.source_size,
        source_mtime_us: cursor.source_mtime_us,
        last_event_id: cursor.last_event_id,
    }
}

fn adapter_error(error: AdapterError) -> SessionControllerError {
    SessionControllerError::internal("adapter_failed", error.to_string())
}

fn store_error(error: agenttraceback_store::StoreError) -> SessionControllerError {
    SessionControllerError::internal("adapter_store_failed", error.to_string())
}

fn store_to_adapter_error(error: agenttraceback_store::StoreError) -> AdapterError {
    AdapterError::Sink(error.to_string())
}

fn current_time_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agenttraceback_blobs::BlobStore;
    use agenttraceback_crypto::{BlobCipher, MasterKey};
    use agenttraceback_events::IngestionConfig;

    #[tokio::test]
    async fn background_import_drains_batches_survives_clients_and_isolates_errors() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        for root in [
            ".claude/projects",
            ".codex/sessions",
            ".gemini/tmp",
            ".hermes",
        ] {
            std::fs::create_dir_all(home.join(root)).unwrap();
        }
        let mut claude = String::new();
        for index in 0..1501 {
            claude.push_str(&format!("{{\"type\":\"user\",\"sessionId\":\"claude-long\",\"uuid\":\"u{index}\",\"timestamp\":\"2026-09-20T10:00:00Z\",\"message\":{{\"role\":\"user\",\"content\":\"fixture {index}\"}}}}\n"));
        }
        std::fs::write(home.join(".claude/projects/long.jsonl"), claude).unwrap();
        let mut codex = "{\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-long\"},\"timestamp\":\"2026-09-20T10:00:00Z\"}\n".to_owned();
        for index in 0..1501 {
            codex.push_str(&format!("{{\"type\":\"response_item\",\"timestamp\":\"2026-09-20T10:00:00Z\",\"payload\":{{\"id\":\"message-{index}\",\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"fixture {index}\"}}]}}}}\n"));
        }
        std::fs::write(home.join(".codex/sessions/rollout-long.jsonl"), codex).unwrap();
        std::fs::write(home.join(".gemini/tmp/a-broken.json"), "{broken").unwrap();
        std::fs::write(
            home.join(".gemini/tmp/z-valid.json"),
            "{\"sessionId\":\"empty\",\"messages\":[]}",
        )
        .unwrap();
        let db = rusqlite::Connection::open(home.join(".hermes/state.db")).unwrap();
        db.execute_batch("CREATE TABLE sessions (id TEXT PRIMARY KEY, cwd TEXT, title TEXT, model TEXT, parent_session_id TEXT, started_at REAL);
            CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, tool_name TEXT, tool_calls TEXT, finish_reason TEXT);
            INSERT INTO sessions VALUES ('hermes-long', NULL, 'History fixture', NULL, NULL, 1789900800.0);
            WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n < 1501)
            INSERT INTO messages SELECT n, 'hermes-long', 'user', 'fixture', NULL, NULL, NULL FROM ids;").unwrap();
        drop(db);
        let store = Store::open(
            directory.path().join("db"),
            directory.path().join("backups"),
        )
        .await
        .unwrap();
        let key = MasterKey::generate();
        let blobs = BlobStore::open(
            directory.path().join("blobs"),
            BlobCipher::from_master_key(&key).unwrap(),
        )
        .unwrap();
        let pipeline = Arc::new(IngestionPipeline::start(
            store.clone(),
            blobs,
            key,
            IngestionConfig::default(),
        ));
        let mut service = AdapterService::new(store.clone(), pipeline.clone());
        service.context = DetectContext {
            home_dir: Some(home.clone()),
            path_entries: Vec::new(),
        };
        // Keep actual import work busy past the desktop's five-second deadline.
        let gate = service.import_gate.lock().await;
        let initial = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            service.start_history_import(),
        )
        .await
        .unwrap()
        .unwrap();
        let second = service.start_history_import().await.unwrap();
        assert_eq!(
            initial.id, second.id,
            "double-click must not duplicate jobs"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5100)).await;
        assert_eq!(
            service.history_import_status().await.unwrap().status,
            "running"
        );
        drop(gate);
        let status = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                let status = service.history_import_status().await.unwrap();
                if status.status != "running" {
                    break status;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(status.status, "completed_with_errors");
        assert_eq!(store.event_count().await.unwrap(), 4504);
        let sessions = store.list_sessions(100).await.unwrap();
        assert_eq!(
            sessions.len(),
            3,
            "Codex batches must retain the same native session"
        );
        let gemini = status
            .agents
            .iter()
            .find(|agent| agent.adapter_id == "gemini-cli")
            .unwrap();
        assert_eq!(gemini.status, "failed");
        assert_eq!(
            gemini.sources, 1,
            "a broken source must not block another source"
        );
        assert_eq!(
            service
                .import_adapter("codex")
                .await
                .unwrap()
                .events_imported,
            0
        );
        assert_eq!(
            service
                .import_adapter("claude-code")
                .await
                .unwrap()
                .events_imported,
            0
        );
        assert_eq!(
            store.event_count().await.unwrap(),
            4504,
            "retry must not duplicate history"
        );
        // Simulate a crash after event persistence but before the cursor checkpoint.
        let connection = rusqlite::Connection::open(store.database_path()).unwrap();
        connection.execute("UPDATE import_cursors SET byte_offset = 0, native_cursor = NULL, last_event_id = NULL WHERE import_source_id IN (SELECT id FROM import_sources WHERE canonical_location = ?1)", [home.join(".claude/projects/long.jsonl").to_string_lossy().as_ref()]).unwrap();
        assert_eq!(
            service
                .import_adapter("claude-code")
                .await
                .unwrap()
                .events_imported,
            1501
        );
        assert_eq!(
            store.event_count().await.unwrap(),
            4504,
            "replay before checkpoint must deduplicate and save a valid cursor"
        );
        assert_eq!(
            service
                .import_adapter("claude-code")
                .await
                .unwrap()
                .events_imported,
            0
        );
        drop(connection);
        pipeline.close().await.unwrap();
        store.close().await.unwrap();
    }

    #[tokio::test]
    async fn imported_title_is_redacted_before_storage() {
        let directory = tempfile::tempdir().expect("directory");
        let store = Store::open(
            directory.path().join("db"),
            directory.path().join("backups"),
        )
        .await
        .expect("store");
        let key = MasterKey::generate();
        let blobs = BlobStore::open(
            directory.path().join("blobs"),
            BlobCipher::from_master_key(&key).expect("cipher"),
        )
        .expect("blobs");
        let pipeline = Arc::new(IngestionPipeline::start(
            store.clone(),
            blobs,
            key,
            IngestionConfig::default(),
        ));
        let sink = PersistentAdapterSink::new(store.clone(), pipeline.clone());
        let id = sink
            .ensure_session(
                EntityId::new(),
                &AdapterSessionMetadata {
                    native_session_id: "fixture".to_owned(),
                    project_root: None,
                    title_preview: Some("Fix sk-secretsecretsecretsecret".to_owned()),
                    agent_name: "fixture".to_owned(),
                    harness_name: None,
                    provider_name: None,
                    model_name: None,
                    historical: true,
                },
                current_time_us(),
            )
            .await
            .expect("session");
        let session = store
            .get_session_summary(id)
            .await
            .expect("query")
            .expect("session");
        let title = session.title_preview.expect("title");
        assert!(!title.contains("sk-secretsecretsecretsecret"));
        assert!(title.contains("REDACTED"));
        pipeline.close().await.expect("shutdown");
        store.close().await.expect("close");
    }
}
