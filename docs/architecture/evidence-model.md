# Evidence Model

## Source Events

A source event is an immutable normalized record from one authority. Its stored
evidence class is `reported` or `observed`.

| Source kind | Stored class | Trust interpretation |
| --- | --- | --- |
| `agent_log`, `agent_hook` | `reported` | Agent-controlled claim |
| `wrapper` | `observed` | Wrapper launch or PTY stream |
| `filesystem_observer` | `observed` | Host filesystem effect |
| `process_observer` | `observed` | Host process observation |
| `git_observer` | `observed` | Read-only Git state |
| `user_action` | `observed` | Action taken inside AgentTraceback |
| `policy_engine` | inherited | Cites the rule's underlying source events |
| `importer` | inherited | Describes ingestion path, not authority |

Source events are never upgraded in place. They retain the original source kind even
when transported through the importer.

## Logical Events and Verification

A logical timeline event projects one source event or a correlated group. The UI shows
`VERIFIED` only when an immutable correlation record joins at least one reported and
one observed source event under the active deterministic rule version.

Two agent-controlled records are not independent evidence. An adapter log and its
corresponding hook both remain reported evidence.

## Correlation Requirements

File mutation correlation requires the same normalized project/path, compatible action
family, a default two-second window, exact or high attribution confidence, matching
after-hash when both sides have one, and no equal-or-stronger conflicting candidate. A
batch can use a ten-second window only with a shared tool-call ID or exact process
subtree.

Command correlation compares executable and normalized arguments. Redacted values are
compared through keyed digests.

Conflicting sources are preserved, produce a `correlation_conflict`, and display a
mismatch warning. The host-observed effect can be the stronger machine-state
description without deleting the report.

## Attribution Confidence

- `exact`: stable session ID, process ancestry, or adapter event directly links it.
- `high`: one active session owns the scope and path/time/hash evidence agrees.
- `medium`: time and project scope agree but multiple writers are possible.
- `low`: heuristic association only.
- `unattributed`: real event that cannot be safely assigned.

Low-confidence evidence cannot become verified. Concurrent sessions in one project
must leave ambiguous writes observable but unattributed.

## Integrity

Normalized source events, correlation records, conflicts, retention tombstones, and
user annotations become canonical chain entries. Event rows and chain advancement are
committed in one transaction. Verification reports missing rows, reordered rows,
changed rows, and missing payloads separately.
