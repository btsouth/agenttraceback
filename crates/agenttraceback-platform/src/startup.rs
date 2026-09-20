use std::{io, path::Path};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{env, fs, path::PathBuf};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::process::Command;

use thiserror::Error;

/// Startup registration failures.
#[derive(Debug, Error)]
pub enum StartupError {
    /// The required home or application-data directory was unavailable.
    #[error("startup directory is unavailable: {0}")]
    MissingDirectory(&'static str),
    /// Filesystem access failed.
    #[error("startup filesystem operation failed: {0}")]
    Io(#[from] io::Error),
    /// Starting the platform startup tool failed.
    #[error("startup command failed: {0}")]
    Command(String),
}

/// Current startup registration state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupStatus {
    /// Whether AgentTraceback is registered.
    pub installed: bool,
    /// Registration location or registry key.
    pub location: String,
}

/// Installs a per-user startup registration for the daemon binary.
pub fn install(daemon_path: &Path) -> Result<StartupStatus, StartupError> {
    if !daemon_path.is_file() {
        return Err(StartupError::Command(format!(
            "daemon binary does not exist: {}",
            daemon_path.display()
        )));
    }
    startup_install(daemon_path)
}

/// Removes the per-user startup registration when present.
pub fn remove() -> Result<StartupStatus, StartupError> {
    startup_remove()
}

/// Reports the current per-user startup registration.
pub fn status() -> Result<StartupStatus, StartupError> {
    startup_status()
}

#[cfg(target_os = "linux")]
fn startup_install(daemon_path: &Path) -> Result<StartupStatus, StartupError> {
    let path = linux_autostart_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = format!(
        "[Desktop Entry]\nType=Application\nName=AgentTraceback Daemon\nComment=Local record and recovery for AI coding-agent runs\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\nHidden=false\n",
        shell_quote(&daemon_path.to_string_lossy())
    );
    fs::write(&path, contents)?;
    Ok(StartupStatus {
        installed: true,
        location: path.to_string_lossy().into_owned(),
    })
}

#[cfg(target_os = "linux")]
fn startup_remove() -> Result<StartupStatus, StartupError> {
    let path = linux_autostart_path()?;
    let installed = path.exists();
    if installed {
        fs::remove_file(&path)?;
    }
    Ok(StartupStatus {
        installed: false,
        location: path.to_string_lossy().into_owned(),
    })
}

#[cfg(target_os = "linux")]
fn startup_status() -> Result<StartupStatus, StartupError> {
    let path = linux_autostart_path()?;
    Ok(StartupStatus {
        installed: path.is_file(),
        location: path.to_string_lossy().into_owned(),
    })
}

#[cfg(target_os = "linux")]
fn linux_autostart_path() -> Result<PathBuf, StartupError> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".config")))
        .ok_or(StartupError::MissingDirectory("XDG_CONFIG_HOME"))?;
    Ok(config.join("autostart/agenttraceback.desktop"))
}

#[cfg(target_os = "macos")]
fn startup_install(daemon_path: &Path) -> Result<StartupStatus, StartupError> {
    let path = macos_launch_agent_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>com.agenttraceback.daemon</string><key>ProgramArguments</key><array><string>{}</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><false/></dict></plist>\n",
        xml_escape(&daemon_path.to_string_lossy())
    );
    fs::write(&path, contents)?;
    let _ = Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&path)
        .status();
    Ok(StartupStatus {
        installed: true,
        location: path.to_string_lossy().into_owned(),
    })
}

#[cfg(target_os = "macos")]
fn startup_remove() -> Result<StartupStatus, StartupError> {
    let path = macos_launch_agent_path()?;
    if path.exists() {
        let _ = Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&path)
            .status();
        fs::remove_file(&path)?;
    }
    Ok(StartupStatus {
        installed: false,
        location: path.to_string_lossy().into_owned(),
    })
}

#[cfg(target_os = "macos")]
fn startup_status() -> Result<StartupStatus, StartupError> {
    let path = macos_launch_agent_path()?;
    Ok(StartupStatus {
        installed: path.is_file(),
        location: path.to_string_lossy().into_owned(),
    })
}

#[cfg(target_os = "macos")]
fn macos_launch_agent_path() -> Result<PathBuf, StartupError> {
    home()
        .map(|home| home.join("Library/LaunchAgents/com.agenttraceback.daemon.plist"))
        .ok_or(StartupError::MissingDirectory("home directory"))
}

#[cfg(target_os = "windows")]
const WINDOWS_RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(target_os = "windows")]
fn startup_install(daemon_path: &Path) -> Result<StartupStatus, StartupError> {
    let output = Command::new("reg")
        .args([
            "add",
            WINDOWS_RUN_KEY,
            "/v",
            "AgentTraceback",
            "/t",
            "REG_SZ",
            "/d",
        ])
        .arg(format!("\"{}\"", daemon_path.display()))
        .arg("/f")
        .output()
        .map_err(|error| StartupError::Command(error.to_string()))?;
    if !output.status.success() {
        return Err(StartupError::Command(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(StartupStatus {
        installed: true,
        location: WINDOWS_RUN_KEY.to_owned(),
    })
}

#[cfg(target_os = "windows")]
fn startup_remove() -> Result<StartupStatus, StartupError> {
    let output = Command::new("reg")
        .args(["delete", WINDOWS_RUN_KEY, "/v", "AgentTraceback", "/f"])
        .output()
        .map_err(|error| StartupError::Command(error.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("unable to find") {
            return Err(StartupError::Command(stderr.trim().to_owned()));
        }
    }
    Ok(StartupStatus {
        installed: false,
        location: WINDOWS_RUN_KEY.to_owned(),
    })
}

#[cfg(target_os = "windows")]
fn startup_status() -> Result<StartupStatus, StartupError> {
    let output = Command::new("reg")
        .args(["query", WINDOWS_RUN_KEY, "/v", "AgentTraceback"])
        .output()
        .map_err(|error| StartupError::Command(error.to_string()))?;
    Ok(StartupStatus {
        installed: output.status.success(),
        location: WINDOWS_RUN_KEY.to_owned(),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn startup_install(_daemon_path: &Path) -> Result<StartupStatus, StartupError> {
    Err(StartupError::MissingDirectory("supported operating system"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn startup_remove() -> Result<StartupStatus, StartupError> {
    Err(StartupError::MissingDirectory("supported operating system"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn startup_status() -> Result<StartupStatus, StartupError> {
    Err(StartupError::MissingDirectory("supported operating system"))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn home() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(target_os = "linux")]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn shell_quote_handles_single_quotes() {
        assert_eq!(super::shell_quote("/tmp/a'b"), "'/tmp/a'\\''b'");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn xml_escape_handles_entities() {
        assert_eq!(super::xml_escape("a&<b>"), "a&amp;&lt;b&gt;");
    }
}
