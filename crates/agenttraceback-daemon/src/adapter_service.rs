use std::{collections::HashMap, env, path::PathBuf, sync::Arc};

use agenttraceback_adapter_sdk::{
    AdapterError, AdapterEvent, AdapterSessionMetadata, AgentAdapter, DetectContext,
    DetectedInstallation, EventSink, HookPlan, HookReceipt, ImportCursor,
};
use agenttraceback_adapters::AdapterRegistry;
use agenttraceback_api::{
    AdapterCapabilityView, AdapterController, AdapterImportResponse, AdapterScanResponse,
    AdapterView, InstallAdapterHookRequest, SessionControllerError, UninstallAdapterHookRequest,
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
}

impl AdapterService {
    /// Creates an adapter service with the built-in registry.
    #[must_use]
    pub fn new(store: Store, pipeline: Arc<IngestionPipeline>) -> Self {
        Self {
            store,
            pipeline,
            registry: Arc::new(AdapterRegistry::built_in()),
        }
    }

    async fn detect_all(
        &self,
    ) -> Result<Vec<(Arc<dyn AgentAdapter>, DetectedInstallation)>, SessionControllerError> {
        let context = detect_context();
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
        let installations = adapter
            .detect(&detect_context())
            .await
            .map_err(adapter_error)?;
        installations
            .into_iter()
            .next()
            .map(|installation| (adapter, installation))
            .ok_or_else(|| {
                SessionControllerError::not_found("No installation was detected for this adapter.")
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
        let adapter = self.registry.get(adapter_id).ok_or_else(|| {
            SessionControllerError::not_found("The requested adapter is not built in.")
        })?;
        let context = detect_context();
        let installations = adapter.detect(&context).await.map_err(adapter_error)?;
        let mut sources_processed = 0_u64;
        let mut events_imported = 0_u64;
        let mut quarantined = 0_u64;
        let mut warnings = Vec::new();
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
            for source in adapter
                .discover_sources(&installation)
                .await
                .map_err(adapter_error)?
            {
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
                ) && offset > size
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
                let outcome = adapter
                    .import(&source, cursor, &sink)
                    .await
                    .map_err(adapter_error)?;
                sources_processed += 1;
                events_imported += outcome.events_imported;
                quarantined += outcome.quarantined;
                warnings.extend(outcome.warnings.clone());
                self.store
                    .save_import_cursor(ImportCursorRecord {
                        import_source_id: source.id,
                        cursor_version: outcome.cursor.version,
                        byte_offset: outcome.cursor.byte_offset,
                        native_cursor: outcome.cursor.native_cursor,
                        source_size: outcome.cursor.source_size,
                        source_mtime_us: outcome.cursor.source_mtime_us,
                        last_event_id: outcome.cursor.last_event_id,
                        updated_at_us: current_time_us(),
                    })
                    .await
                    .map_err(store_error)?;
            }
        }
        Ok(AdapterImportResponse {
            adapter_id: adapter_id.to_owned(),
            sources: sources_processed,
            events_imported,
            quarantined,
            warnings,
        })
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
                started_at_us: current_time_us(),
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
        self.pipeline
            .ingest(event.event, event.raw_payload)
            .await
            .map(|_| ())
            .map_err(|error| AdapterError::Sink(error.to_string()))
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
