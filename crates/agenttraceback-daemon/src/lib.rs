//! AgentTraceback daemon lifecycle and runtime orchestration.

use std::{
    fs::{self, File, OpenOptions},
    io,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
};

mod adapter_service;
mod coordinator;
mod recovery_coordinator;

use agenttraceback_api::{ApiState, CLIENT_TIMEOUT};
use agenttraceback_blobs::BlobStore;
use agenttraceback_config::{ConfigError, PlatformPaths};
use agenttraceback_crypto::{BlobCipher, KeyError, MasterKeyStore, SecretError};
use agenttraceback_events::{IngestionConfig, IngestionError, IngestionPipeline, Redactor};
use agenttraceback_store::{Store, StoreError};
use agenttraceback_types::RuntimeMetadata;
use fs2::FileExt;
use thiserror::Error;
use tokio::net::TcpListener;
use tracing::{info, warn};

pub use adapter_service::AdapterService;
pub use coordinator::WrapperCoordinator;
pub use recovery_coordinator::RecoveryCoordinator;

/// Daemon startup and lifecycle errors.
#[derive(Debug, Error)]
pub enum DaemonError {
    /// Local configuration or filesystem setup failed.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Another daemon owns the selected data directory.
    #[error("another agenttracebackd instance already owns this data directory")]
    AlreadyRunning,
    /// The installation key could not be loaded.
    #[error(transparent)]
    Key(#[from] KeyError),
    /// Blob encryption could not be initialized.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// Encrypted blob storage could not be opened.
    #[error(transparent)]
    Blob(#[from] agenttraceback_blobs::BlobError),
    /// The SQLite event store could not be opened.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The event ingestion pipeline could not be stopped cleanly.
    #[error(transparent)]
    Ingestion(#[from] IngestionError),
    /// Loopback listener setup failed.
    #[error("could not bind loopback API: {0}")]
    Bind(#[from] io::Error),
}

/// Runs the daemon until OS termination or Ctrl+C.
pub async fn run() -> Result<(), DaemonError> {
    let paths = PlatformPaths::discover()?;
    paths.ensure_directories()?;
    let _lock = acquire_single_instance_lock(&paths.lock_file)?;
    let token = paths.load_or_create_api_token()?;
    let loaded_key = MasterKeyStore::new(paths.data_root.clone()).load_or_create()?;
    let cipher = BlobCipher::from_master_key(&loaded_key.key)?;
    let blobs = BlobStore::open(paths.blobs_root.clone(), cipher)?;
    let store = Store::open(&paths.database_file, &paths.backups_root).await?;
    let pipeline = Arc::new(IngestionPipeline::start(
        store.clone(),
        blobs.clone(),
        loaded_key.key.clone(),
        IngestionConfig::default(),
    ));
    let recovery = Arc::new(RecoveryCoordinator::new(store.clone(), blobs.clone()));
    let adapters = Arc::new(AdapterService::new(store.clone(), Arc::clone(&pipeline)));
    let coordinator = Arc::new(WrapperCoordinator::new(
        store.clone(),
        blobs.clone(),
        Arc::clone(&pipeline),
        Redactor::new(loaded_key.key.clone()),
    ));

    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
    let port = listener.local_addr()?.port();
    let started_at_us = current_time_us();
    let metadata = RuntimeMetadata {
        pid: std::process::id(),
        port,
        token: token.clone(),
        daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
        api_version: agenttraceback_types::API_VERSION,
    };
    paths.write_runtime_metadata(&metadata)?;

    let state = ApiState::new(token, port, env!("CARGO_PKG_VERSION"), started_at_us)
        .with_store(store.clone())
        .with_exports_root(paths.exports_root.clone())
        .with_blob_store(blobs.clone())
        .with_key_protection(loaded_key.protection.display_label())
        .with_session_controller(coordinator.clone())
        .with_recovery_controller(recovery)
        .with_adapter_controller(adapters);

    info!(
        port,
        pid = std::process::id(),
        "agenttracebackd listening on loopback"
    );
    let result = agenttraceback_api::serve(listener, state, shutdown_signal()).await;
    coordinator.shutdown().await;
    if let Err(error) = pipeline.close().await {
        warn!(%error, "event ingestion pipeline did not stop cleanly");
    }
    if let Err(error) = store.close().await {
        warn!(%error, "event store did not stop cleanly");
    }
    if let Err(error) = paths.remove_runtime_metadata_for_pid(metadata.pid) {
        warn!(%error, "could not remove runtime metadata during shutdown");
    }
    result.map_err(DaemonError::Bind)
}

fn acquire_single_instance_lock(path: &Path) -> Result<File, DaemonError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    set_private_lock_permissions(path)?;
    file.try_lock_exclusive()
        .map_err(|_| DaemonError::AlreadyRunning)?;
    Ok(file)
}

#[cfg(unix)]
fn set_private_lock_permissions(path: &Path) -> Result<(), DaemonError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        DaemonError::Config(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })
    })
}

#[cfg(not(unix))]
fn set_private_lock_permissions(_path: &Path) -> Result<(), DaemonError> {
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                if let Some(signal) = terminate.as_mut() {
                    signal.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
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

/// Reads runtime metadata without starting a daemon.
pub fn read_runtime() -> Result<RuntimeMetadata, DaemonError> {
    let paths = PlatformPaths::discover()?;
    Ok(paths.read_runtime_metadata()?)
}

/// Removes stale runtime metadata only while holding the daemon instance lock.
pub fn remove_stale_runtime() -> Result<bool, DaemonError> {
    let paths = PlatformPaths::discover()?;
    match paths.read_runtime_metadata() {
        Ok(_) => {
            let _lock = match acquire_single_instance_lock(&paths.lock_file) {
                Ok(lock) => lock,
                Err(DaemonError::AlreadyRunning) => return Ok(false),
                Err(error) => return Err(error),
            };
            match fs::remove_file(&paths.runtime_file) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(source) => Err(ConfigError::Io {
                    path: paths.runtime_file,
                    source,
                }
                .into()),
            }
        }
        Err(ConfigError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

/// Returns the local client timeout used by status checks.
#[must_use]
pub const fn client_timeout() -> std::time::Duration {
    CLIENT_TIMEOUT
}
