use std::{
    ffi::OsString,
    io::{self, Read, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use crossterm::terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use thiserror::Error;

/// Callback receiving raw terminal output chunks.
pub type OutputCallback = Arc<dyn Fn(Vec<u8>) + Send + Sync>;
/// Callback receiving the root child PID after spawn.
pub type SpawnCallback = Arc<dyn Fn(u32) + Send + Sync>;

/// PTY launch errors.
#[derive(Debug, Error)]
pub enum PtyError {
    /// The pseudo-terminal could not be created.
    #[error("could not create pseudo-terminal: {0}")]
    Pty(String),
    /// The child process could not be spawned.
    #[error("could not spawn wrapped command: {0}")]
    Spawn(String),
    /// Terminal I/O failed.
    #[error("terminal I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// Structured argv and environment for a wrapped command.
#[derive(Clone)]
pub struct PtyOptions {
    /// Executable to launch without shell interpolation.
    pub program: OsString,
    /// Exact argument vector.
    pub arguments: Vec<OsString>,
    /// Child working directory.
    pub cwd: PathBuf,
    /// Additional environment entries.
    pub environment: Vec<(OsString, OsString)>,
    /// Optional callback receiving raw output chunks.
    pub output_callback: Option<OutputCallback>,
    /// Optional callback receiving the root child PID immediately after spawn.
    pub spawn_callback: Option<SpawnCallback>,
}

/// Wrapped process outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PtyOutcome {
    /// Process exit code, or 1 when terminated by a signal.
    pub exit_code: u32,
    /// Signal name on Unix platforms when applicable.
    pub signal: Option<String>,
    /// Root child process identifier.
    pub root_pid: Option<u32>,
}

/// Runs a command in an interactive cross-platform PTY and preserves stdin/stdout.
pub fn run_pty(options: PtyOptions) -> Result<PtyOutcome, PtyError> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(current_pty_size())
        .map_err(|error| PtyError::Pty(error.to_string()))?;
    let mut command = CommandBuilder::new(options.program);
    command.args(options.arguments);
    command.cwd(options.cwd);
    for (key, value) in options.environment {
        command.env(key, value);
    }
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| PtyError::Spawn(error.to_string()))?;
    drop(pair.slave);

    let root_pid = child.process_id();
    if let (Some(callback), Some(pid)) = (options.spawn_callback, root_pid) {
        callback(pid);
    }
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| PtyError::Pty(error.to_string()))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|error| PtyError::Pty(error.to_string()))?;
    let callback = options.output_callback;
    let reader_callback = callback.clone();
    std::thread::Builder::new()
        .name("agenttraceback-pty-output".to_owned())
        .spawn(move || copy_output(&mut reader, reader_callback))
        .map_err(PtyError::Io)?;
    std::thread::Builder::new()
        .name("agenttraceback-pty-input".to_owned())
        .spawn(move || copy_input(&mut writer))
        .map_err(PtyError::Io)?;

    let _raw_mode = RawModeGuard::enable();
    let mut last_size = current_pty_size();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(PtyError::Io)? {
            break status;
        }
        let current_size = current_pty_size();
        if current_size.rows != last_size.rows || current_size.cols != last_size.cols {
            pair.master
                .resize(current_size)
                .map_err(|error| PtyError::Pty(error.to_string()))?;
            last_size = current_size;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    Ok(PtyOutcome {
        exit_code: status.exit_code(),
        signal: status.signal().map(str::to_owned),
        root_pid,
    })
}

fn copy_output(reader: &mut (dyn Read + Send), callback: Option<OutputCallback>) {
    let mut buffer = [0_u8; 8192];
    let mut stdout = io::stdout().lock();
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let chunk = &buffer[..read];
                let _ = stdout.write_all(chunk);
                let _ = stdout.flush();
                if let Some(callback) = &callback {
                    callback(chunk.to_vec());
                }
            }
        }
    }
}

fn copy_input(writer: &mut (dyn Write + Send)) {
    let mut stdin = io::stdin().lock();
    let mut buffer = [0_u8; 4096];
    loop {
        match stdin.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if writer.write_all(&buffer[..read]).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        }
    }
}

fn current_pty_size() -> PtySize {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

struct RawModeGuard {
    restore: bool,
}

impl RawModeGuard {
    fn enable() -> Self {
        let already_raw = is_raw_mode_enabled().unwrap_or(false);
        let restore = !already_raw && enable_raw_mode().is_ok();
        Self { restore }
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.restore {
            let _ = disable_raw_mode();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{env, ffi::OsString};

    use super::{PtyOptions, run_pty};

    #[test]
    fn pty_preserves_exit_status_and_output() {
        let outcome = run_pty(PtyOptions {
            program: OsString::from("rustc"),
            arguments: vec![OsString::from("--version")],
            cwd: env::current_dir().expect("current directory"),
            environment: Vec::new(),
            output_callback: None,
            spawn_callback: None,
        })
        .expect("run PTY");
        assert_eq!(outcome.exit_code, 0);
        assert!(outcome.root_pid.is_some());
    }
}
