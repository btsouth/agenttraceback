//! Cross-platform PTY, filesystem watcher, and process-observation primitives.

mod processes;
mod pty;
mod startup;
mod watcher;

pub use processes::{ProcessDelta, ProcessInfo, ProcessKey, ProcessObserver};
pub use pty::{OutputCallback, PtyError, PtyOptions, PtyOutcome, SpawnCallback, run_pty};
pub use startup::{
    StartupError, StartupStatus, install as install_startup, remove as remove_startup,
    status as startup_status,
};
pub use watcher::{
    DebouncedFileEvent, FileAction, FileEventBatch, FileWatchError, FilesystemObserver,
};
