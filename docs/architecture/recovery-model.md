# Recovery Model

Recovery is planning-first and non-destructive by default. Git HEAD is not equivalent
to a user's working tree, so recovery uses the recorded project manifest, including
eligible dirty, staged, and untracked files.

## Coverage States

- `exact`: a complete baseline finished before the first mutation.
- `partial`: useful manifest coverage exists but some pre-session state or timing is
  unknown.
- `metadata_only`: hashes and metadata exist, but content is not restorable.
- `unavailable`: recovery cannot reconstruct the requested state.

Only `exact` sessions may label the action `Reconstruct exact pre-session state`.

## Plan Contract

Every action builds an immutable plan before execution. The plan records the source
snapshot/session, destination, create/replace/delete/rename operations, expected
current hashes, recoverable and unrecoverable entries, symlink/permission changes,
conflicts, estimated bytes, and the backup snapshot required for an in-place action.

Execution accepts the plan digest. A changed or stale plan is rejected rather than
silently recalculated.

## Safe Actions

- Restore one file to a new path.
- Reconstruct a session's eligible pre-state into a new directory.
- Create a Git recovery branch/worktree from the recorded pre-session HEAD, then
  materialize recorded uncommitted state without committing by default.
- Export an inverse patch for text changes.

## Guarded In-Place Restore

In-place restore is advanced and never the default. Before writing, AgentTraceback
recomputes current hashes, marks changed files as conflicts, requires conflict
resolution or explicit per-file overwrite, creates an encrypted current-state backup,
uses atomic replacement where possible, and records every action as `user_action`
evidence.

If a file is labeled restorable, AgentTraceback must reproduce bytes matching the recorded
BLAKE3 digest or fail visibly. Partial bytes are never reported as success.

## Pre-restore Backups

Every in-place restore saves an encrypted backup plan before modifying files. The
run retains `backupPlanId` even if execution fails. The CLI prints that plan ID and
an undo command; it can also be retrieved through the recovery-run API. Execute
that plan with explicit confirmation to recover the previous contents. Undo itself
also creates a fresh backup. Files changed after backup capture are refused.

New-directory reconstruction requires an empty destination. Single-file recovery
never overwrites an existing destination. `--overwrite-conflicts` applies only to
in-place recovery, where the current contents are backed up first. Every source
blob and expected content hash is checked before writing any output.

Git recovery creates a worktree without checkout hooks or content filters, then
writes the captured files. A snapshot limited to a repository subdirectory must
use new-directory reconstruction instead. Project capture does not automatically
expand a selected subdirectory to the enclosing repository root.
