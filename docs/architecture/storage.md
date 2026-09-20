# Storage and Integrity

The event store uses SQLite in WAL mode with foreign keys enabled, a five-second busy
timeout, and `synchronous=NORMAL` for capture. Schema migrations temporarily use
`synchronous=FULL` and take a pre-migration backup when upgrading an existing schema.

## Write Path

One writer thread receives bounded commands. Event batches of up to 1,000 rows run in
an immediate transaction. For each source event, the writer validates the envelope,
detects an existing active source identity, returns an unchanged duplicate, inserts a
superseding event after a changed re-parse, computes the RFC 8785 canonical digest,
advances the session or daemon-global chain, inserts event/source/session/chain rows
atomically, and updates encrypted-blob reference counts.

The `events` table is protected by append-only triggers. Corrections create new rows
and relationships.

## Blob Path

The blob store validates plaintext size, compresses when beneficial, encrypts with
XChaCha20-Poly1305, verifies the BLAKE3 digest, and writes through a temporary file
followed by an atomic rename. Existing content is authenticated and compared before
deduplication.

Blob metadata records plaintext and encrypted sizes, media category, retention class,
state, and expiration. The current implementation has no garbage
collector; retention and unreferenced-blob cleanup arrive with the recovery and
storage-management milestones.

## Verification

`Store::verify_chain` reads a session or global chain in sequence and checks missing
sequence positions, reordered sequence numbers or previous hashes, recomputed chain
entry hashes, canonical event digests against persisted envelopes, and missing event
rows or encrypted payload metadata referenced by chain entries. If a chain root has
been finalized, the verifier also checks its entry count and root hash against the
current chain tail.

Signed roots remain later work. The Ed25519 root-signing interface exists but is
intentionally not enabled in v0.1.
