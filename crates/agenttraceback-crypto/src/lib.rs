//! Installation key management, authenticated encryption, and event-chain integrity.

mod chain;
mod keys;
mod secret;

pub use chain::{
    ChainEntry, ChainError, ChainHash, ChainVerificationIssue, ChainVerificationReport,
    ChainVerifier, RootSigner, canonical_digest, canonicalize, hash_chain_entry,
};
pub use keys::{KeyError, KeyProtectionKind, LoadedMasterKey, MasterKey, MasterKeyStore};
pub use secret::{BlobCipher, BlobCompression, SealedBlob, SecretError};
