# Threat Model

## Assets

- Prompt, response, tool-call, terminal, source-file, and diff content.
- API tokens, master keys, signing material, and keyed redaction digests.
- Session provenance, correlation records, event hash chains, and chain roots.
- Project files and recovery snapshots.
- User decisions such as approved hooks, annotations, and recovery plans.

## Trust Boundaries

- Agent-controlled logs, hooks, terminal output, and repository content are untrusted.
- The local API is reachable by other local processes unless authenticated.
- Browser-origin requests can be initiated by any local or remote page.
- Other local OS users are outside the application account's privacy boundary.
- Filesystem paths and symlinks can change between validation and use.
- Update metadata and artifacts cross the network boundary and must be verified.

## Required Threats and Controls

| Threat | Required control |
| --- | --- |
| Malformed agent log or source mutation | Bounded parser, read-only import, quarantine, fixture tests |
| Agent-crafted HTML or terminal sequences | No raw HTML, bounded ANSI sanitizer, restrictive CSP |
| Path traversal or symlink escape | Canonical containment and immediate destination revalidation |
| Hostile browser calls loopback API | Bearer token, strict Host and Origin checks |
| Another local user reads state | Owner-only permissions/ACLs and encrypted payloads |
| Evidence tampering | Append-only SHA-256 chain and verification status |
| Secret enters search/log/export | Redact before persistence/indexing plus seeded-secret tests |
| Hook configuration corruption | Structural merge, backup, atomic replace, reversible ownership tags |
| Untrusted repository content | Ignore rules, size bounds, safe rendering, no execution |
| Unknown or compromised update | Signed metadata and artifact verification |

## Explicit Non-Guarantees in Standard Mode

AgentTraceback v0.1 does not guarantee every file read, every short-lived child process,
every network connection, or the writing PID for every filesystem notification. It
does not block or sandbox an agent. Deep Capture is post-v0.1 and must not be implied
by standard-mode UI or documentation.

## Security Review Checklist

Before v0.1, verify loopback authentication and origin rejection, owner-only
permissions, encrypted payload and blob round trips, redaction leakage tests,
snapshot/recovery containment, symlink races, parser bounds, dependency audit,
update-signature verification, and database migration backup/recovery.
