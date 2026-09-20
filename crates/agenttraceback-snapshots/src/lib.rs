//! Pre/post project manifests, baseline capture, and Git state integration.

use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use agenttraceback_crypto::canonical_digest;
use agenttraceback_projects::{IgnoreSource, ProjectError, ProjectScope};
use agenttraceback_risk::RiskEngine;
use agenttraceback_types::EntityId;
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use thiserror::Error;
use walkdir::WalkDir;

/// Baseline manifest capture errors.
#[derive(Debug, Error)]
pub enum SnapshotError {
    /// File, directory, or Git metadata operation failed.
    #[error("snapshot operation failed at {path}: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// Ignore rules could not be loaded or compiled.
    #[error("ignore rules could not be loaded: {0}")]
    Ignore(#[source] ProjectError),
    /// Git could not be invoked.
    #[error("Git command failed during snapshot: {0}")]
    Git(String),
    /// Canonical manifest hashing failed.
    #[error(transparent)]
    Canonical(#[from] agenttraceback_crypto::ChainError),
    /// The scan exceeded its file-count bound.
    #[error("project scan exceeded the {limit} file limit")]
    TooManyFiles {
        /// Configured bound.
        limit: usize,
    },
}

/// Baseline completeness for recovery eligibility.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestCoverage {
    /// Eligible files were scanned and hashed.
    Exact,
    /// Metadata was captured without restorable content.
    MetadataOnly,
    /// Some roots or files could not be inspected.
    Partial,
}

/// Capture state for one manifest entry.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    /// Regular file content was hashed and is eligible for later content capture.
    Hashed,
    /// Symlink target was captured without following the link.
    Symlink,
    /// Metadata and hash were captured, but content exceeded the capture limit.
    MetadataOnly,
    /// Entry could not be read.
    Error,
}

/// One file or symlink in a project manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    /// Stable project-relative display path.
    pub display_path: String,
    /// Platform-normalized comparison path.
    pub comparison_path: String,
    /// BLAKE3 content digest when capturable.
    pub content_hash: Option<String>,
    /// File size in bytes.
    pub byte_length: Option<u64>,
    /// Last modification time in UTC microseconds.
    pub modified_at_us: Option<i64>,
    /// Whether an executable bit was set.
    pub executable: bool,
    /// Symlink target, never followed for capture.
    pub symlink_target: Option<String>,
    /// Capture state.
    pub capture_status: CaptureStatus,
    /// Redacted diagnostic code when capture failed.
    pub diagnostic_code: Option<String>,
    /// Sensitive class assigned by the path policy.
    pub sensitive_class: Option<String>,
}

/// A deterministic project baseline manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectManifest {
    /// Manifest identifier.
    pub id: EntityId,
    /// Session identifier when attached to a run.
    pub session_id: Option<EntityId>,
    /// Manifest completeness.
    pub coverage: ManifestCoverage,
    /// Capture limits used for this manifest.
    pub limits: ManifestLimits,
    /// Sorted manifest entries.
    pub entries: Vec<ManifestEntry>,
    /// SHA-256 canonical digest of the manifest payload.
    pub manifest_hash: String,
    /// Capture time in UTC microseconds.
    pub captured_at_us: i64,
}

/// Scan and capture limits.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestLimits {
    /// Maximum bytes for one content capture.
    pub max_file_bytes: u64,
    /// Maximum candidate entries in one scan.
    pub max_scan_files: usize,
}

impl Default for ManifestLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 20 * 1024 * 1024,
            max_scan_files: 250_000,
        }
    }
}

/// A deterministic change between two manifests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestChangeKind {
    /// A path exists only in the new manifest.
    Created,
    /// A path exists in both manifests with changed metadata or content hash.
    Modified,
    /// A path exists only in the previous manifest.
    Deleted,
}

/// Structured file diff result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileDiff {
    /// UTF-8 text changes.
    Text {
        /// Changed lines.
        lines: Vec<DiffLine>,
        /// Whether the diff exceeded the display bound.
        truncated: bool,
    },
    /// Binary metadata instead of content.
    Binary {
        /// Before byte length.
        before_bytes: u64,
        /// After byte length.
        after_bytes: u64,
        /// Before digest.
        before_hash: String,
        /// After digest.
        after_hash: String,
    },
}

/// One changed text line.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    /// Change tag: equal, insert, or delete.
    pub tag: String,
    /// Original line index.
    pub old_index: Option<usize>,
    /// New line index.
    pub new_index: Option<usize>,
    /// Line content without the trailing newline.
    pub content: String,
}

/// Computes a bounded structured diff without executing file content.
#[must_use]
pub fn structured_diff(before: &[u8], after: &[u8]) -> FileDiff {
    let Ok(before_text) = std::str::from_utf8(before) else {
        return binary_diff(before, after);
    };
    let Ok(after_text) = std::str::from_utf8(after) else {
        return binary_diff(before, after);
    };
    if before.contains(&0) || after.contains(&0) {
        return binary_diff(before, after);
    }
    let diff = TextDiff::from_lines(before_text, after_text);
    let mut lines = Vec::new();
    let mut truncated = false;
    const MAX_DIFF_LINES: usize = 10_000;
    for change in diff.iter_all_changes() {
        if lines.len() >= MAX_DIFF_LINES {
            truncated = true;
            break;
        }
        let tag = match change.tag() {
            ChangeTag::Delete => "delete",
            ChangeTag::Insert => "insert",
            ChangeTag::Equal => "equal",
        };
        lines.push(DiffLine {
            tag: tag.to_owned(),
            old_index: change.old_index(),
            new_index: change.new_index(),
            content: change.value().trim_end_matches(['\r', '\n']).to_owned(),
        });
    }
    FileDiff::Text { lines, truncated }
}

fn binary_diff(before: &[u8], after: &[u8]) -> FileDiff {
    FileDiff::Binary {
        before_bytes: before.len() as u64,
        after_bytes: after.len() as u64,
        before_hash: blake3::hash(before).to_hex().to_string(),
        after_hash: blake3::hash(after).to_hex().to_string(),
    }
}

/// One reconciliation change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestChange {
    /// Change category.
    pub kind: ManifestChangeKind,
    /// Previous entry when present.
    pub before: Option<ManifestEntry>,
    /// Current entry when present.
    pub after: Option<ManifestEntry>,
}

impl ProjectManifest {
    /// Creates an empty manifest for an explicit metadata-only fast path.
    pub fn empty(
        session_id: Option<EntityId>,
        coverage: ManifestCoverage,
    ) -> Result<Self, SnapshotError> {
        let limits = ManifestLimits::default();
        let entries = Vec::new();
        let digest_input = ManifestDigestInput {
            coverage,
            limits,
            entries: entries.clone(),
        };
        Ok(Self {
            id: EntityId::new(),
            session_id,
            coverage,
            limits,
            entries,
            manifest_hash: canonical_digest(&digest_input)?.to_hex(),
            captured_at_us: current_time_us(),
        })
    }

    /// Builds a baseline without spawning the recorded command.
    pub fn capture(
        scope: &ProjectScope,
        session_id: Option<EntityId>,
        limits: ManifestLimits,
    ) -> Result<Self, SnapshotError> {
        let ignore = scope.load_ignore_engine().map_err(SnapshotError::Ignore)?;
        let tracked = tracked_files(scope)?;
        let mut entries = Vec::new();
        let mut coverage = ManifestCoverage::Exact;

        for entry in WalkDir::new(&scope.canonical_root)
            .follow_links(false)
            .into_iter()
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    coverage = ManifestCoverage::Partial;
                    continue;
                }
            };
            if entry.path() == scope.canonical_root {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&scope.canonical_root)
                .map_err(|_| SnapshotError::Io {
                    path: entry.path().to_path_buf(),
                    source: io::Error::new(io::ErrorKind::InvalidInput, "path escaped root"),
                })?;
            let display_path = relative.to_string_lossy().replace('\\', "/");
            let comparison_path = scope
                .relative_comparison_path(entry.path())
                .unwrap_or_else(|_| display_path.clone());
            let decision = ignore.check(&comparison_path);
            let is_tracked = tracked.contains(&comparison_path);
            if decision.ignored {
                let overridden = decision.source == Some(IgnoreSource::Default) && is_tracked;
                if !overridden {
                    if entry.file_type().is_dir() {
                        continue;
                    }
                    continue;
                }
            }
            if entry.file_type().is_dir() {
                continue;
            }
            if entries.len() >= limits.max_scan_files {
                return Err(SnapshotError::TooManyFiles {
                    limit: limits.max_scan_files,
                });
            }
            entries.push(capture_entry(
                entry.path(),
                &display_path,
                &comparison_path,
                limits,
            )?);
        }
        entries.sort_by(|left, right| left.comparison_path.cmp(&right.comparison_path));
        let digest_input = ManifestDigestInput {
            coverage,
            limits,
            entries: entries.clone(),
        };
        let manifest_hash = canonical_digest(&digest_input)?.to_hex();
        Ok(Self {
            id: EntityId::new(),
            session_id,
            coverage,
            limits,
            entries,
            manifest_hash,
            captured_at_us: current_time_us(),
        })
    }

    /// Compares this manifest to a newer scan.
    #[must_use]
    pub fn diff(&self, current: &Self) -> Vec<ManifestChange> {
        let before: BTreeMap<_, _> = self
            .entries
            .iter()
            .cloned()
            .map(|entry| (entry.comparison_path.clone(), entry))
            .collect();
        let after: BTreeMap<_, _> = current
            .entries
            .iter()
            .cloned()
            .map(|entry| (entry.comparison_path.clone(), entry))
            .collect();
        let mut changes = Vec::new();
        for (path, before_entry) in &before {
            match after.get(path) {
                None => changes.push(ManifestChange {
                    kind: ManifestChangeKind::Deleted,
                    before: Some(before_entry.clone()),
                    after: None,
                }),
                Some(after_entry) if after_entry != before_entry => {
                    changes.push(ManifestChange {
                        kind: ManifestChangeKind::Modified,
                        before: Some(before_entry.clone()),
                        after: Some(after_entry.clone()),
                    });
                }
                Some(_) => {}
            }
        }
        for (path, after_entry) in after {
            if !before.contains_key(&path) {
                changes.push(ManifestChange {
                    kind: ManifestChangeKind::Created,
                    before: None,
                    after: Some(after_entry),
                });
            }
        }
        changes
    }
}

impl ManifestCoverage {
    /// Returns the stable API/storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::MetadataOnly => "metadata_only",
            Self::Partial => "partial",
        }
    }
}

impl CaptureStatus {
    /// Returns the stable API/storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hashed => "hashed",
            Self::Symlink => "symlink",
            Self::MetadataOnly => "metadata_only",
            Self::Error => "error",
        }
    }
}

/// Captures one path relative to a project without checking ignore rules.
pub fn capture_manifest_entry(
    scope: &ProjectScope,
    path: &Path,
    limits: ManifestLimits,
) -> Result<ManifestEntry, SnapshotError> {
    let display_path = path
        .strip_prefix(&scope.canonical_root)
        .map_err(|_| SnapshotError::Io {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "path escaped root"),
        })?
        .to_string_lossy()
        .replace('\\', "/");
    let comparison_path =
        scope
            .relative_comparison_path(path)
            .map_err(|error| SnapshotError::Io {
                path: path.to_path_buf(),
                source: io::Error::new(io::ErrorKind::InvalidInput, error.to_string()),
            })?;
    capture_entry(path, &display_path, &comparison_path, limits)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestDigestInput {
    coverage: ManifestCoverage,
    limits: ManifestLimits,
    entries: Vec<ManifestEntry>,
}

fn capture_entry(
    path: &Path,
    display_path: &str,
    comparison_path: &str,
    limits: ManifestLimits,
) -> Result<ManifestEntry, SnapshotError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let modified_at_us = metadata.modified().ok().and_then(system_time_us);
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(|source| SnapshotError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        return Ok(ManifestEntry {
            display_path: display_path.to_owned(),
            comparison_path: comparison_path.to_owned(),
            content_hash: None,
            byte_length: None,
            modified_at_us,
            executable: false,
            symlink_target: Some(target.to_string_lossy().into_owned()),
            capture_status: CaptureStatus::Symlink,
            diagnostic_code: None,
            sensitive_class: None,
        });
    }
    let status = if metadata.len() > limits.max_file_bytes {
        CaptureStatus::MetadataOnly
    } else {
        CaptureStatus::Hashed
    };
    let content_hash = match hash_file(path) {
        Ok(hash) => Some(hash),
        Err(_) => {
            return Ok(ManifestEntry {
                display_path: display_path.to_owned(),
                comparison_path: comparison_path.to_owned(),
                content_hash: None,
                byte_length: Some(metadata.len()),
                modified_at_us,
                executable: is_executable(&metadata),
                symlink_target: None,
                capture_status: CaptureStatus::Error,
                diagnostic_code: Some("file_read_failed".to_owned()),
                sensitive_class: None,
            });
        }
    };
    Ok(ManifestEntry {
        display_path: display_path.to_owned(),
        comparison_path: comparison_path.to_owned(),
        content_hash,
        byte_length: Some(metadata.len()),
        modified_at_us,
        executable: is_executable(&metadata),
        symlink_target: None,
        capture_status: status,
        diagnostic_code: None,
        sensitive_class: RiskEngine::default()
            .sensitive_class(Path::new(comparison_path))
            .map(|class| class.as_str().to_owned()),
    })
}

fn hash_file(path: &Path) -> Result<String, SnapshotError> {
    let mut file = File::open(path).map_err(|source| SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = blake3::Hasher::new();
    io::copy(&mut file, &mut hasher).map_err(|source| SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(hasher.finalize().to_hex().to_string())
}

fn tracked_files(scope: &ProjectScope) -> Result<HashSet<String>, SnapshotError> {
    if scope.vcs_kind != agenttraceback_projects::VcsKind::Git {
        return Ok(HashSet::new());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(&scope.canonical_root)
        .arg("--no-optional-locks")
        .args(["ls-files", "-z"])
        .output()
        .map_err(|error| SnapshotError::Git(error.to_string()))?;
    if !output.status.success() {
        return Err(SnapshotError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let display = String::from_utf8_lossy(value).replace('\\', "/");
            if scope.case_insensitive {
                display.to_lowercase()
            } else {
                display
            }
        })
        .collect())
}

fn is_executable(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

fn system_time_us(value: SystemTime) -> Option<i64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| duration.as_micros().try_into().ok())
}

fn current_time_us() -> i64 {
    system_time_us(SystemTime::now()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use agenttraceback_projects::ProjectScope;

    use super::{FileDiff, ManifestChangeKind, ManifestLimits, ProjectManifest, structured_diff};

    #[test]
    fn manifest_applies_explicit_exclusions_and_detects_changes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(directory.path().join("keep.txt"), "one").expect("fixture");
        fs::write(
            directory.path().join(".agenttracebackignore"),
            "ignored/**\n",
        )
        .expect("ignore file");
        fs::create_dir_all(directory.path().join("ignored")).expect("ignored directory");
        fs::write(directory.path().join("ignored/secret.txt"), "secret").expect("ignored fixture");

        let scope = ProjectScope::discover(directory.path()).expect("project");
        let before =
            ProjectManifest::capture(&scope, None, ManifestLimits::default()).expect("baseline");
        assert!(
            before
                .entries
                .iter()
                .any(|entry| entry.display_path == "keep.txt")
        );
        assert!(
            !before
                .entries
                .iter()
                .any(|entry| entry.display_path.starts_with("ignored/"))
        );

        fs::write(directory.path().join("keep.txt"), "two").expect("modify");
        fs::write(directory.path().join("new.txt"), "new").expect("create");
        fs::remove_file(directory.path().join("keep.txt")).expect("delete");
        let after = ProjectManifest::capture(&scope, None, ManifestLimits::default())
            .expect("reconciliation");
        let changes = before.diff(&after);
        assert!(
            changes
                .iter()
                .any(|change| change.kind == ManifestChangeKind::Created)
        );
        assert!(
            changes
                .iter()
                .any(|change| change.kind == ManifestChangeKind::Deleted)
        );
    }

    #[test]
    fn large_file_is_hashed_but_marked_metadata_only() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(directory.path().join("large.bin"), vec![1_u8; 16]).expect("fixture");
        let scope = ProjectScope::discover(directory.path()).expect("project");
        let manifest = ProjectManifest::capture(
            &scope,
            None,
            ManifestLimits {
                max_file_bytes: 8,
                ..ManifestLimits::default()
            },
        )
        .expect("manifest");
        let entry = manifest
            .entries
            .iter()
            .find(|entry| entry.display_path == "large.bin")
            .expect("large entry");
        assert!(entry.content_hash.is_some());
        assert_eq!(entry.capture_status, super::CaptureStatus::MetadataOnly);
    }

    #[test]
    fn produces_text_diff_and_binary_metadata() {
        let text = structured_diff(b"one\ntwo\n", b"one\nthree\n");
        let FileDiff::Text { lines, .. } = text else {
            panic!("expected text diff");
        };
        assert!(
            lines
                .iter()
                .any(|line| line.tag == "delete" && line.content == "two")
        );
        assert!(
            lines
                .iter()
                .any(|line| line.tag == "insert" && line.content == "three")
        );

        let binary = structured_diff(&[0, 1, 2], &[0, 1, 3]);
        assert!(matches!(binary, FileDiff::Binary { .. }));
    }
}
