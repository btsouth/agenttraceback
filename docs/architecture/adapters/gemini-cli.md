# Gemini CLI Adapter

Source root: `~/.gemini/tmp`, recursively scanned for JSON chat files.

The adapter normalizes top-level or wrapped message arrays, user/model roles,
timestamps, responses, and function calls. Unknown fields are ignored. The cursor is
the next message index. A shorter source restarts import and emits a truncation
warning; large batches are bounded.
