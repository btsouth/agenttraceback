# Standard Capture

Standard mode uses no administrator privilege, kernel driver, security extension,
network observer, or blocking policy.

## Generic Wrapper

`agenttraceback run [options] -- <command> [args...]` asks the daemon to create a
session and baseline, launches the exact argv in a cross-platform PTY, forwards
stdin/stdout, polls terminal size for resize propagation, and preserves the child
exit code. The daemon receives the root PID, optional encrypted transcript chunks,
and final exit status.

The wrapper does not use shell concatenation. It supports the current working
directory, environment, interactive input, output color, and resize behavior through
`portable-pty` and the active terminal.

## Project Observation

One recursive native watcher observes each active project root. Notifications are
drained every 100 ms; repeated modify events are coalesced while create, delete, and
rename lifecycle events remain ordered. Ignore rules are applied before persistence.

Filesystem notifications carry no reliable writer PID. If exactly one session is
active for the project, the event receives high-confidence session attribution. If
multiple sessions are active, it remains observed and unattributed.

## Process Observation

A 100 ms best-effort process scan runs while sessions are active. The daemon tracks
PID plus process start time, parent relationships, executable, argv preview, cwd,
and process lifecycle. Events are exact only when the process belongs to a registered
wrapper root's observed subtree. Short-lived descendants can be missed and are
reported as a standard-mode limitation.

## Filesystem Reconciliation

Each session saves a baseline manifest before launch. Watcher overflow emits
`recorder_gap`, then captures a new manifest and compares it with the previous one.
Create, modify, and delete differences become observed reconciliation events. This
recovers final state but cannot prove precise timing or process attribution.

## Git Boundaries

For Git projects, the daemon invokes read-only Git commands before and after the
session for HEAD, branch, upstream, porcelain v2 status, and submodules. Status
payloads are encrypted in the blob store; normalized Git metadata remains queryable.
