use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use sysinfo::{Pid, ProcessesToUpdate, System};

/// Stable identity for a process, including start time to avoid PID reuse.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProcessKey {
    /// Operating-system PID.
    pub pid: u32,
    /// Process start time as reported by the OS.
    pub start_time: u64,
}

/// Best-effort process metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessInfo {
    /// Stable process key.
    pub key: ProcessKey,
    /// Parent PID when observable.
    pub parent_pid: Option<u32>,
    /// Executable display path.
    pub executable: Option<PathBuf>,
    /// Redaction-ready argv preview.
    pub argv_preview: Option<String>,
    /// Working directory when readable.
    pub cwd: Option<PathBuf>,
    /// Observation time in UTC microseconds.
    pub observed_at_us: i64,
}

/// Process starts and exits observed between polls.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProcessDelta {
    /// Newly observed processes.
    pub starts: Vec<ProcessInfo>,
    /// Processes that disappeared.
    pub exits: Vec<ProcessInfo>,
}

/// Best-effort cross-platform process observer.
pub struct ProcessObserver {
    system: System,
    known: HashMap<ProcessKey, ProcessInfo>,
    initialized: bool,
}

impl ProcessObserver {
    /// Creates a process observer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            system: System::new_all(),
            known: HashMap::new(),
            initialized: false,
        }
    }

    /// Captures the current process set without emitting starts.
    pub fn baseline(&mut self) -> ProcessDelta {
        self.refresh();
        self.known = self.current_processes();
        self.initialized = true;
        ProcessDelta::default()
    }

    /// Refreshes process state and returns starts and exits.
    pub fn poll(&mut self) -> ProcessDelta {
        self.refresh();
        if !self.initialized {
            self.initialized = true;
            return ProcessDelta::default();
        }
        let current = self.current_processes();
        let starts = current
            .iter()
            .filter(|(key, _)| !self.known.contains_key(key))
            .map(|(_, info)| info.clone())
            .collect();
        let exits = self
            .known
            .iter()
            .filter(|(key, _)| !current.contains_key(key))
            .map(|(_, info)| info.clone())
            .collect();
        self.known = current;
        ProcessDelta { starts, exits }
    }

    /// Returns all currently known processes.
    pub fn processes(&self) -> impl Iterator<Item = &ProcessInfo> {
        self.known.values()
    }

    /// Returns true when a process belongs to a root's observed process subtree.
    #[must_use]
    pub fn is_descendant(&self, root_pid: u32, candidate_pid: u32) -> bool {
        if root_pid == candidate_pid {
            return true;
        }
        let parents: HashMap<u32, Option<u32>> = self
            .known
            .values()
            .map(|process| (process.key.pid, process.parent_pid))
            .collect();
        let mut current = candidate_pid;
        let mut visited = HashSet::new();
        while visited.insert(current) {
            let Some(parent) = parents.get(&current).copied().flatten() else {
                return false;
            };
            if parent == root_pid {
                return true;
            }
            current = parent;
        }
        false
    }

    fn refresh(&mut self) {
        self.system.refresh_processes(ProcessesToUpdate::All, true);
    }

    fn current_processes(&self) -> HashMap<ProcessKey, ProcessInfo> {
        self.system
            .processes()
            .values()
            .map(|process| {
                let key = ProcessKey {
                    pid: process.pid().as_u32(),
                    start_time: process.start_time(),
                };
                let info = ProcessInfo {
                    key,
                    parent_pid: process.parent().map(Pid::as_u32),
                    executable: process.exe().map(PathBuf::from),
                    argv_preview: Some(
                        process
                            .cmd()
                            .iter()
                            .map(|value| value.to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                    .filter(|value| !value.is_empty()),
                    cwd: process.cwd().map(PathBuf::from),
                    observed_at_us: timestamp_us(),
                };
                (key, info)
            })
            .collect()
    }
}

impl Default for ProcessObserver {
    fn default() -> Self {
        Self::new()
    }
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
    use super::ProcessObserver;

    #[test]
    fn observes_the_current_process_tree() {
        let mut observer = ProcessObserver::new();
        observer.baseline();
        let current_pid = std::process::id();
        assert!(observer.is_descendant(current_pid, current_pid));
        assert!(
            observer
                .processes()
                .any(|process| process.key.pid == current_pid)
        );
    }
}
