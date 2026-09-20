use std::io::{Cursor, Read};

use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use rand::RngCore;
use thiserror::Error;

use crate::{KeyError, MasterKey};

const MAGIC: &[u8; 8] = b"ATBBLB01";
const FORMAT_VERSION: u16 = 1;
const HEADER_LEN: usize = 76;
const NONCE_OFFSET: usize = 20;
const NONCE_LEN: usize = 24;
const DIGEST_OFFSET: usize = 44;
const DIGEST_LEN: usize = 32;
const DEFAULT_MAX_PLAINTEXT_BYTES: usize = 64 * 1024 * 1024;

/// Compression mode recorded in a sealed blob header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BlobCompression {
    /// Plaintext bytes were stored without compression.
    None = 0,
    /// Plaintext bytes were compressed with Zstandard.
    Zstandard = 1,
}

impl BlobCompression {
    fn from_byte(value: u8) -> Result<Self, SecretError> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Zstandard),
            _ => Err(SecretError::UnsupportedCompression(value)),
        }
    }
}

/// A sealed blob plus its content identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SealedBlob {
    /// Encrypted, versioned file bytes.
    pub bytes: Vec<u8>,
    /// BLAKE3 digest of the plaintext.
    pub digest: [u8; 32],
    /// Plaintext byte length.
    pub plaintext_len: u64,
    /// Compression selected at write time.
    pub compression: BlobCompression,
}

/// Authenticated-encryption failures for content blobs.
#[derive(Debug, Error)]
pub enum SecretError {
    /// The blob header or ciphertext is malformed.
    #[error("blob is malformed: {0}")]
    Malformed(&'static str),
    /// The blob uses an unsupported compression mode.
    #[error("unsupported blob compression mode: {0}")]
    UnsupportedCompression(u8),
    /// Authentication failed, indicating tampering or a wrong key.
    #[error("blob authentication failed")]
    Authentication,
    /// Compression or decompression failed.
    #[error("blob compression failed")]
    Compression,
    /// The plaintext exceeds the configured safety limit.
    #[error("blob exceeds the configured plaintext limit")]
    TooLarge,
    /// The decrypted digest does not match the header.
    #[error("blob digest mismatch")]
    DigestMismatch,
    /// Key derivation failed.
    #[error(transparent)]
    Key(#[from] KeyError),
}

/// Encrypts and decrypts versioned content-addressed blobs.
#[derive(Clone, Debug)]
pub struct BlobCipher {
    cipher: XChaCha20Poly1305,
    max_plaintext_bytes: usize,
}

impl BlobCipher {
    /// Derives the blob encryption subkey from an installation master key.
    pub fn from_master_key(key: &MasterKey) -> Result<Self, SecretError> {
        let subkey = key.derive_subkey("agenttraceback-blob-encryption-v1")?;
        Ok(Self {
            cipher: XChaCha20Poly1305::new((&subkey).into()),
            max_plaintext_bytes: DEFAULT_MAX_PLAINTEXT_BYTES,
        })
    }

    /// Overrides the maximum accepted or produced plaintext size.
    #[must_use]
    pub const fn with_max_plaintext_bytes(mut self, limit: usize) -> Self {
        self.max_plaintext_bytes = limit;
        self
    }

    /// Compresses when beneficial, encrypts, and returns a versioned blob.
    pub fn seal(&self, plaintext: &[u8]) -> Result<SealedBlob, SecretError> {
        if plaintext.len() > self.max_plaintext_bytes {
            return Err(SecretError::TooLarge);
        }
        let compressed = zstd::stream::encode_all(Cursor::new(plaintext), 3)
            .map_err(|_| SecretError::Compression)?;
        let (compression, body) = if compressed.len() < plaintext.len() {
            (BlobCompression::Zstandard, compressed)
        } else {
            (BlobCompression::None, plaintext.to_vec())
        };

        let mut nonce = [0_u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce);
        let digest = *blake3::hash(plaintext).as_bytes();
        let plaintext_len = u64::try_from(plaintext.len())
            .map_err(|_| SecretError::Malformed("plaintext length overflow"))?;
        let header = build_header(compression, plaintext_len, &nonce, &digest);
        let nonce = XNonce::try_from(nonce.as_slice())
            .map_err(|_| SecretError::Malformed("invalid nonce"))?;
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &body,
                    aad: &header,
                },
            )
            .map_err(|_| SecretError::Authentication)?;
        let mut bytes = Vec::with_capacity(header.len() + ciphertext.len());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&ciphertext);
        Ok(SealedBlob {
            bytes,
            digest,
            plaintext_len,
            compression,
        })
    }

    /// Authenticates, decompresses, and verifies a sealed blob.
    pub fn open(&self, bytes: &[u8]) -> Result<Vec<u8>, SecretError> {
        if bytes.len() < HEADER_LEN || &bytes[..MAGIC.len()] != MAGIC {
            return Err(SecretError::Malformed("invalid header or magic"));
        }
        let version = u16::from_be_bytes(
            bytes[8..10]
                .try_into()
                .map_err(|_| SecretError::Malformed("missing format version"))?,
        );
        if version != FORMAT_VERSION {
            return Err(SecretError::Malformed("unsupported format version"));
        }
        let compression = BlobCompression::from_byte(bytes[10])?;
        if bytes[11] != 0 {
            return Err(SecretError::Malformed("reserved byte is not zero"));
        }
        let plaintext_len = u64::from_be_bytes(
            bytes[12..20]
                .try_into()
                .map_err(|_| SecretError::Malformed("missing plaintext length"))?,
        );
        let plaintext_len_usize = usize::try_from(plaintext_len)
            .map_err(|_| SecretError::Malformed("plaintext length overflow"))?;
        if plaintext_len_usize > self.max_plaintext_bytes {
            return Err(SecretError::TooLarge);
        }

        let header = &bytes[..HEADER_LEN];
        let nonce = &bytes[NONCE_OFFSET..NONCE_OFFSET + NONCE_LEN];
        let expected_digest = &bytes[DIGEST_OFFSET..DIGEST_OFFSET + DIGEST_LEN];
        let nonce = XNonce::try_from(nonce).map_err(|_| SecretError::Malformed("invalid nonce"))?;
        let plaintext = self
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &bytes[HEADER_LEN..],
                    aad: header,
                },
            )
            .map_err(|_| SecretError::Authentication)?;
        let plaintext = match compression {
            BlobCompression::None => plaintext,
            BlobCompression::Zstandard => {
                let decoder = zstd::stream::read::Decoder::new(Cursor::new(plaintext))
                    .map_err(|_| SecretError::Compression)?;
                let mut bounded = decoder.take(
                    u64::try_from(self.max_plaintext_bytes)
                        .unwrap_or(u64::MAX)
                        .saturating_add(1),
                );
                let mut output = Vec::with_capacity(plaintext_len_usize);
                bounded
                    .read_to_end(&mut output)
                    .map_err(|_| SecretError::Compression)?;
                if output.len() > self.max_plaintext_bytes {
                    return Err(SecretError::TooLarge);
                }
                output
            }
        };
        if plaintext.len() != plaintext_len_usize {
            return Err(SecretError::Malformed("plaintext length mismatch"));
        }
        if blake3::hash(&plaintext).as_bytes() != expected_digest {
            return Err(SecretError::DigestMismatch);
        }
        Ok(plaintext)
    }
}

fn build_header(
    compression: BlobCompression,
    plaintext_len: u64,
    nonce: &[u8; NONCE_LEN],
    digest: &[u8; DIGEST_LEN],
) -> [u8; HEADER_LEN] {
    let mut header = [0_u8; HEADER_LEN];
    header[..8].copy_from_slice(MAGIC);
    header[8..10].copy_from_slice(&FORMAT_VERSION.to_be_bytes());
    header[10] = compression as u8;
    header[12..20].copy_from_slice(&plaintext_len.to_be_bytes());
    header[NONCE_OFFSET..NONCE_OFFSET + NONCE_LEN].copy_from_slice(nonce);
    header[DIGEST_OFFSET..DIGEST_OFFSET + DIGEST_LEN].copy_from_slice(digest);
    header
}

#[cfg(test)]
mod tests {
    use super::{BlobCipher, BlobCompression, SecretError};
    use crate::MasterKey;

    fn cipher() -> BlobCipher {
        BlobCipher::from_master_key(&MasterKey::generate()).expect("cipher")
    }

    #[test]
    fn round_trip_preserves_bytes_and_digest() {
        let plaintext = b"hello agenttraceback\n".repeat(100);
        let cipher = cipher();
        let sealed = cipher.seal(&plaintext).expect("seal");
        assert_eq!(sealed.compression, BlobCompression::Zstandard);
        assert_eq!(sealed.plaintext_len, plaintext.len() as u64);
        assert_eq!(cipher.open(&sealed.bytes).expect("open"), plaintext);
        assert_eq!(sealed.digest, *blake3::hash(&plaintext).as_bytes());
    }

    #[test]
    fn random_bytes_are_stored_without_compression() {
        let plaintext: Vec<u8> = (0..=255).collect();
        let cipher = cipher();
        let sealed = cipher.seal(&plaintext).expect("seal");
        assert_eq!(sealed.compression, BlobCompression::None);
        assert_eq!(cipher.open(&sealed.bytes).expect("open"), plaintext);
    }

    #[test]
    fn tampering_is_rejected() {
        let cipher = cipher();
        let mut sealed = cipher.seal(b"authenticated content").expect("seal");
        let last = sealed.bytes.len() - 1;
        sealed.bytes[last] ^= 0x40;
        assert!(matches!(
            cipher.open(&sealed.bytes),
            Err(SecretError::Authentication)
        ));
    }

    #[test]
    fn wrong_key_is_rejected() {
        let sealed = cipher().seal(b"content").expect("seal");
        assert!(matches!(
            cipher().open(&sealed.bytes),
            Err(SecretError::Authentication)
        ));
    }
}
