//! Project discovery, ignore rules, Git discovery, and path containment.

mod git;
mod ignore;

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use agenttraceback_types::EntityId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use git::{GitError, GitStateSnapshot, capture_git_state, git_root};
pub use ignore::{IgnoreDecision, IgnoreEngine, IgnoreSource};

/// Project discovery and path errors.
#[derive(Debug, Error)]
pub enum ProjectError {
    /// A path could not be canonicalized or read.
    #[error("project path operation failed at {path}: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The project root was not a directory.
    #[error("project root is not a directory: {0}")]
    NotDirectory(PathBuf),
    /// A path escaped the project root.
    #[error("path escapes the registered project root")]
    OutsideProject,
    /// Git discovery failed unexpectedly.
    #[error(transparent)]
    Git(#[from] GitError),
}

/// Version-control kind detected for a project.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VcsKind {
    /// Git repository.
    Git,
    /// Directory without recognized version control.
    None,
}

/// A registered project root and its normalized comparison identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectScope {
    /// Stable project identifier.
    pub id: EntityId,
    /// Human-readable project name.
    pub display_name: String,
    /// Canonical filesystem root.
    pub canonical_root: PathBuf,
    /// Normalized comparison path.
    pub comparison_root: String,
    /// Detected version-control kind.
    pub vcs_kind: VcsKind,
    /// Git worktree root when detected.
    pub git_root: Option<PathBuf>,
    /// Path comparison is case-insensitive on this platform.
    pub case_insensitive: bool,
}

impl ProjectScope {
    /// Discovers and canonicalizes a project from any path inside it.
    pub fn discover(path: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let requested = canonicalize_existing(path.as_ref())?;
        let root = if requested.is_dir() {
            requested
        } else {
            requested
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| ProjectError::NotDirectory(requested.clone()))?
        };
        let root = git_root(&root)?.unwrap_or(root);
        let display_name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project")
            .to_owned();
        let comparison_root = comparison_path(&root);
        Ok(Self {
            id: EntityId::new(),
            display_name,
            canonical_root: root.clone(),
            comparison_root,
            vcs_kind: if root.join(".git").exists() {
                VcsKind::Git
            } else {
                VcsKind::None
            },
            git_root: if root.join(".git").exists() {
                Some(root)
            } else {
                None
            },
            case_insensitive: cfg!(windows),
        })
    }

    /// Returns a normalized project-relative comparison path.
    pub fn relative_comparison_path(&self, path: &Path) -> Result<String, ProjectError> {
        let canonical = canonicalize_existing(path)?;
        if !self.contains(&canonical)? {
            return Err(ProjectError::OutsideProject);
        }
        let relative = canonical
            .strip_prefix(&self.canonical_root)
            .map_err(|_| ProjectError::OutsideProject)?;
        Ok(normalize_relative_path(relative, self.case_insensitive))
    }

    /// Checks canonical containment, rejecting symlink escapes.
    pub fn contains(&self, path: &Path) -> Result<bool, ProjectError> {
        let canonical = canonicalize_existing(path)?;
        let root = comparison_path(&self.canonical_root);
        let candidate = comparison_path(&canonical);
        Ok(candidate == root || candidate.starts_with(&format!("{root}/")))
    }

    /// Loads the project's explicit ignore rules.
    pub fn load_ignore_engine(&self) -> Result<IgnoreEngine, ProjectError> {
        IgnoreEngine::load(&self.canonical_root).map_err(|source| ProjectError::Io {
            path: self.canonical_root.join(".agenttracebackignore"),
            source,
        })
    }
}

/// Returns a stable, platform-aware comparison form for a path.
#[must_use]
pub fn comparison_path(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    let trimmed = value.trim_end_matches('/');
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// Rejects parent traversal and returns a slash-separated relative path.
pub fn normalize_relative_path(path: &Path, case_insensitive: bool) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return String::new();
            }
        }
    }
    let value = parts.join("/");
    if case_insensitive {
        value.to_lowercase()
    } else {
        value
    }
}

fn canonicalize_existing(path: &Path) -> Result<PathBuf, ProjectError> {
    fs::canonicalize(path).map_err(|source| ProjectError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{ProjectScope, VcsKind, comparison_path, normalize_relative_path};

    #[test]
    fn discovers_plain_directory_and_rejects_parent_traversal() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(directory.path().join("file.txt"), "content").expect("fixture");
        let scope = ProjectScope::discover(directory.path()).expect("project");
        assert_eq!(scope.vcs_kind, VcsKind::None);
        assert!(
            scope
                .contains(&directory.path().join("file.txt"))
                .expect("containment")
        );
        assert_eq!(
            normalize_relative_path(std::path::Path::new("../escape"), false),
            ""
        );
    }

    #[test]
    fn comparison_path_is_stable() {
        let path = std::path::Path::new("/tmp/project/");
        assert_eq!(comparison_path(path), "/tmp/project");
    }
}
