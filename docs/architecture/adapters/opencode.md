# OpenCode Adapter

Source: `~/.local/share/opencode/opencode.db`, opened read-only.

The cursor is `time_created:message_id`. Messages and parts are normalized into
prompts, responses, text, commands, file operations, and generic tool calls.
Malformed or oversized message/part records are quarantined while the cursor still
advances, preventing a single bad row from blocking historical import.
