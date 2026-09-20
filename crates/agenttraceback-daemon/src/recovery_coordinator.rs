use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use agenttraceback_api::{
    ExecuteRecoveryRequest, PlanRecoveryRequest, RecoveryController, RecoveryExclusionView,
    RecoveryOperationView, RecoveryPlanView, RecoveryRunView, SessionControllerError,
};
use agenttraceback_blobs::BlobStore;
use agenttraceback_recovery::{
    ConflictMode, RecoveryAction, RecoveryError, RecoveryOperationKind, RecoveryPlan,
    backup_before_restore, create_git_recovery_worktree, execute, execute_with_backup_guard,
};
use agenttraceback_store::{BlobMetadata, RecoveryPlanRecord, RecoveryRunRecord, Store};
use agenttraceback_types::EntityId;
use async_trait::async_trait;

/// Daemon-owned recovery plan persistence and execution.
#[derive(Clone)]
pub struct RecoveryCoordinator {
    store: Store,
    blobs: BlobStore,
    execution_lock: Arc<tokio::sync::Mutex<()>>,
}

impl RecoveryCoordinator {
    /// Creates a recovery coordinator.
    #[must_use]
    pub fn new(store: Store, blobs: BlobStore) -> Self {
        Self {
            store,
            blobs,
            execution_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    async fn persist_plan(&self, plan: &RecoveryPlan) -> Result<(), SessionControllerError> {
        let plan_bytes = serde_json::to_vec(plan)
            .map_err(|error| controller_internal("recovery_plan_encode_failed", error))?;
        let stored = self
            .blobs
            .put(&plan_bytes)
            .map_err(|error| controller_internal("recovery_plan_store_failed", error))?;
        self.store
            .record_blob(BlobMetadata {
                id: stored.descriptor.hex_digest.clone(),
                digest: stored.descriptor.hex_digest.clone(),
                format_version: stored.descriptor.format_version,
                plaintext_bytes: stored.descriptor.plaintext_bytes,
                encrypted_bytes: stored.descriptor.encrypted_bytes,
                media_category: "recovery_plan".to_owned(),
                retention_class: "metadata".to_owned(),
                created_at_us: current_time_us(),
                expires_at_us: None,
            })
            .await
            .map_err(|error| controller_internal("recovery_plan_store_failed", error))?;
        self.store
            .save_recovery_plan(RecoveryPlanRecord {
                id: plan.id,
                session_id: plan.session_id,
                action: action_name(plan.action).to_owned(),
                destination: plan.destination.to_string_lossy().into_owned(),
                status: "prepared".to_owned(),
                plan_digest: plan.plan_digest.clone(),
                plan_blob_id: stored.descriptor.hex_digest,
                created_at_us: plan.created_at_us,
                executed_at_us: None,
                result_blob_id: None,
            })
            .await
            .map_err(|error| controller_internal("recovery_plan_store_failed", error))?;
        Ok(())
    }

    async fn load_plan(
        &self,
        plan_id: EntityId,
    ) -> Result<(RecoveryPlanRecord, RecoveryPlan), SessionControllerError> {
        let record = self
            .store
            .get_recovery_plan(plan_id)
            .await
            .map_err(|error| controller_internal("recovery_plan_load_failed", error))?
            .ok_or_else(|| SessionControllerError::not_found("Recovery plan was not found."))?;
        let digest = parse_digest(&record.plan_blob_id)
            .map_err(|error| controller_internal("recovery_plan_load_failed", error))?;
        let bytes = self
            .blobs
            .get(&digest)
            .map_err(|error| controller_internal("recovery_plan_load_failed", error))?;
        let plan: RecoveryPlan = serde_json::from_slice(&bytes)
            .map_err(|error| controller_internal("recovery_plan_load_failed", error))?;
        plan.verify_digest()
            .map_err(|error| map_recovery_error(error, false))?;
        if plan.id != record.id
            || plan.session_id != record.session_id
            || plan.plan_digest != record.plan_digest
            || plan.destination.to_string_lossy() != record.destination
        {
            return Err(controller_internal(
                "recovery_plan_stale",
                "Stored recovery metadata does not match the authenticated plan.",
            ));
        }
        Ok((record, plan))
    }
}

#[async_trait]
impl RecoveryController for RecoveryCoordinator {
    async fn plan_recovery(
        &self,
        request: PlanRecoveryRequest,
    ) -> Result<RecoveryPlanView, SessionControllerError> {
        let session_id = parse_id(&request.session_id, "session")?;
        let mut versions = self
            .store
            .load_snapshot_versions(session_id, "pre_session")
            .await
            .map_err(|error| controller_internal("recovery_snapshot_load_failed", error))?;
        if versions.is_empty() {
            return Err(controller_internal(
                "recovery_snapshot_unavailable",
                "No eligible pre-session snapshot exists for this session.",
            ));
        }
        let destination = PathBuf::from(&request.destination);
        if destination.as_os_str().is_empty() {
            return Err(controller_internal(
                "recovery_destination_required",
                "A recovery destination is required.",
            ));
        }
        let action = parse_action(&request.action)?;
        let plan = match action {
            RecoveryAction::ReconstructPreSession => RecoveryPlan::reconstruct_pre_session(
                session_id,
                "exact_or_partial_from_snapshot",
                destination,
                &versions,
            ),
            RecoveryAction::RestoreSingleFile => {
                let path = request.paths.first().ok_or_else(|| {
                    controller_internal(
                        "recovery_path_required",
                        "A source path is required for single-file recovery.",
                    )
                })?;
                versions.retain(|version| {
                    version.comparison_path == *path || version.display_path == *path
                });
                let version = versions.first().ok_or_else(|| {
                    SessionControllerError::not_found(
                        "The requested file was not present in the pre-session snapshot.",
                    )
                })?;
                RecoveryPlan::restore_single_file(session_id, "exact", destination, version)
            }
            RecoveryAction::InPlaceRestore => {
                let selected: Vec<_> = if request.paths.is_empty() {
                    versions.clone()
                } else {
                    versions
                        .into_iter()
                        .filter(|version| {
                            request.paths.iter().any(|path| {
                                path == &version.comparison_path || path == &version.display_path
                            })
                        })
                        .collect()
                };
                RecoveryPlan::in_place(session_id, "exact", destination, &selected)
            }
            RecoveryAction::GitRecoveryWorktree => RecoveryPlan::git_recovery_worktree(
                session_id,
                "exact_or_partial_from_snapshot",
                destination,
                &versions,
            ),
        }
        .map_err(|error| map_recovery_error(error, false))?;

        self.persist_plan(&plan).await?;
        Ok(plan_view(&plan))
    }

    async fn get_recovery_plan(
        &self,
        plan_id: &str,
    ) -> Result<RecoveryPlanView, SessionControllerError> {
        let plan_id = parse_id(plan_id, "plan")?;
        let (_record, plan) = self.load_plan(plan_id).await?;
        Ok(plan_view(&plan))
    }

    async fn execute_recovery(
        &self,
        plan_id: &str,
        request: ExecuteRecoveryRequest,
    ) -> Result<RecoveryRunView, SessionControllerError> {
        if !request.confirm {
            return Err(controller_internal(
                "recovery_confirmation_required",
                "Recovery execution requires explicit confirmation.",
            ));
        }
        let _execution_guard = self.execution_lock.lock().await;
        let plan_id = parse_id(plan_id, "plan")?;
        let (mut record, plan) = self.load_plan(plan_id).await?;
        if request.plan_digest != plan.plan_digest || record.plan_digest != plan.plan_digest {
            return Err(controller_internal(
                "recovery_plan_stale",
                "The recovery plan digest is stale or does not match.",
            ));
        }
        let run_id = EntityId::new();
        let target_root = if plan.action == RecoveryAction::GitRecoveryWorktree {
            let git_state = self
                .store
                .get_git_state(plan.session_id, "before")
                .await
                .map_err(|error| controller_internal("recovery_git_state_failed", error))?
                .ok_or_else(|| {
                    controller_internal(
                        "recovery_git_state_unavailable",
                        "The pre-session Git state is unavailable.",
                    )
                })?;
            let git_root = agenttraceback_projects::git_root(Path::new(&git_state.repo_root))
                .map_err(|error| controller_internal("recovery_git_root_failed", error))?;
            let selected_root = std::fs::canonicalize(&git_state.repo_root)
                .map_err(|error| controller_internal("recovery_git_root_failed", error))?;
            if git_root
                .as_deref()
                .and_then(|path| std::fs::canonicalize(path).ok())
                .as_ref()
                != Some(&selected_root)
            {
                return Err(controller_internal(
                    "recovery_git_scope_partial",
                    "This snapshot covers a subdirectory. Reconstruct into a new directory instead of a Git worktree.",
                ));
            }
            let head_oid = git_state.head_oid.ok_or_else(|| {
                controller_internal(
                    "recovery_git_head_unavailable",
                    "The pre-session Git HEAD is unavailable.",
                )
            })?;
            let branch = format!(
                "agenttraceback/recovery/{}-{}",
                plan.session_id
                    .to_string()
                    .chars()
                    .take(8)
                    .collect::<String>(),
                current_time_us()
            );
            create_git_recovery_worktree(
                Path::new(&git_state.repo_root),
                &plan.destination,
                &head_oid,
                &branch,
            )
            .map_err(|error| map_recovery_error(error, false))?;
            plan.destination.clone()
        } else {
            plan.destination.clone()
        };
        let conflict_mode = if request.overwrite_conflicts {
            ConflictMode::Overwrite
        } else {
            ConflictMode::Refuse
        };
        let backup_plan = if plan.action == RecoveryAction::InPlaceRestore {
            let (backup, descriptors) = backup_before_restore(&plan, &self.blobs, conflict_mode)
                .map_err(|error| map_recovery_error(error, false))?;
            for descriptor in descriptors {
                self.store
                    .record_blob(BlobMetadata {
                        id: descriptor.hex_digest.clone(),
                        digest: descriptor.hex_digest,
                        format_version: descriptor.format_version,
                        plaintext_bytes: descriptor.plaintext_bytes,
                        encrypted_bytes: descriptor.encrypted_bytes,
                        media_category: "pre_restore_backup".to_owned(),
                        retention_class: "recovery".to_owned(),
                        created_at_us: current_time_us(),
                        expires_at_us: None,
                    })
                    .await
                    .map_err(|error| controller_internal("recovery_backup_failed", error))?;
            }
            self.persist_plan(&backup).await?;
            Some(backup)
        } else {
            None
        };
        let run = RecoveryRunRecord {
            id: run_id,
            plan_id,
            backup_plan_id: backup_plan.as_ref().map(|backup| backup.id),
            state: "running".to_owned(),
            restored_files: 0,
            skipped_files: 0,
            conflict_files: 0,
            result_blob_id: None,
            created_at_us: current_time_us(),
            finished_at_us: None,
            error_code: None,
        };
        self.store
            .save_recovery_run(run.clone())
            .await
            .map_err(|error| controller_internal("recovery_run_store_failed", error))?;
        let execution = if let Some(backup) = &backup_plan {
            execute_with_backup_guard(&plan, backup, &self.blobs)
        } else {
            execute(&plan, &self.blobs, &target_root, conflict_mode)
        };
        let report = match execution {
            Ok(report) => report,
            Err(error) => {
                let mut failed = run;
                failed.state = "failed".to_owned();
                failed.finished_at_us = Some(current_time_us());
                failed.error_code = Some(error_code(&error).to_owned());
                let _ = self.store.save_recovery_run(failed).await;
                let mut error = map_recovery_error(error, true);
                if let Some(backup) = &backup_plan {
                    error
                        .message
                        .push_str(&format!(" Pre-restore backup plan: {}.", backup.id));
                }
                return Err(error);
            }
        };
        let report_bytes = serde_json::json!({
            "destination": report.destination,
            "restoredFiles": report.restored_files,
            "skippedFiles": report.skipped_files,
            "conflictFiles": report.conflict_files,
        })
        .to_string()
        .into_bytes();
        let stored = self
            .blobs
            .put(&report_bytes)
            .map_err(|error| controller_internal("recovery_result_store_failed", error))?;
        self.store
            .record_blob(BlobMetadata {
                id: stored.descriptor.hex_digest.clone(),
                digest: stored.descriptor.hex_digest.clone(),
                format_version: stored.descriptor.format_version,
                plaintext_bytes: stored.descriptor.plaintext_bytes,
                encrypted_bytes: stored.descriptor.encrypted_bytes,
                media_category: "recovery_result".to_owned(),
                retention_class: "metadata".to_owned(),
                created_at_us: current_time_us(),
                expires_at_us: None,
            })
            .await
            .map_err(|error| controller_internal("recovery_result_store_failed", error))?;
        let run = RecoveryRunRecord {
            state: "succeeded".to_owned(),
            restored_files: report.restored_files,
            skipped_files: report.skipped_files,
            conflict_files: report.conflict_files,
            result_blob_id: Some(stored.descriptor.hex_digest),
            finished_at_us: Some(current_time_us()),
            ..run
        };
        self.store
            .save_recovery_run(run.clone())
            .await
            .map_err(|error| controller_internal("recovery_run_store_failed", error))?;
        record.status = "executed".to_owned();
        record.executed_at_us = run.finished_at_us;
        record.result_blob_id = run.result_blob_id.clone();
        self.store
            .save_recovery_plan(record)
            .await
            .map_err(|error| controller_internal("recovery_plan_store_failed", error))?;
        Ok(run_view(&run, &plan.destination))
    }

    async fn get_recovery_run(
        &self,
        run_id: &str,
    ) -> Result<RecoveryRunView, SessionControllerError> {
        let run_id = parse_id(run_id, "run")?;
        let run = self
            .store
            .get_recovery_run(run_id)
            .await
            .map_err(|error| controller_internal("recovery_run_load_failed", error))?
            .ok_or_else(|| SessionControllerError::not_found("Recovery run was not found."))?;
        let (_record, plan) = self.load_plan(run.plan_id).await?;
        Ok(run_view(&run, &plan.destination))
    }
}

fn plan_view(plan: &RecoveryPlan) -> RecoveryPlanView {
    RecoveryPlanView {
        plan_id: plan.id.to_string(),
        session_id: plan.session_id.to_string(),
        action: action_name(plan.action).to_owned(),
        destination: plan.destination.to_string_lossy().into_owned(),
        coverage: plan.coverage.clone(),
        plan_digest: plan.plan_digest.clone(),
        operations: plan
            .operations
            .iter()
            .map(|operation| RecoveryOperationView {
                relative_path: operation.relative_path.clone(),
                kind: match operation.kind {
                    RecoveryOperationKind::WriteFile => "write_file",
                    RecoveryOperationKind::CreateSymlink => "create_symlink",
                    RecoveryOperationKind::RemoveFile => "remove_file",
                }
                .to_owned(),
                expected_hash: operation.expected_hash.clone(),
                expected_current_hash: operation.expected_current_hash.clone(),
                byte_length: operation.byte_length,
                executable: operation.executable,
            })
            .collect(),
        exclusions: plan
            .exclusions
            .iter()
            .map(|exclusion| RecoveryExclusionView {
                relative_path: exclusion.relative_path.clone(),
                reason: exclusion.reason.clone(),
            })
            .collect(),
    }
}

fn run_view(run: &RecoveryRunRecord, destination: &Path) -> RecoveryRunView {
    RecoveryRunView {
        run_id: run.id.to_string(),
        plan_id: run.plan_id.to_string(),
        state: run.state.clone(),
        restored_files: run.restored_files,
        skipped_files: run.skipped_files,
        conflict_files: run.conflict_files,
        destination: destination.to_string_lossy().into_owned(),
        error_code: run.error_code.clone(),
        backup_plan_id: run.backup_plan_id.map(|id| id.to_string()),
    }
}

fn parse_action(value: &str) -> Result<RecoveryAction, SessionControllerError> {
    match value {
        "reconstruct_pre_session" => Ok(RecoveryAction::ReconstructPreSession),
        "restore_single_file" => Ok(RecoveryAction::RestoreSingleFile),
        "in_place_restore" => Ok(RecoveryAction::InPlaceRestore),
        "git_recovery_worktree" => Ok(RecoveryAction::GitRecoveryWorktree),
        _ => Err(controller_internal(
            "recovery_action_invalid",
            "The requested recovery action is invalid.",
        )),
    }
}

fn action_name(action: RecoveryAction) -> &'static str {
    match action {
        RecoveryAction::ReconstructPreSession => "reconstruct_pre_session",
        RecoveryAction::RestoreSingleFile => "restore_single_file",
        RecoveryAction::InPlaceRestore => "in_place_restore",
        RecoveryAction::GitRecoveryWorktree => "git_recovery_worktree",
    }
}

fn parse_id(value: &str, kind: &str) -> Result<EntityId, SessionControllerError> {
    value
        .parse()
        .map_err(|_| controller_internal("invalid_id", format!("Invalid {kind} identifier.")))
}

fn parse_digest(value: &str) -> Result<[u8; 32], RecoveryError> {
    let bytes = hex::decode(value).map_err(|_| RecoveryError::NotRestorable(value.to_owned()))?;
    bytes
        .try_into()
        .map_err(|_| RecoveryError::NotRestorable(value.to_owned()))
}

fn map_recovery_error(error: RecoveryError, _execution: bool) -> SessionControllerError {
    let code = error_code(&error);
    SessionControllerError {
        code: code.to_owned(),
        message: error.to_string(),
        not_found: false,
    }
}

fn error_code(error: &RecoveryError) -> &'static str {
    match error {
        RecoveryError::Conflicts { .. } => "recovery_conflicts",
        RecoveryError::UnsafePath(_) => "recovery_unsafe_path",
        RecoveryError::HashMismatch(_) => "recovery_hash_mismatch",
        RecoveryError::PlanDigestMismatch => "recovery_plan_stale",
        RecoveryError::NotRestorable(_) => "recovery_not_restorable",
        RecoveryError::BackupTooLarge { .. } => "recovery_backup_too_large",
        RecoveryError::Git(_) => "recovery_git_failed",
        RecoveryError::Io { .. } | RecoveryError::Blob(_) | RecoveryError::Canonical(_) => {
            "recovery_io_failed"
        }
    }
}

fn controller_internal(
    code: &'static str,
    error: impl std::fmt::Display,
) -> SessionControllerError {
    SessionControllerError {
        code: code.to_owned(),
        message: error.to_string(),
        not_found: false,
    }
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
    use super::*;
    use agenttraceback_crypto::{BlobCipher, MasterKey};
    use agenttraceback_store::{SessionRecord, SnapshotFileVersion};

    #[tokio::test]
    async fn in_place_api_persists_a_retrievable_undo_plan() {
        let directory = tempfile::tempdir().expect("directory");
        let store = Store::open(
            directory.path().join("db"),
            directory.path().join("backups"),
        )
        .await
        .expect("store");
        let blobs = BlobStore::open(
            directory.path().join("blobs"),
            BlobCipher::from_master_key(&MasterKey::generate()).expect("cipher"),
        )
        .expect("blobs");
        let session_id = EntityId::new();
        store
            .create_session(SessionRecord {
                id: session_id,
                project_id: None,
                title_preview: None,
                state: "complete".to_owned(),
                started_at_us: 1,
                ended_at_us: Some(2),
                agent_name: None,
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
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let path = workspace.join("file.txt");
        std::fs::write(&path, b"uncommitted work").expect("current");
        let stored = blobs.put(b"historical").expect("blob");
        let plan = RecoveryPlan::in_place(
            session_id,
            "exact",
            workspace,
            &[SnapshotFileVersion {
                file_version_id: EntityId::new(),
                file_id: EntityId::new(),
                display_path: "file.txt".to_owned(),
                comparison_path: "file.txt".to_owned(),
                content_hash: Some(stored.descriptor.hex_digest.clone()),
                blob_id: Some(stored.descriptor.hex_digest),
                byte_length: Some(10),
                symlink_target: None,
                capture_status: "hashed".to_owned(),
                executable: false,
            }],
        )
        .expect("plan");
        let coordinator = RecoveryCoordinator::new(store.clone(), blobs);
        coordinator.persist_plan(&plan).await.expect("persist");
        let run = coordinator
            .execute_recovery(
                &plan.id.to_string(),
                ExecuteRecoveryRequest {
                    plan_digest: plan.plan_digest.clone(),
                    confirm: true,
                    overwrite_conflicts: false,
                },
            )
            .await
            .expect("restore");
        assert_eq!(std::fs::read(&path).expect("restored"), b"historical");
        let backup_id = run.backup_plan_id.expect("backup ID");
        let fetched_run = coordinator
            .get_recovery_run(&run.run_id)
            .await
            .expect("run");
        assert_eq!(
            fetched_run.backup_plan_id.as_deref(),
            Some(backup_id.as_str())
        );
        let backup = coordinator
            .get_recovery_plan(&backup_id)
            .await
            .expect("backup plan");
        coordinator
            .execute_recovery(
                &backup_id,
                ExecuteRecoveryRequest {
                    plan_digest: backup.plan_digest,
                    confirm: true,
                    overwrite_conflicts: false,
                },
            )
            .await
            .expect("undo");
        assert_eq!(std::fs::read(&path).expect("undone"), b"uncommitted work");
        store.close().await.expect("close");
    }
}
