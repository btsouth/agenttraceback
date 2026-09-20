# ADR 0007: Daemon-Owned Wrapper Lifecycle

- Status: Accepted
- Date: 2026-09-20

## Context

`agenttraceback run` must preserve an interactive terminal while the daemon owns
project baselines, observers, session records, and final evidence. A CLI that wrote
directly to SQLite or independently normalized observations would duplicate runtime
logic and bypass the single-writer boundary.

## Decision

The CLI prepares a wrapper session through the authenticated loopback API, launches
the command in a portable PTY, reports the root child PID and optional transcript
chunks back to the daemon, then requests completion with the exact exit status. The
daemon owns the session row, baseline, Git boundaries, filesystem observer, process
observer, event filtering, attribution, chain finalization, and capture health.

## Consequences

The terminal remains CLI-local for latency and behavior preservation while recording
state remains daemon-owned. A daemon failure before launch is a hard failure rather
than an unrecorded claim of success. Completion flushes pending watcher events before
the session chain is finalized.
