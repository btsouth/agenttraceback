use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use notify::{
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{ModifyKind, RenameMode},
};
use thiserror::Error;
use tokio::sync::mpsc;

/// Filesystem observer error.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum FileWatchError {
    /// The native watcher reported an overflow or watch-capacity failure.
    #[error("filesystem watcher overflowed")]
    Overflow,
    /// The native watcher reported another error.
    #[error("filesystem watcher error: {0}")]
    Backend(String),
}

/// Observed filesystem action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileAction {
    /// A file or directory was created.
    Created,
    /// A file was modified.
    Modified,
    /// A file or directory was removed.
    Deleted,
    /// A rename source.
    RenameFrom,
    /// A rename destination.
    RenameTo,
    /// A rename where source and destination could not be paired.
    Rename,
}

/// Debounced observed filesystem event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebouncedFileEvent {
    /// Canonical or best-known path.
    pub path: PathBuf,
    /// Normalized action.
    pub action: FileAction,
    /// Observation time in UTC microseconds.
    pub observed_at_us: i64,
}

/// One debounced batch.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FileEventBatch {
    /// Events in stable path order.
    pub events: Vec<DebouncedFileEvent>,
    /// Capture gap flags produced while collecting the batch.
    pub gaps: Vec<FileWatchError>,
}

/// A recursive native filesystem observer for one project root.
pub struct FilesystemObserver {
    _watcher: RecommendedWatcher,
    receiver: mpsc::UnboundedReceiver<notify::Result<Event>>,
    root: PathBuf,
    pending: Vec<DebouncedFileEvent>,
}

impl FilesystemObserver {
    /// Starts recursive observation without following symlink targets.
    pub fn watch(root: impl Into<PathBuf>) -> Result<Self, FileWatchError> {
        let root = root.into();
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })
        .map_err(|error| FileWatchError::Backend(error.to_string()))?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|error| FileWatchError::Backend(error.to_string()))?;
        Ok(Self {
            _watcher: watcher,
            receiver,
            root,
            pending: Vec::new(),
        })
    }

    /// Returns the watched project root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Receives and debounces the next non-empty batch.
    pub async fn next_batch(&mut self) -> Option<FileEventBatch> {
        let mut batch = FileEventBatch::default();
        let first = self.receiver.recv().await?;
        apply_notify_result(first, &mut self.pending, &mut batch.gaps);
        loop {
            match tokio::time::timeout(Duration::from_millis(100), self.receiver.recv()).await {
                Ok(Some(event)) => {
                    apply_notify_result(event, &mut self.pending, &mut batch.gaps);
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        batch.events = std::mem::take(&mut self.pending);
        Some(batch)
    }

    /// Drains already-delivered events without waiting for another notification.
    #[must_use]
    pub fn drain_now(&mut self) -> FileEventBatch {
        let mut batch = FileEventBatch::default();
        while let Ok(event) = self.receiver.try_recv() {
            apply_notify_result(event, &mut self.pending, &mut batch.gaps);
        }
        batch.events = std::mem::take(&mut self.pending);
        batch
    }
}

fn apply_notify_result(
    result: notify::Result<Event>,
    pending: &mut Vec<DebouncedFileEvent>,
    gaps: &mut Vec<FileWatchError>,
) {
    match result {
        Ok(event) => apply_event(event, pending),
        Err(error) => gaps.push(match error.kind {
            notify::ErrorKind::MaxFilesWatch => FileWatchError::Overflow,
            other => FileWatchError::Backend(format!("{other:?}")),
        }),
    }
}

fn apply_event(event: Event, pending: &mut Vec<DebouncedFileEvent>) {
    let observed_at_us = timestamp_us();
    if event.paths.len() > 1
        && matches!(
            event.kind,
            EventKind::Modify(ModifyKind::Name(RenameMode::Both))
        )
    {
        for (index, path) in event.paths.into_iter().enumerate() {
            let action = if index == 0 {
                FileAction::RenameFrom
            } else {
                FileAction::RenameTo
            };
            push_debounced(pending, path, action, observed_at_us);
        }
        return;
    }
    let action = match event.kind {
        EventKind::Create(_) => FileAction::Created,
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => FileAction::RenameFrom,
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => FileAction::RenameTo,
        EventKind::Modify(ModifyKind::Name(_)) => FileAction::Rename,
        EventKind::Modify(_) => FileAction::Modified,
        EventKind::Remove(_) => FileAction::Deleted,
        _ => return,
    };
    for path in event.paths {
        push_debounced(pending, path, action, observed_at_us);
    }
}

fn push_debounced(
    pending: &mut Vec<DebouncedFileEvent>,
    path: PathBuf,
    action: FileAction,
    observed_at_us: i64,
) {
    if let Some(existing) = pending
        .iter_mut()
        .rev()
        .find(|event| event.path == path && event.action == action)
        && action == FileAction::Modified
    {
        existing.observed_at_us = observed_at_us;
        return;
    }
    pending.push(DebouncedFileEvent {
        path,
        action,
        observed_at_us,
    });
}

fn timestamp_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use super::FilesystemObserver;

    #[tokio::test]
    async fn observes_a_file_creation() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut observer = FilesystemObserver::watch(directory.path()).expect("watcher");
        tokio::time::sleep(Duration::from_millis(150)).await;
        fs::write(directory.path().join("observed.txt"), "content").expect("fixture");
        let batch = tokio::time::timeout(Duration::from_secs(5), observer.next_batch())
            .await
            .expect("watch timeout")
            .expect("event batch");
        assert!(
            batch
                .events
                .iter()
                .any(|event| event.path.ends_with("observed.txt"))
        );
    }
}
