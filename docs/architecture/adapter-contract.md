# Adapter Contract

First-party adapters implement one asynchronous trait for descriptor, detection,
capability reporting, source discovery, historical import, live tail, and optional
hook planning/install/uninstall.

## Descriptor and Capabilities

Every descriptor contains a stable ID, display name, icon, supported version probes,
known source locations, capability set, adapter schema version, upstream documentation
link, and whether richer capture requires a configuration change.

Capabilities are explicit per feature: historical sessions, live sessions, prompts,
responses, tool calls, commands, file reads, file writes, subagents, tokens, cost
metadata, process ancestry, host-observed writes/reads/network, and recovery
snapshots. Missing evidence is displayed as unavailable, not as zero activity.

## Import and Tail

- Discovery scans known locations, not the entire home directory.
- A source fingerprint uses stable identity, size, modification time, and any native
  identity.
- Cursors advance in the same transaction as imported events.
- Import is idempotent, resumable, and oldest-to-newest within a source.
- Unparseable records are quarantined with source offset and error while other records
  continue.
- Live tail uses notifications plus periodic cursor verification and handles append,
  rotation, truncation, sleep/resume, and source creation.
- Agent databases are opened read-only or through a consistent snapshot. The daemon
  never holds a writer lock on an agent's database.

## Required Conformance Cases

Adapters must prove idempotent import, appended records, partial final records,
truncation, rotation/replacement, duplicate IDs, unknown fields, invalid Unicode,
oversized payload handling, redaction-before-indexing, timestamp normalization,
missing explicit session end, source-version drift, and no source-file mutation during
read-only import.

## Hook Safety

Hooks are optional and require explicit consent. The plan identifies the exact files
and capability improvement. Installation parses and merges structurally, preserves
unrelated settings, backs up the original, tags AgentTraceback-owned entries with a stable
ID, validates the merged result, and replaces the original atomically. Uninstall
removes only owned entries. User conflicts stop execution.
