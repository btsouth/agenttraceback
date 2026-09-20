use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use std::fs::File;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hkdf::Hkdf;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tempfile::NamedTempFile;
use thiserror::Error;
use zeroize::Zeroize;

const KEYRING_SERVICE: &str = "com.agenttraceback.desktop";
const KEYRING_ACCOUNT: &str = "installation-master-key-v1";
const MASTER_KEY_BYTES: usize = 32;

/// Errors produced by installation key storage and derivation.
#[derive(Debug, Error)]
pub enum KeyError {
    /// Platform credential storage is unavailable when it is mandatory.
    #[error("platform credential storage is unavailable: {0}")]
    CredentialStoreUnavailable(String),
    /// A stored key is malformed.
    #[error("stored installation key is malformed")]
    MalformedKey,
    /// Key derivation rejected a context.
    #[error("invalid key derivation context")]
    InvalidDerivationContext,
    /// A key file operation failed.
    #[error("key file operation failed at {path}: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
}

/// The protection mechanism backing the installation master key.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyProtectionKind {
    /// macOS Keychain Services.
    MacosKeychain,
    /// Windows Credential Manager.
    WindowsCredentialManager,
    /// Linux Secret Service.
    LinuxSecretService,
    /// Owner-only key file fallback on Linux.
    DevicePermissionsOnly,
}

impl KeyProtectionKind {
    /// Returns a plain-language security label for the UI.
    #[must_use]
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::MacosKeychain => "macOS Keychain",
            Self::WindowsCredentialManager => "Windows Credential Manager",
            Self::LinuxSecretService => "Linux Secret Service",
            Self::DevicePermissionsOnly => "Device permissions only",
        }
    }
}

/// A loaded installation key and the protection mechanism that owns it.
#[derive(Clone, Debug)]
pub struct LoadedMasterKey {
    /// In-memory key material.
    pub key: MasterKey,
    /// Where the key is persisted.
    pub protection: KeyProtectionKind,
}

/// A 256-bit installation master key.
#[derive(Clone)]
pub struct MasterKey(KeyMaterial);

#[derive(Clone)]
struct KeyMaterial(Arc<[u8; MASTER_KEY_BYTES]>);

impl KeyMaterial {
    fn new(bytes: [u8; MASTER_KEY_BYTES]) -> Self {
        Self(Arc::new(bytes))
    }
}

impl Drop for KeyMaterial {
    fn drop(&mut self) {
        if let Some(bytes) = Arc::get_mut(&mut self.0) {
            bytes.zeroize();
        }
    }
}

impl MasterKey {
    /// Generates a random installation key.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0_u8; MASTER_KEY_BYTES];
        rand::rng().fill_bytes(&mut bytes);
        Self(KeyMaterial::new(bytes))
    }

    /// Restores a key from exactly 32 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, KeyError> {
        let key: [u8; MASTER_KEY_BYTES] = bytes.try_into().map_err(|_| KeyError::MalformedKey)?;
        Ok(Self(KeyMaterial::new(key)))
    }

    /// Derives a versioned 256-bit subkey without exposing the master key.
    pub fn derive_subkey(&self, context: &str) -> Result<[u8; 32], KeyError> {
        if context.is_empty() || context.len() > 128 {
            return Err(KeyError::InvalidDerivationContext);
        }
        let hkdf = Hkdf::<Sha256>::new(Some(b"agenttraceback-installation-key-v1"), &*self.0.0);
        let mut output = [0_u8; 32];
        hkdf.expand(context.as_bytes(), &mut output)
            .map_err(|_| KeyError::InvalidDerivationContext)?;
        Ok(output)
    }

    /// Computes a short keyed digest suitable for redaction placeholders.
    pub fn keyed_digest(&self, value: &[u8]) -> Result<String, KeyError> {
        let key = self.derive_subkey("agenttraceback-redaction-digest-v1")?;
        let digest = blake3::keyed_hash(&key, value);
        Ok(hex::encode(&digest.as_bytes()[..4]))
    }

    fn expose_for_persistence(&self) -> &[u8; MASTER_KEY_BYTES] {
        &self.0.0
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MasterKey([REDACTED])")
    }
}

/// Loads or creates the installation master key.
#[derive(Clone, Debug)]
pub struct MasterKeyStore {
    key_file: PathBuf,
    force_file_backed: bool,
}

impl MasterKeyStore {
    /// Creates a key store rooted at a private data directory.
    #[must_use]
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            key_file: data_root.into().join("master.key"),
            force_file_backed: false,
        }
    }

    /// Forces owner-only file storage. Intended for tests and explicit recovery.
    #[must_use]
    pub fn file_backed(data_root: impl Into<PathBuf>) -> Self {
        Self {
            key_file: data_root.into().join("master.key"),
            force_file_backed: true,
        }
    }

    /// Loads the key from the platform store or creates it on first run.
    pub fn load_or_create(&self) -> Result<LoadedMasterKey, KeyError> {
        if !self.force_file_backed {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                return self.load_or_create_native();
            }

            #[cfg(target_os = "linux")]
            {
                if self.key_file.exists() {
                    return Ok(LoadedMasterKey {
                        key: load_file_key(&self.key_file)?,
                        protection: KeyProtectionKind::DevicePermissionsOnly,
                    });
                }
                match self.load_or_create_secret_service() {
                    Ok(loaded) => return Ok(loaded),
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            "Secret Service unavailable; using owner-only key file"
                        );
                    }
                }
            }
        }

        Ok(LoadedMasterKey {
            key: load_or_create_file_key(&self.key_file)?,
            protection: KeyProtectionKind::DevicePermissionsOnly,
        })
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn load_or_create_native(&self) -> Result<LoadedMasterKey, KeyError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(|error| {
            KeyError::CredentialStoreUnavailable(redacted_keyring_error(&error))
        })?;
        let key = match entry.get_secret() {
            Ok(bytes) => MasterKey::from_bytes(&bytes)?,
            Err(keyring::Error::NoEntry) => {
                let key = MasterKey::generate();
                entry
                    .set_secret(key.expose_for_persistence())
                    .map_err(|error| {
                        KeyError::CredentialStoreUnavailable(redacted_keyring_error(&error))
                    })?;
                key
            }
            Err(error) => {
                return Err(KeyError::CredentialStoreUnavailable(
                    redacted_keyring_error(&error),
                ));
            }
        };
        let protection = if cfg!(target_os = "macos") {
            KeyProtectionKind::MacosKeychain
        } else {
            KeyProtectionKind::WindowsCredentialManager
        };
        Ok(LoadedMasterKey { key, protection })
    }

    #[cfg(target_os = "linux")]
    fn load_or_create_secret_service(&self) -> Result<LoadedMasterKey, KeyError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(|error| {
            KeyError::CredentialStoreUnavailable(redacted_keyring_error(&error))
        })?;
        let key = match entry.get_secret() {
            Ok(bytes) => MasterKey::from_bytes(&bytes)?,
            Err(keyring::Error::NoEntry) => {
                let key = MasterKey::generate();
                entry
                    .set_secret(key.expose_for_persistence())
                    .map_err(|error| {
                        KeyError::CredentialStoreUnavailable(redacted_keyring_error(&error))
                    })?;
                key
            }
            Err(error) => {
                return Err(KeyError::CredentialStoreUnavailable(
                    redacted_keyring_error(&error),
                ));
            }
        };
        Ok(LoadedMasterKey {
            key,
            protection: KeyProtectionKind::LinuxSecretService,
        })
    }
}

fn redacted_keyring_error(error: &keyring::Error) -> String {
    if matches!(error, keyring::Error::NoEntry) {
        "credential entry does not exist".to_owned()
    } else {
        "platform credential operation failed".to_owned()
    }
}

fn load_or_create_file_key(path: &Path) -> Result<MasterKey, KeyError> {
    if path.exists() {
        return load_file_key(path);
    }
    let key = MasterKey::generate();
    write_file_key(path, &key)?;
    Ok(key)
}

fn load_file_key(path: &Path) -> Result<MasterKey, KeyError> {
    let encoded = fs::read_to_string(path).map_err(|source| KeyError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded.trim())
        .map_err(|_| KeyError::MalformedKey)?;
    MasterKey::from_bytes(&decoded)
}

fn write_file_key(path: &Path, key: &MasterKey) -> Result<(), KeyError> {
    let parent = path.parent().ok_or_else(|| KeyError::Io {
        path: path.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "key path has no parent"),
    })?;
    fs::create_dir_all(parent).map_err(|source| KeyError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    set_private_directory(parent)?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| KeyError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    set_private_file(temporary.path())?;
    let encoded = URL_SAFE_NO_PAD.encode(key.expose_for_persistence());
    temporary
        .write_all(encoded.as_bytes())
        .and_then(|_| temporary.as_file_mut().sync_all())
        .map_err(|source| KeyError::Io {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary.persist(path).map_err(|error| KeyError::Io {
        path: path.to_path_buf(),
        source: error.error,
    })?;
    sync_parent_directory(path)?;
    set_private_file(path)
}

fn sync_parent_directory(path: &Path) -> Result<(), KeyError> {
    #[cfg(unix)]
    {
        let parent = path.parent().ok_or_else(|| KeyError::Io {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "key path has no parent"),
        })?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| KeyError::Io {
                path: parent.to_path_buf(),
                source,
            })
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> Result<(), KeyError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| KeyError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_private_directory(_path: &Path) -> Result<(), KeyError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<(), KeyError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| KeyError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> Result<(), KeyError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{KeyProtectionKind, MasterKey, MasterKeyStore};

    #[test]
    fn file_backed_key_round_trips() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = MasterKeyStore::file_backed(directory.path());
        let first = store.load_or_create().expect("create key");
        let second = store.load_or_create().expect("load key");

        assert_eq!(first.protection, KeyProtectionKind::DevicePermissionsOnly);
        assert_eq!(
            first.key.derive_subkey("test").expect("derive first"),
            second.key.derive_subkey("test").expect("derive second")
        );
    }

    #[test]
    fn debug_output_does_not_expose_key_material() {
        let key = MasterKey::generate();
        assert_eq!(format!("{key:?}"), "MasterKey([REDACTED])");
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        MasterKeyStore::file_backed(directory.path())
            .load_or_create()
            .expect("create key");
        let mode = std::fs::metadata(directory.path().join("master.key"))
            .expect("key metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
