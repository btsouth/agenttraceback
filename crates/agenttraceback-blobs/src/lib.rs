//! Content-addressed, compressed, encrypted blob storage.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use agenttraceback_crypto::{BlobCipher, BlobCompression, SecretError};
use tempfile::NamedTempFile;
use thiserror::Error;

const MAX_ENCRYPTED_BLOB_BYTES: u64 = 128 * 1024 * 1024;

/// Errors returned by the content-addressed blob store.
#[derive(Debug, Error)]
pub enum BlobError {
    /// The store root or blob file could not be accessed.
    #[error("blob filesystem operation failed at {path}: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// Encryption or decryption failed.
    #[error(transparent)]
    Crypto(#[from] SecretError),
    /// A stored blob failed digest or format verification.
    #[error("stored blob {digest} failed verification")]
    Corrupt {
        /// Hexadecimal digest.
        digest: String,
    },
}

/// Metadata describing one content-addressed blob.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobDescriptor {
    /// BLAKE3 digest of the plaintext content.
    pub digest: [u8; 32],
    /// Lowercase hexadecimal digest.
    pub hex_digest: String,
    /// Versioned on-disk format.
    pub format_version: u16,
    /// Plaintext byte length.
    pub plaintext_bytes: u64,
    /// Encrypted file byte length.
    pub encrypted_bytes: u64,
    /// Compression selected at write time.
    pub compression: BlobCompression,
}

/// Whether a write created a new file or resolved to existing content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PutOutcome {
    /// New encrypted content was persisted.
    Created,
    /// Existing content with the same plaintext digest was reused.
    Deduplicated,
}

/// Result of writing one content blob.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PutResult {
    /// Stored blob metadata.
    pub descriptor: BlobDescriptor,
    /// Whether deduplication occurred.
    pub outcome: PutOutcome,
}

/// Encrypted content-addressed storage rooted below the AgentTraceback data directory.
#[derive(Clone, Debug)]
pub struct BlobStore {
    root: PathBuf,
    cipher: BlobCipher,
}

impl BlobStore {
    /// Creates a blob store and ensures its owner-only root exists.
    pub fn open(root: impl Into<PathBuf>, cipher: BlobCipher) -> Result<Self, BlobError> {
        let store = Self {
            root: root.into(),
            cipher,
        };
        create_private_directory(&store.root)?;
        Ok(store)
    }

    /// Returns the configured blob root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Seals plaintext and stores it under its BLAKE3 digest.
    pub fn put(&self, plaintext: &[u8]) -> Result<PutResult, BlobError> {
        let sealed = self.cipher.seal(plaintext)?;
        let hex_digest = hex::encode(sealed.digest);
        let path = self.path_for_hex(&hex_digest);
        let descriptor = BlobDescriptor {
            digest: sealed.digest,
            hex_digest: hex_digest.clone(),
            format_version: 1,
            plaintext_bytes: sealed.plaintext_len,
            encrypted_bytes: u64::try_from(sealed.bytes.len()).unwrap_or(u64::MAX),
            compression: sealed.compression,
        };

        if path.exists() {
            let existing = fs::read(&path).map_err(|source| BlobError::Io {
                path: path.clone(),
                source,
            })?;
            let opened = self
                .cipher
                .open(&existing)
                .map_err(|_| BlobError::Corrupt {
                    digest: hex_digest.clone(),
                })?;
            if opened != plaintext {
                return Err(BlobError::Corrupt { digest: hex_digest });
            }
            return Ok(PutResult {
                descriptor,
                outcome: PutOutcome::Deduplicated,
            });
        }

        let parent = path.parent().ok_or_else(|| BlobError::Io {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "blob path has no parent"),
        })?;
        create_private_directory(parent)?;
        let mut temporary = NamedTempFile::new_in(parent).map_err(|source| BlobError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        temporary
            .write_all(&sealed.bytes)
            .and_then(|_| temporary.as_file_mut().sync_all())
            .map_err(|source| BlobError::Io {
                path: temporary.path().to_path_buf(),
                source,
            })?;
        match temporary.persist(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                return Ok(PutResult {
                    descriptor,
                    outcome: PutOutcome::Deduplicated,
                });
            }
            Err(error) => {
                return Err(BlobError::Io {
                    path: path.clone(),
                    source: error.error,
                });
            }
        }
        Ok(PutResult {
            descriptor,
            outcome: PutOutcome::Created,
        })
    }

    /// Loads and authenticates a blob by plaintext digest.
    pub fn get(&self, digest: &[u8; 32]) -> Result<Vec<u8>, BlobError> {
        let hex_digest = hex::encode(digest);
        let path = self.path_for_hex(&hex_digest);
        let metadata = fs::metadata(&path).map_err(|source| BlobError::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.len() > MAX_ENCRYPTED_BLOB_BYTES {
            return Err(BlobError::Corrupt { digest: hex_digest });
        }
        let bytes = fs::read(&path).map_err(|source| BlobError::Io {
            path: path.clone(),
            source,
        })?;
        let plaintext = self.cipher.open(&bytes).map_err(|error| match error {
            SecretError::DigestMismatch | SecretError::Authentication => BlobError::Corrupt {
                digest: hex_digest.clone(),
            },
            other => BlobError::Crypto(other),
        })?;
        if blake3::hash(&plaintext).as_bytes() != digest {
            return Err(BlobError::Corrupt { digest: hex_digest });
        }
        Ok(plaintext)
    }

    /// Verifies that the stored plaintext matches the requested digest.
    pub fn verify(&self, digest: &[u8; 32]) -> Result<bool, BlobError> {
        Ok(blake3::hash(&self.get(digest)?).as_bytes() == digest)
    }

    fn path_for_hex(&self, hex_digest: &str) -> PathBuf {
        self.root.join(&hex_digest[..2]).join(hex_digest)
    }
}

fn create_private_directory(path: &Path) -> Result<(), BlobError> {
    fs::create_dir_all(path).map_err(|source| BlobError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    set_private_directory(path)
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> Result<(), BlobError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| BlobError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_private_directory(_path: &Path) -> Result<(), BlobError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{BlobError, BlobStore, PutOutcome};
    use agenttraceback_crypto::{BlobCipher, MasterKey};

    fn store() -> (tempfile::TempDir, BlobStore) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let cipher = BlobCipher::from_master_key(&MasterKey::generate()).expect("cipher");
        let store = BlobStore::open(directory.path(), cipher).expect("blob store");
        (directory, store)
    }

    #[test]
    fn round_trip_and_verify() {
        let (_directory, store) = store();
        let content = b"content addressed blob\n".repeat(200);
        let result = store.put(&content).expect("put");
        assert_eq!(result.outcome, PutOutcome::Created);
        assert_eq!(store.get(&result.descriptor.digest).expect("get"), content);
        assert!(store.verify(&result.descriptor.digest).expect("verify"));
    }

    #[test]
    fn identical_content_is_deduplicated() {
        let (_directory, store) = store();
        let first = store.put(b"same bytes").expect("first");
        let second = store.put(b"same bytes").expect("second");
        assert_eq!(first.descriptor.digest, second.descriptor.digest);
        assert_eq!(second.outcome, PutOutcome::Deduplicated);
    }

    #[test]
    fn corrupt_blob_is_rejected() {
        let (_directory, store) = store();
        let result = store.put(b"authenticated content").expect("put");
        let path = store.path_for_hex(&result.descriptor.hex_digest);
        let mut bytes = std::fs::read(&path).expect("read blob");
        let last = bytes.len() - 1;
        bytes[last] ^= 0x20;
        std::fs::write(path, bytes).expect("write corrupt blob");
        assert!(matches!(
            store.get(&result.descriptor.digest),
            Err(BlobError::Corrupt { .. })
        ));
    }
}
