//! Configuration discovery and owner-only platform paths.

use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use agenttraceback_types::RuntimeMetadata;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(target_os = "linux")]
const APP_NAME: &str = "agenttraceback";
#[cfg(any(target_os = "macos", target_os = "windows"))]
const DISPLAY_APP_NAME: &str = "AgentTraceback";

/// Errors returned while resolving or managing local paths.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The operating system did not expose a required base directory.
    #[error("could not resolve {0}")]
    MissingBaseDirectory(&'static str),
    /// Filesystem operation failed.
    #[error("filesystem operation failed at {path}: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Source error.
        #[source]
        source: io::Error,
    },
    /// TOML parsing failed.
    #[error("invalid bootstrap configuration: {0}")]
    Toml(#[from] toml::de::Error),
    /// TOML serialization failed.
    #[error("could not serialize bootstrap configuration: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
    /// Runtime metadata could not be decoded.
    #[error("invalid daemon runtime metadata: {0}")]
    RuntimeMetadata(#[from] serde_json::Error),
}

/// Minimal bootstrap configuration stored outside the primary data store.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BootstrapConfig {
    /// Optional data-root override.
    pub data_dir: Option<PathBuf>,
    /// Daemon log level.
    pub daemon_log_level: Option<String>,
    /// Whether the user enabled daemon startup at login.
    pub start_at_login: bool,
    /// User-selected update channel.
    pub update_channel: Option<String>,
}

/// Resolved platform paths used by the daemon, CLI, and desktop shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformPaths {
    /// Primary persistent data root.
    pub data_root: PathBuf,
    /// Bootstrap configuration directory.
    pub config_root: PathBuf,
    /// Runtime metadata file.
    pub runtime_file: PathBuf,
    /// Single-instance lock file.
    pub lock_file: PathBuf,
    /// Persistent API token file.
    pub token_file: PathBuf,
    /// SQLite database file.
    pub database_file: PathBuf,
    /// Encrypted content-addressed blob root.
    pub blobs_root: PathBuf,
    /// Bounded ingestion spool root.
    pub spool_root: PathBuf,
    /// Pre-migration and recovery backup root.
    pub backups_root: PathBuf,
    /// Export output root.
    pub exports_root: PathBuf,
    /// Rotating daemon log root.
    pub logs_root: PathBuf,
}

impl PlatformPaths {
    /// Resolves platform paths, honoring `AGENTTRACEBACK_DATA_DIR` for tests and development.
    pub fn discover() -> Result<Self, ConfigError> {
        let config_root = platform_config_root()?;
        let default_data_root = platform_data_root()?;
        let bootstrap_path = config_root.join("config.toml");
        let bootstrap = load_bootstrap_config(&bootstrap_path)?;
        let data_root = env::var_os("AGENTTRACEBACK_DATA_DIR")
            .map(PathBuf::from)
            .or(bootstrap.data_dir)
            .unwrap_or(default_data_root);
        let runtime_file = platform_runtime_file()?;

        Ok(Self {
            database_file: data_root.join("agenttraceback.db"),
            blobs_root: data_root.join("blobs"),
            spool_root: data_root.join("spool"),
            backups_root: data_root.join("backups"),
            exports_root: data_root.join("exports"),
            logs_root: data_root.join("logs"),
            lock_file: data_root.join("agenttraceback.lock"),
            token_file: data_root.join("api-token"),
            config_root,
            data_root,
            runtime_file,
        })
    }

    /// Creates owner-only application directories.
    pub fn ensure_directories(&self) -> Result<(), ConfigError> {
        for directory in [
            &self.data_root,
            &self.config_root,
            &self.blobs_root,
            &self.spool_root,
            &self.backups_root,
            &self.exports_root,
            &self.logs_root,
        ] {
            create_private_directory(directory)?;
        }

        if let Some(parent) = self.runtime_file.parent() {
            create_private_directory(parent)?;
        }

        Ok(())
    }

    /// Loads runtime metadata if a daemon has published it.
    pub fn read_runtime_metadata(&self) -> Result<RuntimeMetadata, ConfigError> {
        let bytes = fs::read(&self.runtime_file).map_err(|source| ConfigError::Io {
            path: self.runtime_file.clone(),
            source,
        })?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Atomically publishes runtime metadata with owner-only permissions.
    pub fn write_runtime_metadata(&self, metadata: &RuntimeMetadata) -> Result<(), ConfigError> {
        let parent = self
            .runtime_file
            .parent()
            .ok_or(ConfigError::MissingBaseDirectory(
                "runtime metadata parent directory",
            ))?;
        create_private_directory(parent)?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| ConfigError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        set_private_file_permissions(temporary.path())?;
        let bytes = serde_json::to_vec_pretty(metadata)?;
        temporary
            .write_all(&bytes)
            .and_then(|_| temporary.as_file_mut().sync_all())
            .map_err(|source| ConfigError::Io {
                path: temporary.path().to_path_buf(),
                source,
            })?;
        persist_atomic(temporary, &self.runtime_file)
    }

    /// Removes runtime metadata only when it still belongs to the supplied process.
    pub fn remove_runtime_metadata_for_pid(&self, pid: u32) -> Result<(), ConfigError> {
        match self.read_runtime_metadata() {
            Ok(metadata) if metadata.pid == pid => {
                fs::remove_file(&self.runtime_file).map_err(|source| ConfigError::Io {
                    path: self.runtime_file.clone(),
                    source,
                })
            }
            Ok(_) | Err(ConfigError::Io { .. }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Loads or creates the persistent per-installation API token.
    pub fn load_or_create_api_token(&self) -> Result<String, ConfigError> {
        match fs::read_to_string(&self.token_file) {
            Ok(token) if token.trim().len() == 64 => Ok(token.trim().to_owned()),
            Ok(_) => Err(ConfigError::Io {
                path: self.token_file.clone(),
                source: io::Error::new(io::ErrorKind::InvalidData, "token file is malformed"),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                generate_and_store_token(&self.token_file)
            }
            Err(source) => Err(ConfigError::Io {
                path: self.token_file.clone(),
                source,
            }),
        }
    }
}

fn generate_and_store_token(path: &Path) -> Result<String, ConfigError> {
    use rand::RngCore;

    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let parent = path
        .parent()
        .ok_or(ConfigError::MissingBaseDirectory("token parent directory"))?;
    create_private_directory(parent)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| ConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    set_private_file_permissions(temporary.path())?;
    temporary
        .write_all(token.as_bytes())
        .and_then(|_| temporary.as_file_mut().sync_all())
        .map_err(|source| ConfigError::Io {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    persist_atomic(temporary, path)?;
    Ok(token)
}

fn load_bootstrap_config(path: &Path) -> Result<BootstrapConfig, ConfigError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(toml::from_str(&contents)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BootstrapConfig::default()),
        Err(source) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn persist_atomic(
    temporary: tempfile::NamedTempFile,
    destination: &Path,
) -> Result<(), ConfigError> {
    match temporary.persist(destination) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(destination).map_err(|source| ConfigError::Io {
                path: destination.to_path_buf(),
                source,
            })?;
            error
                .file
                .persist(destination)
                .map(|_| ())
                .map_err(|source| ConfigError::Io {
                    path: destination.to_path_buf(),
                    source: source.error,
                })
        }
        Err(error) => Err(ConfigError::Io {
            path: destination.to_path_buf(),
            source: error.error,
        }),
    }
}

fn platform_config_root() -> Result<PathBuf, ConfigError> {
    if let Some(path) = env::var_os("AGENTTRACEBACK_CONFIG_DIR") {
        return Ok(PathBuf::from(path));
    }

    platform_config_root_inner()
}

#[cfg(target_os = "linux")]
fn platform_config_root_inner() -> Result<PathBuf, ConfigError> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs_home().map(|home| home.join(".config")))
        .ok_or(ConfigError::MissingBaseDirectory("XDG config directory"))?;
    Ok(base.join(APP_NAME))
}

#[cfg(target_os = "macos")]
fn platform_config_root_inner() -> Result<PathBuf, ConfigError> {
    dirs_home()
        .map(|home| {
            home.join("Library/Application Support")
                .join(DISPLAY_APP_NAME)
        })
        .ok_or(ConfigError::MissingBaseDirectory("home directory"))
}

#[cfg(target_os = "windows")]
fn platform_config_root_inner() -> Result<PathBuf, ConfigError> {
    env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| path.join(DISPLAY_APP_NAME))
        .ok_or(ConfigError::MissingBaseDirectory("APPDATA"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_config_root_inner() -> Result<PathBuf, ConfigError> {
    Err(ConfigError::MissingBaseDirectory(
        "supported operating system",
    ))
}

fn platform_data_root() -> Result<PathBuf, ConfigError> {
    platform_data_root_inner()
}

#[cfg(target_os = "linux")]
fn platform_data_root_inner() -> Result<PathBuf, ConfigError> {
    let base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs_home().map(|home| home.join(".local/share")))
        .ok_or(ConfigError::MissingBaseDirectory("XDG data directory"))?;
    Ok(base.join(APP_NAME))
}

#[cfg(target_os = "macos")]
fn platform_data_root_inner() -> Result<PathBuf, ConfigError> {
    dirs_home()
        .map(|home| {
            home.join("Library/Application Support")
                .join(DISPLAY_APP_NAME)
        })
        .ok_or(ConfigError::MissingBaseDirectory("home directory"))
}

#[cfg(target_os = "windows")]
fn platform_data_root_inner() -> Result<PathBuf, ConfigError> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join(DISPLAY_APP_NAME))
        .ok_or(ConfigError::MissingBaseDirectory("LOCALAPPDATA"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_data_root_inner() -> Result<PathBuf, ConfigError> {
    Err(ConfigError::MissingBaseDirectory(
        "supported operating system",
    ))
}

fn platform_runtime_file() -> Result<PathBuf, ConfigError> {
    platform_runtime_file_inner()
}

#[cfg(target_os = "linux")]
fn platform_runtime_file_inner() -> Result<PathBuf, ConfigError> {
    if let Some(runtime) = env::var_os("XDG_RUNTIME_DIR") {
        return Ok(PathBuf::from(runtime).join(APP_NAME).join("runtime.json"));
    }
    let state = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs_home().map(|home| home.join(".local/state")))
        .ok_or(ConfigError::MissingBaseDirectory("XDG state directory"))?;
    Ok(state.join(APP_NAME).join("runtime.json"))
}

#[cfg(target_os = "macos")]
fn platform_runtime_file_inner() -> Result<PathBuf, ConfigError> {
    dirs_home()
        .map(|home| {
            home.join("Library/Application Support")
                .join(DISPLAY_APP_NAME)
                .join("runtime.json")
        })
        .ok_or(ConfigError::MissingBaseDirectory("home directory"))
}

#[cfg(target_os = "windows")]
fn platform_runtime_file_inner() -> Result<PathBuf, ConfigError> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join(DISPLAY_APP_NAME).join("runtime.json"))
        .ok_or(ConfigError::MissingBaseDirectory("LOCALAPPDATA"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_runtime_file_inner() -> Result<PathBuf, ConfigError> {
    Err(ConfigError::MissingBaseDirectory(
        "supported operating system",
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn dirs_home() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn create_private_directory(path: &Path) -> Result<(), ConfigError> {
    fs::create_dir_all(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    set_directory_permissions(path)
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}
