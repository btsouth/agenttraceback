use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Git discovery or read-only state capture failure.
#[derive(Debug, Error)]
pub enum GitError {
    /// Git could not be launched.
    #[error("could not execute Git: {0}")]
    Command(#[from] io::Error),
    /// Git returned an unexpected failure.
    #[error("Git command failed: {0}")]
    Failed(String),
}

/// Read-only Git state captured at a session boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStateSnapshot {
    /// Repository root.
    pub repo_root: PathBuf,
    /// HEAD object ID when the repository has a commit.
    pub head_oid: Option<String>,
    /// Branch name, or detached when appropriate.
    pub branch_name: Option<String>,
    /// Upstream branch when configured.
    pub upstream_name: Option<String>,
    /// Porcelain v2 status output.
    pub porcelain_v2: String,
    /// Recursive submodule status output.
    pub submodule_status: String,
    /// Whether tracked or untracked changes exist.
    pub dirty: bool,
}

/// Resolves the Git worktree root containing a path.
pub fn git_root(start: &Path) -> Result<Option<PathBuf>, GitError> {
    match run_git(start, ["rev-parse", "--show-toplevel"]) {
        Ok(output) => Ok(Some(PathBuf::from(output.trim()))),
        Err(GitError::Failed(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Captures branch, HEAD, working-tree, and submodule state without mutation.
pub fn capture_git_state(repo_root: &Path) -> Result<Option<GitStateSnapshot>, GitError> {
    if git_root(repo_root)?.is_none() {
        return Ok(None);
    }
    let head_oid = run_git(repo_root, ["rev-parse", "--verify", "HEAD"])
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let branch_name = run_git(repo_root, ["symbolic-ref", "--short", "HEAD"])
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let upstream_name = run_git(
        repo_root,
        [
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .ok()
    .map(|value| value.trim().to_owned())
    .filter(|value| !value.is_empty());
    let porcelain_v2 = run_git(
        repo_root,
        [
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?;
    let submodule_status =
        run_git(repo_root, ["submodule", "status", "--recursive"]).unwrap_or_default();
    let dirty = porcelain_v2
        .lines()
        .any(|line| !line.starts_with('#') && !line.trim().is_empty());
    Ok(Some(GitStateSnapshot {
        repo_root: repo_root.to_path_buf(),
        head_oid,
        branch_name,
        upstream_name,
        porcelain_v2,
        submodule_status,
        dirty,
    }))
}

fn run_git<const N: usize>(repo_root: &Path, arguments: [&str; N]) -> Result<String, GitError> {
    let mut command = base_git_command(repo_root);
    if arguments.first() == Some(&"status") {
        let filters = base_git_command(repo_root)
            .args([
                "config",
                "--null",
                "--name-only",
                "--get-regexp",
                r"^filter\..*\.(clean|smudge|process|required)$",
            ])
            .output()?;
        if !filters.status.success() && filters.status.code() != Some(1) {
            return Err(GitError::Failed(
                "could not inspect Git filters safely".to_owned(),
            ));
        }
        let keys = std::str::from_utf8(&filters.stdout)
            .map_err(|_| GitError::Failed("Git filter keys are not UTF-8".to_owned()))?;
        for key in keys.split('\0').filter(|key| !key.is_empty()) {
            let value = if key.ends_with(".required") {
                "false"
            } else {
                ""
            };
            command.arg("-c").arg(format!("{key}={value}"));
        }
    }
    let output = command.args(arguments).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(GitError::Failed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

fn base_git_command(repo_root: &Path) -> Command {
    let mut command = Command::new("git");
    command
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
        .arg("--no-optional-locks");
    command
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::capture_git_state;

    #[test]
    fn captures_a_clean_git_repository() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .expect("git init");
        assert!(status.success());
        let state = capture_git_state(directory.path())
            .expect("capture")
            .expect("git state");
        assert!(!state.dirty);
        assert_eq!(state.repo_root, directory.path());
    }

    #[test]
    fn passive_capture_does_not_execute_repository_filters() {
        let directory = tempfile::tempdir().expect("directory");
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(directory.path())
                    .status()
                    .expect("git")
                    .success()
            );
        };
        git(&["init", "-q"]);
        std::fs::write(
            directory.path().join(".gitattributes"),
            "*.txt filter=fixture\n",
        )
        .expect("attributes");
        std::fs::write(directory.path().join("file.txt"), "before").expect("file");
        git(&["add", "."]);
        git(&[
            "config",
            "filter.fixture.clean",
            "echo invoked > filter-ran; cat",
        ]);
        git(&["config", "filter.fixture.required", "true"]);
        std::fs::write(directory.path().join("file.txt"), "modified after indexing")
            .expect("modify");
        capture_git_state(directory.path())
            .expect("capture")
            .expect("state");
        assert!(!directory.path().join("filter-ran").exists());
    }
}
