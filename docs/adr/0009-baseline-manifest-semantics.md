# ADR 0009: Baseline Manifest Semantics

- Status: Accepted
- Date: 2026-09-20

## Context

Git HEAD does not represent the user's pre-session working tree. Recovery planning
needs deterministic metadata and content hashes for tracked, dirty, staged, and
untracked files without following symlinks outside the project.

## Decision

Capture a recursive manifest before the wrapper child starts. Include Git-tracked
files, non-ignored project files, dirty and untracked files, symlink targets, hashes,
sizes, executable bits, and capture status. Built-in generated-directory exclusions
may be overridden by Git tracking; `.agenttracebackignore` rules cannot. Do not
follow symlinks, enforce a 20 MiB content-capture threshold, and bound scans at
250,000 candidates.

`--no-recovery-snapshot` is an explicit metadata-only fast path. A discovered
passively appended source with no completed baseline cannot claim exact recovery.

## Consequences

Every exact recovery claim has a manifest boundary. Large or sensitive files may be
hash-only and are explicitly marked metadata-only. Milestone 3 materializes encrypted
pre/post file versions and recovery plans on top of this manifest contract.
