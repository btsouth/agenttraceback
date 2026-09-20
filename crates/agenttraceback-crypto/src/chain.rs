use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// A 32-byte chain hash or digest.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChainHash(pub [u8; 32]);

impl ChainHash {
    /// A zero hash used for the first entry in a chain.
    #[must_use]
    pub const fn zero() -> Self {
        Self([0_u8; 32])
    }

    /// Encodes the hash as lowercase hexadecimal.
    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    /// Decodes 64 hexadecimal characters.
    pub fn from_hex(value: &str) -> Result<Self, ChainError> {
        let bytes = hex::decode(value).map_err(|_| ChainError::MalformedHash)?;
        let hash = bytes.try_into().map_err(|_| ChainError::MalformedHash)?;
        Ok(Self(hash))
    }
}

/// A chain digest computation error.
#[derive(Debug, Error)]
pub enum ChainError {
    /// A hash was not exactly 32 bytes of hexadecimal.
    #[error("chain hash is malformed")]
    MalformedHash,
    /// Canonical JSON serialization failed.
    #[error("canonical JSON serialization failed: {0}")]
    CanonicalJson(#[from] serde_json::Error),
}

/// A single immutable integrity-chain entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainEntry {
    /// One-based position in the chain.
    pub sequence: u64,
    /// Entry category, such as `source_event` or `correlation`.
    pub entry_kind: String,
    /// Identifier of the target row.
    pub target_id: String,
    /// SHA-256 digest of the canonical target payload.
    pub canonical_digest: ChainHash,
    /// Digest of the encrypted raw payload, when one exists.
    pub raw_payload_digest: Option<ChainHash>,
    /// Previous entry hash, or zero for the first entry.
    pub previous_hash: ChainHash,
    /// Hash of this entry.
    pub entry_hash: ChainHash,
}

/// Result of verifying a contiguous chain.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChainVerificationReport {
    /// Missing sequence numbers or missing rows.
    pub missing_sequences: Vec<u64>,
    /// Entries whose sequence or previous hash is reordered.
    pub reordered_sequences: Vec<u64>,
    /// Entries whose persisted hash no longer matches their contents.
    pub changed_entries: Vec<u64>,
    /// Chain entries whose referenced encrypted payload is missing or inactive.
    pub missing_payloads: Vec<u64>,
    /// Whether a finalized root disagrees with the current chain tail.
    pub root_mismatch: bool,
}

impl ChainVerificationReport {
    /// Returns true when the chain is internally consistent.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.missing_sequences.is_empty()
            && self.reordered_sequences.is_empty()
            && self.changed_entries.is_empty()
            && self.missing_payloads.is_empty()
            && !self.root_mismatch
    }
}

/// A categorized verification issue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChainVerificationIssue {
    /// A sequence number is absent.
    MissingSequence(u64),
    /// A sequence or previous pointer is out of order.
    ReorderedSequence(u64),
    /// The stored hash does not match the recomputed hash.
    ChangedEntry(u64),
}

/// Verifies a complete session or global chain.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChainVerifier;

impl ChainVerifier {
    /// Verifies sequence continuity and every entry hash.
    #[must_use]
    pub fn verify(entries: &[ChainEntry]) -> ChainVerificationReport {
        let mut report = ChainVerificationReport::default();
        let mut expected_previous = ChainHash::zero();
        let mut expected_sequence = 1_u64;

        for entry in entries {
            if entry.sequence < expected_sequence {
                report.reordered_sequences.push(entry.sequence);
            } else {
                while expected_sequence < entry.sequence {
                    report.missing_sequences.push(expected_sequence);
                    expected_sequence += 1;
                }
                if entry.sequence != expected_sequence {
                    report.reordered_sequences.push(entry.sequence);
                }
            }
            if entry.previous_hash != expected_previous {
                report.reordered_sequences.push(entry.sequence);
            }
            let recomputed = hash_chain_entry(
                entry.previous_hash,
                &entry.entry_kind,
                entry.canonical_digest,
                entry.raw_payload_digest,
            );
            if recomputed != entry.entry_hash {
                report.changed_entries.push(entry.sequence);
            }
            expected_previous = entry.entry_hash;
            expected_sequence = entry.sequence.saturating_add(1);
        }
        report
    }
}

/// Defines the Ed25519 root-signing boundary without enabling v0.1 signing.
pub trait RootSigner: Send + Sync {
    /// Signs a finalized chain root.
    fn sign_root(&self, root_hash: ChainHash) -> Result<Vec<u8>, ChainError>;
    /// Returns the key identifier used in chain-root metadata.
    fn key_id(&self) -> &str;
}

/// Canonicalizes a serializable value with RFC 8785 semantics.
pub fn canonicalize<T: Serialize>(value: &T) -> Result<Vec<u8>, ChainError> {
    Ok(serde_jcs::to_vec(value)?)
}

/// Computes the SHA-256 digest of a canonical JSON value.
pub fn canonical_digest<T: Serialize>(value: &T) -> Result<ChainHash, ChainError> {
    let bytes = canonicalize(value)?;
    Ok(ChainHash(Sha256::digest(bytes).into()))
}

/// Computes one chain entry hash using the versioned preimage contract.
#[must_use]
pub fn hash_chain_entry(
    previous_hash: ChainHash,
    entry_kind: &str,
    canonical_digest: ChainHash,
    raw_payload_digest: Option<ChainHash>,
) -> ChainHash {
    let kind_len = u16::try_from(entry_kind.len()).unwrap_or(u16::MAX);
    let raw_digest = raw_payload_digest.unwrap_or_else(ChainHash::zero);
    let mut hasher = Sha256::new();
    hasher.update(1_u16.to_be_bytes());
    hasher.update(previous_hash.0);
    hasher.update(kind_len.to_be_bytes());
    hasher.update(&entry_kind.as_bytes()[..usize::from(kind_len)]);
    hasher.update(canonical_digest.0);
    hasher.update(raw_digest.0);
    ChainHash(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::{ChainEntry, ChainHash, ChainVerifier, canonical_digest, hash_chain_entry};

    #[derive(Serialize)]
    struct Value {
        z: u32,
        a: &'static str,
    }

    #[test]
    fn canonical_digest_is_stable() {
        let first = canonical_digest(&Value { z: 2, a: "one" }).expect("first");
        let second = canonical_digest(&Value { z: 2, a: "one" }).expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn verifier_detects_changed_entry() {
        let canonical = canonical_digest(&Value { z: 1, a: "entry" }).expect("digest");
        let entry_hash = hash_chain_entry(ChainHash::zero(), "source_event", canonical, None);
        let entry = ChainEntry {
            sequence: 1,
            entry_kind: "source_event".to_owned(),
            target_id: "event-1".to_owned(),
            canonical_digest: canonical,
            raw_payload_digest: None,
            previous_hash: ChainHash::zero(),
            entry_hash,
        };
        assert!(ChainVerifier::verify(std::slice::from_ref(&entry)).is_valid());

        let mut changed = entry;
        changed.canonical_digest = ChainHash([9_u8; 32]);
        let report = ChainVerifier::verify(&[changed]);
        assert_eq!(report.changed_entries, vec![1]);
    }
}
