# ADR 0005: Encrypted Content-Addressed Blobs

- Status: Accepted
- Date: 2026-09-20

## Context

Prompts, responses, terminal streams, file content, and raw source records can include
secrets or proprietary source code. They must not be stored as plaintext metadata or
indexed for search. Recovery and evidence features also require content identity.

## Decision

Identify plaintext by BLAKE3. Compress with Zstandard only when it reduces size, then
encrypt with XChaCha20-Poly1305 using a random 24-byte nonce. Store a versioned header
containing format version, compression mode, nonce, plaintext length, and digest. The
header is authenticated as additional data.

Derive the encryption subkey with HKDF-SHA-256 from the installation master key.
Persist the master key in macOS Keychain, Windows Credential Manager, or Linux Secret
Service. If Linux Secret Service is unavailable, use an owner-only key file and report
`Device permissions only` in health/UI.

## Consequences

Identical plaintext deduplicates by digest without revealing content. Any header,
ciphertext, key, or plaintext-length tampering fails authentication. Key loss makes
payloads unrecoverable, so key-protection status is visible and recovery/export paths
must test decryptability explicitly.
