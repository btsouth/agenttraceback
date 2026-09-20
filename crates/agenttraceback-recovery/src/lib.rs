//! Immutable recovery plans and conflict-safe restoration primitives.

use std::{
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
};

use agenttraceback_blobs::{BlobDescriptor, BlobStore};
use agenttraceback_crypto::canonical_digest;
use agenttraceback_store::SnapshotFileVersion;
use agenttraceback_types::EntityId;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

/// Recovery action requested by the user.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    /// Reconstruct the complete pre-session state into a new directory.
    ReconstructPreSession,
    /// Restore one file to a new path.
    RestoreSingleFile,
    /// Restore one or more files in place.
    InPlaceRestore,
    /// Materialize pre-session state into a Git recovery worktree.
    GitRecoveryWorktree,
}

/// Conflict behavior during execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictMode {
    /// Refuse to overwrite changed files.
    Refuse,
    /// Explicitly overwrite conflicts.
    Overwrite,
}

/// Recovery operation category.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryOperationKind {
    /// Write a regular file.
    WriteFile,
    /// Create a symlink without following it.
    CreateSymlink,
    /// Remove a file created by a prior restore when executing its undo plan.
    RemoveFile,
}

/// One planned recovery operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryOperation {
    /// Project-relative slash-separated path.
    pub relative_path: String,
    /// Operation category.
    pub kind: RecoveryOperationKind,
    /// Source file version.
    pub source_version_id: EntityId,
    /// Encrypted content blob.
    pub blob_id: Option<String>,
    /// Expected output BLAKE3 digest.
    pub expected_hash: Option<String>,
    /// Expected current digest for conflict checks.
    pub expected_current_hash: Option<String>,
    /// Expected content size.
    pub byte_length: Option<u64>,
    /// Executable-bit state.
    pub executable: bool,
    /// Symlink target.
    pub symlink_target: Option<String>,
}

/// One file excluded from exact recovery.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryExclusion {
    /// Project-relative path.
    pub relative_path: String,
    /// Explanation shown before execution.
    pub reason: String,
}

/// Immutable recovery plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryPlan {
    /// Plan identifier.
    pub id: EntityId,
    /// Source session.
    pub session_id: EntityId,
    /// Requested action.
    pub action: RecoveryAction,
    /// Destination root.
    pub destination: PathBuf,
    /// Source snapshot coverage.
    pub coverage: String,
    /// Operations in deterministic path order.
    pub operations: Vec<RecoveryOperation>,
    /// Files that cannot be restored exactly.
    pub exclusions: Vec<RecoveryExclusion>,
    /// SHA-256 digest of immutable plan content.
    pub plan_digest: String,
    /// Creation time in UTC microseconds.
    pub created_at_us: i64,
}

impl RecoveryPlan {
    /// Builds an exact pre-session reconstruction plan.
    pub fn reconstruct_pre_session(
        session_id: EntityId,
        coverage: impl Into<String>,
        destination: PathBuf,
        versions: &[SnapshotFileVersion],
    ) -> Result<Self, RecoveryError> {
        let mut operations = Vec::new();
        let mut exclusions = Vec::new();
        for version in versions {
            match version.capture_status.as_str() {
                "hashed" if version.blob_id.is_some() => {
                    operations.push(RecoveryOperation {
                        relative_path: version.comparison_path.clone(),
                        kind: RecoveryOperationKind::WriteFile,
                        source_version_id: version.file_version_id,
                        blob_id: version.blob_id.clone(),
                        expected_hash: version.content_hash.clone(),
                        expected_current_hash: None,
                        byte_length: version.byte_length,
                        executable: version.executable,
                        symlink_target: None,
                    });
                }
                "symlink" => operations.push(RecoveryOperation {
                    relative_path: version.comparison_path.clone(),
                    kind: RecoveryOperationKind::CreateSymlink,
                    source_version_id: version.file_version_id,
                    blob_id: None,
                    expected_hash: None,
                    expected_current_hash: None,
                    byte_length: None,
                    executable: false,
                    symlink_target: version.symlink_target.clone(),
                }),
                _ => exclusions.push(RecoveryExclusion {
                    relative_path: version.comparison_path.clone(),
                    reason: "content is metadata-only, sensitive, missing, or unsupported"
                        .to_owned(),
                }),
            }
        }
        operations.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        exclusions.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        finalize_plan(
            session_id,
            RecoveryAction::ReconstructPreSession,
            destination,
            coverage.into(),
            operations,
            exclusions,
        )
    }

    /// Builds a Git worktree recovery plan.
    pub fn git_recovery_worktree(
        session_id: EntityId,
        coverage: impl Into<String>,
        worktree_path: PathBuf,
        versions: &[SnapshotFileVersion],
    ) -> Result<Self, RecoveryError> {
        let provisional =
            Self::reconstruct_pre_session(session_id, coverage, worktree_path.clone(), versions)?;
        finalize_plan(
            session_id,
            RecoveryAction::GitRecoveryWorktree,
            worktree_path,
            provisional.coverage,
            provisional.operations,
            provisional.exclusions,
        )
    }

    /// Builds a single-file restore to a new path.
    pub fn restore_single_file(
        session_id: EntityId,
        coverage: impl Into<String>,
        destination_file: PathBuf,
        version: &SnapshotFileVersion,
    ) -> Result<Self, RecoveryError> {
        if version.capture_status != "hashed" || version.blob_id.is_none() {
            return Err(RecoveryError::NotRestorable(
                version.comparison_path.clone(),
            ));
        }
        let relative_path = destination_file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| RecoveryError::UnsafePath(destination_file.clone()))?
            .to_owned();
        let destination = destination_file
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| RecoveryError::UnsafePath(destination_file.clone()))?;
        finalize_plan(
            session_id,
            RecoveryAction::RestoreSingleFile,
            destination,
            coverage.into(),
            vec![RecoveryOperation {
                relative_path,
                kind: RecoveryOperationKind::WriteFile,
                source_version_id: version.file_version_id,
                blob_id: version.blob_id.clone(),
                expected_hash: version.content_hash.clone(),
                expected_current_hash: None,
                byte_length: version.byte_length,
                executable: version.executable,
                symlink_target: None,
            }],
            Vec::new(),
        )
    }

    /// Builds an in-place plan with current-state conflict hashes.
    pub fn in_place(
        session_id: EntityId,
        coverage: impl Into<String>,
        workspace_root: PathBuf,
        versions: &[SnapshotFileVersion],
    ) -> Result<Self, RecoveryError> {
        let mut operations = Vec::new();
        for version in versions {
            if version.capture_status != "hashed" || version.blob_id.is_none() {
                continue;
            }
            let current = workspace_root.join(&version.comparison_path);
            operations.push(RecoveryOperation {
                relative_path: version.comparison_path.clone(),
                kind: RecoveryOperationKind::WriteFile,
                source_version_id: version.file_version_id,
                blob_id: version.blob_id.clone(),
                expected_hash: version.content_hash.clone(),
                expected_current_hash: hash_path(&current)?,
                byte_length: version.byte_length,
                executable: version.executable,
                symlink_target: None,
            });
        }
        operations.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        finalize_plan(
            session_id,
            RecoveryAction::InPlaceRestore,
            workspace_root,
            coverage.into(),
            operations,
            Vec::new(),
        )
    }

    /// Verifies that the plan content has not changed since preparation.
    pub fn verify_digest(&self) -> Result<(), RecoveryError> {
        if plan_digest(self)? == self.plan_digest {
            Ok(())
        } else {
            Err(RecoveryError::PlanDigestMismatch)
        }
    }
}

/// Recovery execution summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryReport {
    /// Files written or linked.
    pub restored_files: u64,
    /// Files intentionally skipped.
    pub skipped_files: u64,
    /// Conflicts encountered.
    pub conflict_files: u64,
    /// Destination root.
    pub destination: PathBuf,
}

/// Recovery planning or execution failure.
#[derive(Debug, Error)]
pub enum RecoveryError {
    /// Filesystem operation failed.
    #[error("recovery filesystem operation failed at {path}: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// Blob read or authentication failed.
    #[error(transparent)]
    Blob(#[from] agenttraceback_blobs::BlobError),
    /// Canonical plan serialization failed.
    #[error(transparent)]
    Canonical(#[from] agenttraceback_crypto::ChainError),
    /// A path was absolute, empty, or contained parent traversal.
    #[error("unsafe recovery path: {0}")]
    UnsafePath(PathBuf),
    /// A source version has no restorable content.
    #[error("file version is not restorable: {0}")]
    NotRestorable(String),
    /// Current content is too large to back up before an in-place write.
    #[error(
        "current file {path} exceeds the {limit}-byte pre-restore backup limit; no restore was performed"
    )]
    BackupTooLarge {
        /// Current file that must be backed up first.
        path: PathBuf,
        /// Maximum supported backup size in bytes.
        limit: u64,
    },
    /// A prepared plan was changed.
    #[error("recovery plan digest does not match its contents")]
    PlanDigestMismatch,
    /// Current workspace state conflicts with the prepared plan.
    #[error("recovery conflicts require explicit overwrite: {paths:?}")]
    Conflicts {
        /// Conflicting paths.
        paths: Vec<String>,
    },
    /// A restored file did not match its recorded digest.
    #[error("restored file hash mismatch at {0}")]
    HashMismatch(String),
    /// Git recovery worktree creation failed.
    #[error("Git recovery worktree failed: {0}")]
    Git(String),
}

/// Captures every existing in-place target before any restore writes occur.
/// The returned plan and blob descriptors must be persisted before execution.
pub fn backup_before_restore(
    plan: &RecoveryPlan,
    blobs: &BlobStore,
    conflict_mode: ConflictMode,
) -> Result<(RecoveryPlan, Vec<BlobDescriptor>), RecoveryError> {
    plan.verify_digest()?;
    let conflicts = conflict_paths(plan, &plan.destination)?;
    if !conflicts.is_empty() && conflict_mode == ConflictMode::Refuse {
        return Err(RecoveryError::Conflicts { paths: conflicts });
    }
    let mut operations = Vec::new();
    let mut descriptors = Vec::new();
    for operation in &plan.operations {
        let path = safe_join(&plan.destination, &operation.relative_path)?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                operations.push(RecoveryOperation {
                    relative_path: operation.relative_path.clone(),
                    kind: RecoveryOperationKind::RemoveFile,
                    source_version_id: EntityId::new(),
                    blob_id: None,
                    expected_hash: None,
                    expected_current_hash: operation.expected_hash.clone(),
                    byte_length: None,
                    executable: false,
                    symlink_target: None,
                });
                continue;
            }
            Err(source) => return Err(RecoveryError::Io { path, source }),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(RecoveryError::UnsafePath(path));
        }
        const MAX_BACKUP_BYTES: u64 = 64 * 1024 * 1024;
        if metadata.len() > MAX_BACKUP_BYTES {
            return Err(RecoveryError::BackupTooLarge {
                path,
                limit: MAX_BACKUP_BYTES,
            });
        }
        let file = fs::File::open(&path).map_err(|source| RecoveryError::Io {
            path: path.clone(),
            source,
        })?;
        let mut bytes = Vec::new();
        file.take(MAX_BACKUP_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| RecoveryError::Io {
                path: path.clone(),
                source,
            })?;
        if bytes.len() as u64 > MAX_BACKUP_BYTES {
            return Err(RecoveryError::BackupTooLarge {
                path,
                limit: MAX_BACKUP_BYTES,
            });
        }
        let stored = blobs.put(&bytes)?;
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        operations.push(RecoveryOperation {
            relative_path: operation.relative_path.clone(),
            kind: RecoveryOperationKind::WriteFile,
            source_version_id: EntityId::new(),
            blob_id: Some(stored.descriptor.hex_digest.clone()),
            expected_hash: Some(stored.descriptor.hex_digest.clone()),
            expected_current_hash: operation.expected_hash.clone(),
            byte_length: Some(bytes.len() as u64),
            executable,
            symlink_target: None,
        });
        descriptors.push(stored.descriptor);
    }
    let backup = finalize_plan(
        plan.session_id,
        RecoveryAction::InPlaceRestore,
        plan.destination.clone(),
        "pre_restore_backup".to_owned(),
        operations,
        Vec::new(),
    )?;
    Ok((backup, descriptors))
}

/// Executes an in-place plan only while targets still match the persisted backup.
pub fn execute_with_backup_guard(
    plan: &RecoveryPlan,
    backup: &RecoveryPlan,
    blobs: &BlobStore,
) -> Result<RecoveryReport, RecoveryError> {
    plan.verify_digest()?;
    backup.verify_digest()?;
    if plan.destination != backup.destination || plan.session_id != backup.session_id {
        return Err(RecoveryError::PlanDigestMismatch);
    }
    let mut guarded = plan.clone();
    for operation in &mut guarded.operations {
        operation.expected_current_hash = backup
            .operations
            .iter()
            .find(|before| before.relative_path == operation.relative_path)
            .and_then(|before| before.expected_hash.clone());
    }
    guarded.plan_digest = plan_digest(&guarded)?;
    execute(&guarded, blobs, &guarded.destination, ConflictMode::Refuse)
}

/// Executes a plan against its destination or an explicit target root.
pub fn execute(
    plan: &RecoveryPlan,
    blobs: &BlobStore,
    target_root: &Path,
    conflict_mode: ConflictMode,
) -> Result<RecoveryReport, RecoveryError> {
    plan.verify_digest()?;
    let conflict_mode = if plan.action == RecoveryAction::InPlaceRestore {
        conflict_mode
    } else {
        ConflictMode::Refuse
    };
    prepare_destination(target_root)?;
    if plan.action == RecoveryAction::ReconstructPreSession
        && fs::read_dir(target_root)
            .map_err(|source| RecoveryError::Io {
                path: target_root.to_path_buf(),
                source,
            })?
            .next()
            .transpose()
            .map_err(|source| RecoveryError::Io {
                path: target_root.to_path_buf(),
                source,
            })?
            .is_some()
    {
        return Err(RecoveryError::Conflicts {
            paths: vec!["destination directory is not empty".to_owned()],
        });
    }
    let conflicts = conflict_paths(plan, target_root)?;
    if !conflicts.is_empty() && conflict_mode == ConflictMode::Refuse {
        return Err(RecoveryError::Conflicts { paths: conflicts });
    }
    for operation in &plan.operations {
        safe_join(target_root, &operation.relative_path)?;
        if operation.kind == RecoveryOperationKind::RemoveFile
            && plan.action != RecoveryAction::InPlaceRestore
        {
            return Err(RecoveryError::NotRestorable(
                operation.relative_path.clone(),
            ));
        }
        if operation.kind == RecoveryOperationKind::WriteFile {
            let digest =
                parse_digest(operation.blob_id.as_deref().ok_or_else(|| {
                    RecoveryError::NotRestorable(operation.relative_path.clone())
                })?)?;
            let bytes = blobs.get(&digest)?;
            if operation.expected_hash.as_deref() != Some(blake3::hash(&bytes).to_hex().as_str()) {
                return Err(RecoveryError::HashMismatch(operation.relative_path.clone()));
            }
        }
    }
    let mut restored_files = 0_u64;
    for operation in &plan.operations {
        let destination = safe_join(target_root, &operation.relative_path)?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|source| RecoveryError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        match operation.kind {
            RecoveryOperationKind::WriteFile => {
                let blob_id = operation
                    .blob_id
                    .as_deref()
                    .ok_or_else(|| RecoveryError::NotRestorable(operation.relative_path.clone()))?;
                let digest = parse_digest(blob_id)?;
                let bytes = blobs.get(&digest)?;
                if plan.action == RecoveryAction::InPlaceRestore
                    && conflict_mode == ConflictMode::Refuse
                    && operation_conflicts(operation, &destination)?
                {
                    return Err(RecoveryError::Conflicts {
                        paths: vec![operation.relative_path.clone()],
                    });
                }
                safe_join(target_root, &operation.relative_path)?;
                let replace_existing = plan.action == RecoveryAction::InPlaceRestore
                    || plan.action == RecoveryAction::GitRecoveryWorktree
                    || conflict_mode == ConflictMode::Overwrite;
                write_file(&destination, &bytes, operation.executable, replace_existing)?;
                if let Some(expected) = &operation.expected_hash
                    && hash_path(&destination)?.as_deref() != Some(expected)
                {
                    return Err(RecoveryError::HashMismatch(operation.relative_path.clone()));
                }
            }
            RecoveryOperationKind::RemoveFile => {
                if plan.action != RecoveryAction::InPlaceRestore {
                    return Err(RecoveryError::NotRestorable(
                        operation.relative_path.clone(),
                    ));
                }
                if conflict_mode == ConflictMode::Refuse
                    && operation_conflicts(operation, &destination)?
                {
                    return Err(RecoveryError::Conflicts {
                        paths: vec![operation.relative_path.clone()],
                    });
                }
                match fs::remove_file(&destination) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(RecoveryError::Io {
                            path: destination,
                            source,
                        });
                    }
                }
            }
            RecoveryOperationKind::CreateSymlink => {
                let target = operation
                    .symlink_target
                    .as_deref()
                    .ok_or_else(|| RecoveryError::NotRestorable(operation.relative_path.clone()))?;
                create_symlink(Path::new(target), &destination)?;
            }
        }
        restored_files = restored_files.saturating_add(1);
    }
    Ok(RecoveryReport {
        restored_files,
        skipped_files: plan.exclusions.len() as u64,
        conflict_files: conflicts.len() as u64,
        destination: target_root.to_path_buf(),
    })
}

/// Creates a detached Git worktree for recovery and returns its path.
pub fn create_git_recovery_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    head_oid: &str,
    branch_name: &str,
) -> Result<(), RecoveryError> {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.sshCommand=false",
            "-c",
            "protocol.file.allow=never",
        ])
        .arg("-C")
        .arg(repo_root)
        .arg("--no-optional-locks")
        .arg("worktree")
        .arg("add")
        .arg("--no-checkout")
        .arg("-b")
        .arg(branch_name)
        .arg("--")
        .arg(worktree_path)
        .arg(head_oid)
        .output()
        .map_err(|error| RecoveryError::Git(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(RecoveryError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

fn finalize_plan(
    session_id: EntityId,
    action: RecoveryAction,
    destination: PathBuf,
    coverage: String,
    operations: Vec<RecoveryOperation>,
    exclusions: Vec<RecoveryExclusion>,
) -> Result<RecoveryPlan, RecoveryError> {
    let mut plan = RecoveryPlan {
        id: EntityId::new(),
        session_id,
        action,
        destination,
        coverage,
        operations,
        exclusions,
        plan_digest: String::new(),
        created_at_us: current_time_us(),
    };
    plan.plan_digest = plan_digest(&plan)?;
    Ok(plan)
}

fn plan_digest(plan: &RecoveryPlan) -> Result<String, RecoveryError> {
    let input = PlanDigestInput {
        session_id: plan.session_id,
        action: plan.action,
        destination: &plan.destination,
        coverage: &plan.coverage,
        operations: &plan.operations,
        exclusions: &plan.exclusions,
    };
    Ok(canonical_digest(&input)?.to_hex())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanDigestInput<'a> {
    session_id: EntityId,
    action: RecoveryAction,
    destination: &'a Path,
    coverage: &'a str,
    operations: &'a [RecoveryOperation],
    exclusions: &'a [RecoveryExclusion],
}

// A partial restore may leave an undo target already in its desired state.
fn operation_conflicts(operation: &RecoveryOperation, path: &Path) -> Result<bool, RecoveryError> {
    let current = hash_path(path)?;
    if operation.kind == RecoveryOperationKind::RemoveFile && current.is_none() {
        return Ok(false);
    }
    if operation.kind == RecoveryOperationKind::WriteFile
        && current.is_some()
        && current == operation.expected_hash
    {
        return Ok(false);
    }
    Ok(current != operation.expected_current_hash)
}

fn conflict_paths(plan: &RecoveryPlan, target_root: &Path) -> Result<Vec<String>, RecoveryError> {
    if plan.action == RecoveryAction::GitRecoveryWorktree {
        return Ok(Vec::new());
    }
    let mut conflicts = Vec::new();
    for operation in &plan.operations {
        let path = safe_join(target_root, &operation.relative_path)?;
        let has_conflict = if plan.action == RecoveryAction::InPlaceRestore {
            operation_conflicts(operation, &path)?
        } else {
            match fs::symlink_metadata(&path) {
                Ok(_) => true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(source) => return Err(RecoveryError::Io { path, source }),
            }
        };
        if has_conflict {
            conflicts.push(operation.relative_path.clone());
        }
    }
    Ok(conflicts)
}

fn prepare_destination(root: &Path) -> Result<(), RecoveryError> {
    if root.exists() {
        if !root.is_dir() {
            return Err(RecoveryError::UnsafePath(root.to_path_buf()));
        }
    } else {
        fs::create_dir_all(root).map_err(|source| RecoveryError::Io {
            path: root.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, RecoveryError> {
    let normalized = relative.replace('\\', "/");
    let relative_path = Path::new(&normalized);
    if normalized.is_empty()
        || relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(RecoveryError::UnsafePath(relative_path.to_path_buf()));
    }
    let destination = root.join(relative_path);
    let canonical_root = fs::canonicalize(root).map_err(|source| RecoveryError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let mut ancestor = destination.as_path();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| RecoveryError::UnsafePath(destination.clone()))?;
    }
    let canonical_ancestor = fs::canonicalize(ancestor).map_err(|source| RecoveryError::Io {
        path: ancestor.to_path_buf(),
        source,
    })?;
    if !canonical_ancestor.starts_with(&canonical_root) {
        return Err(RecoveryError::UnsafePath(destination));
    }
    Ok(destination)
}

fn write_file(
    path: &Path,
    bytes: &[u8],
    executable: bool,
    replace_existing: bool,
) -> Result<(), RecoveryError> {
    let parent = path
        .parent()
        .ok_or_else(|| RecoveryError::UnsafePath(path.to_path_buf()))?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| RecoveryError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    temporary
        .write_all(bytes)
        .and_then(|_| temporary.as_file_mut().sync_all())
        .map_err(|source| RecoveryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let result = if replace_existing {
        temporary.persist(path)
    } else {
        temporary.persist_noclobber(path)
    };
    result.map_err(|error| RecoveryError::Io {
        path: path.to_path_buf(),
        source: error.error,
    })?;
    set_executable(path, executable)
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> Result<(), RecoveryError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|source| {
        RecoveryError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> Result<(), RecoveryError> {
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, path: &Path) -> Result<(), RecoveryError> {
    std::os::unix::fs::symlink(target, path).map_err(|source| RecoveryError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(windows)]
fn create_symlink(target: &Path, path: &Path) -> Result<(), RecoveryError> {
    std::os::windows::fs::symlink_file(target, path).map_err(|source| RecoveryError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_digest(value: &str) -> Result<[u8; 32], RecoveryError> {
    let bytes = hex::decode(value).map_err(|_| RecoveryError::NotRestorable(value.to_owned()))?;
    bytes
        .try_into()
        .map_err(|_| RecoveryError::NotRestorable(value.to_owned()))
}

fn hash_path(path: &Path) -> Result<Option<String>, RecoveryError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(blake3::hash(&bytes).to_hex().to_string())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(RecoveryError::Io {
            path: path.to_path_buf(),
            source,
        }),
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

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use agenttraceback_blobs::BlobStore;
    use agenttraceback_crypto::{BlobCipher, MasterKey};
    use agenttraceback_store::SnapshotFileVersion;
    use agenttraceback_types::EntityId;

    use super::{
        ConflictMode, RecoveryAction, RecoveryError, RecoveryOperation, RecoveryOperationKind,
        RecoveryPlan, create_git_recovery_worktree, execute,
    };

    fn blob_store() -> (tempfile::TempDir, BlobStore) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let cipher = BlobCipher::from_master_key(&MasterKey::generate()).expect("cipher");
        let blobs = BlobStore::open(directory.path(), cipher).expect("blobs");
        (directory, blobs)
    }

    fn version(blobs: &BlobStore, path: &str, bytes: &[u8]) -> SnapshotFileVersion {
        let stored = blobs.put(bytes).expect("blob");
        SnapshotFileVersion {
            file_version_id: EntityId::new(),
            file_id: EntityId::new(),
            display_path: path.to_owned(),
            comparison_path: path.to_owned(),
            content_hash: Some(hex::encode(stored.descriptor.digest)),
            blob_id: Some(stored.descriptor.hex_digest),
            byte_length: Some(bytes.len() as u64),
            symlink_target: None,
            capture_status: "hashed".to_owned(),
            executable: false,
        }
    }

    #[test]
    fn reconstructs_dirty_and_untracked_files_exactly() {
        let (_blob_directory, blobs) = blob_store();
        let destination = tempfile::tempdir().expect("destination");
        let versions = vec![
            version(&blobs, "src/dirty-file.txt", b"dirty before"),
            version(&blobs, "untracked.txt", b"untracked before"),
        ];
        let plan = RecoveryPlan::reconstruct_pre_session(
            EntityId::new(),
            "exact",
            destination.path().to_path_buf(),
            &versions,
        )
        .expect("plan");
        let report =
            execute(&plan, &blobs, destination.path(), ConflictMode::Refuse).expect("execute");
        assert_eq!(report.restored_files, 2);
        assert_eq!(
            fs::read(destination.path().join("src/dirty-file.txt")).expect("dirty"),
            b"dirty before"
        );
        assert_eq!(
            fs::read(destination.path().join("untracked.txt")).expect("untracked"),
            b"untracked before"
        );
    }

    #[test]
    fn symlink_parent_traversal_is_rejected() {
        let (_blob_directory, blobs) = blob_store();
        let destination = tempfile::tempdir().expect("destination");
        let mut plan = RecoveryPlan::reconstruct_pre_session(
            EntityId::new(),
            "exact",
            destination.path().to_path_buf(),
            &[],
        )
        .expect("plan");
        plan.operations.push(RecoveryOperation {
            relative_path: "../escape".to_owned(),
            kind: RecoveryOperationKind::CreateSymlink,
            source_version_id: EntityId::new(),
            blob_id: None,
            expected_hash: None,
            expected_current_hash: None,
            byte_length: None,
            executable: false,
            symlink_target: Some("../../outside".to_owned()),
        });
        plan.plan_digest = super::plan_digest(&plan).expect("digest");
        let result = execute(&plan, &blobs, destination.path(), ConflictMode::Refuse);
        assert!(matches!(result, Err(RecoveryError::UnsafePath(_))));
    }

    #[test]
    fn in_place_restore_refuses_changed_file() {
        let (_blob_directory, blobs) = blob_store();
        let workspace = tempfile::tempdir().expect("workspace");
        let path = workspace.path().join("file.txt");
        fs::write(&path, "before").expect("before");
        let mut version = version(&blobs, "file.txt", b"before");
        version.content_hash = Some(blake3::hash(b"before").to_hex().to_string());
        let plan = RecoveryPlan::in_place(
            EntityId::new(),
            "exact",
            workspace.path().to_path_buf(),
            &[version],
        )
        .expect("plan");
        fs::write(&path, "after").expect("user change");
        let result = execute(&plan, &blobs, workspace.path(), ConflictMode::Refuse);
        assert!(matches!(result, Err(RecoveryError::Conflicts { .. })));
        assert_eq!(fs::read(&path).expect("unchanged"), b"after");
    }

    #[test]
    fn reconstruction_refuses_existing_file() {
        let (_blob_directory, blobs) = blob_store();
        let destination = tempfile::tempdir().expect("destination");
        let path = destination.path().join("file.txt");
        fs::write(&path, b"user content").expect("existing file");
        let plan = RecoveryPlan::reconstruct_pre_session(
            EntityId::new(),
            "exact",
            destination.path().to_path_buf(),
            &[version(&blobs, "file.txt", b"snapshot content")],
        )
        .expect("plan");
        let result = execute(&plan, &blobs, destination.path(), ConflictMode::Refuse);
        assert!(matches!(result, Err(RecoveryError::Conflicts { .. })));
        assert_eq!(fs::read(path).expect("preserved file"), b"user content");
    }

    #[test]
    fn single_file_restore_refuses_existing_file() {
        let (_blob_directory, blobs) = blob_store();
        let destination = tempfile::tempdir().expect("destination");
        let path = destination.path().join("file.txt");
        fs::write(&path, b"user content").expect("existing file");
        let plan = RecoveryPlan::restore_single_file(
            EntityId::new(),
            "exact",
            path.clone(),
            &version(&blobs, "source.txt", b"snapshot content"),
        )
        .expect("plan");
        let result = execute(&plan, &blobs, destination.path(), ConflictMode::Refuse);
        assert!(matches!(result, Err(RecoveryError::Conflicts { .. })));
        assert_eq!(fs::read(path).expect("preserved file"), b"user content");
    }

    #[test]
    fn in_place_restore_refuses_file_deleted_after_planning() {
        let (_blob_directory, blobs) = blob_store();
        let workspace = tempfile::tempdir().expect("workspace");
        let path = workspace.path().join("file.txt");
        fs::write(&path, b"current content").expect("existing file");
        let plan = RecoveryPlan::in_place(
            EntityId::new(),
            "exact",
            workspace.path().to_path_buf(),
            &[version(&blobs, "file.txt", b"snapshot content")],
        )
        .expect("plan");
        fs::remove_file(&path).expect("user deletes file");
        let result = execute(&plan, &blobs, workspace.path(), ConflictMode::Refuse);
        assert!(matches!(result, Err(RecoveryError::Conflicts { .. })));
        assert!(!path.exists());
    }

    #[test]
    fn plan_actions_are_stable() {
        let plan = RecoveryPlan::reconstruct_pre_session(
            EntityId::new(),
            "exact",
            PathBuf::from("/tmp/recovery"),
            &[],
        )
        .expect("plan");
        assert_eq!(plan.action, RecoveryAction::ReconstructPreSession);
        plan.verify_digest().expect("digest");
    }

    #[test]
    fn encrypted_backup_can_undo_in_place_restore() {
        let (_blob_directory, blobs) = blob_store();
        let workspace = tempfile::tempdir().expect("workspace");
        let path = workspace.path().join("file.txt");
        fs::write(&path, b"uncommitted work").expect("current");
        let plan = RecoveryPlan::in_place(
            EntityId::new(),
            "exact",
            workspace.path().to_path_buf(),
            &[
                version(&blobs, "file.txt", b"historical content"),
                version(&blobs, "absent.txt", b"previously deleted"),
            ],
        )
        .expect("plan");
        let (backup, descriptors) =
            super::backup_before_restore(&plan, &blobs, ConflictMode::Refuse).expect("backup");
        assert_eq!(descriptors.len(), 1);
        execute(&plan, &blobs, workspace.path(), ConflictMode::Refuse).expect("restore");
        assert_eq!(fs::read(&path).expect("restored"), b"historical content");
        assert!(workspace.path().join("absent.txt").exists());
        fs::write(workspace.path().join("absent.txt"), b"new user edit").expect("new edit");
        assert!(execute(&backup, &blobs, workspace.path(), ConflictMode::Refuse).is_err());
        assert_eq!(
            fs::read(&path).expect("no partial undo"),
            b"historical content"
        );
        fs::write(workspace.path().join("absent.txt"), b"previously deleted")
            .expect("reset fixture");
        execute(&backup, &blobs, workspace.path(), ConflictMode::Refuse).expect("undo");
        assert!(!workspace.path().join("absent.txt").exists());
        assert_eq!(fs::read(&path).expect("undone"), b"uncommitted work");
    }

    #[test]
    fn undo_partial_restore_accepts_files_never_created() {
        let (_blob_directory, blobs) = blob_store();
        let workspace = tempfile::tempdir().expect("workspace");
        let path = workspace.path().join("file.txt");
        fs::write(&path, b"uncommitted work").expect("current");
        fs::write(workspace.path().join("untouched.txt"), b"untouched work").expect("current");
        let plan = RecoveryPlan::in_place(
            EntityId::new(),
            "exact",
            workspace.path().to_path_buf(),
            &[
                version(&blobs, "file.txt", b"historical content"),
                version(&blobs, "absent.txt", b"previously deleted"),
                version(&blobs, "untouched.txt", b"old untouched"),
            ],
        )
        .expect("plan");
        let (backup, _) =
            super::backup_before_restore(&plan, &blobs, ConflictMode::Refuse).expect("backup");
        // Simulate interruption after replacing one file, before creating the other.
        fs::write(&path, b"historical content").expect("partial restore");
        execute(&backup, &blobs, workspace.path(), ConflictMode::Refuse).expect("undo");
        assert_eq!(fs::read(&path).expect("undone"), b"uncommitted work");
        assert!(!workspace.path().join("absent.txt").exists());
        assert_eq!(
            fs::read(workspace.path().join("untouched.txt")).expect("untouched"),
            b"untouched work"
        );
    }

    #[test]
    fn invalid_later_blob_does_not_modify_earlier_file() {
        let (_blob_directory, blobs) = blob_store();
        let workspace = tempfile::tempdir().expect("workspace");
        fs::write(workspace.path().join("a.txt"), b"current").expect("current");
        let mut bad = version(&blobs, "z.txt", b"wrong");
        bad.content_hash = Some(blake3::hash(b"expected").to_hex().to_string());
        let plan = RecoveryPlan::in_place(
            EntityId::new(),
            "exact",
            workspace.path().to_path_buf(),
            &[version(&blobs, "a.txt", b"historical"), bad],
        )
        .expect("plan");
        assert!(execute(&plan, &blobs, workspace.path(), ConflictMode::Refuse).is_err());
        assert_eq!(
            fs::read(workspace.path().join("a.txt")).expect("preserved"),
            b"current"
        );
    }

    #[test]
    fn git_worktree_helper_uses_structured_argv() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let repo = directory.path().join("repo");
        fs::create_dir_all(&repo).expect("repo");
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .expect("git init")
                .success()
        );
        fs::write(repo.join("file.txt"), "content").expect("file");
        assert!(
            std::process::Command::new("git")
                .args(["add", "."])
                .current_dir(&repo)
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "-qm", "fixture"])
                .current_dir(&repo)
                .env("GIT_AUTHOR_NAME", "fixture")
                .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
                .env("GIT_COMMITTER_NAME", "fixture")
                .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
                .status()
                .expect("git commit")
                .success()
        );
        let head = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo)
                .output()
                .expect("git head")
                .stdout,
        )
        .expect("utf8 head");
        let worktree = directory.path().join("recovery");
        create_git_recovery_worktree(
            &repo,
            &worktree,
            head.trim(),
            "agenttraceback/recovery/test",
        )
        .expect("worktree");
        assert!(!worktree.join("file.txt").exists());
        assert!(worktree.join(".git").is_file());
    }
}
