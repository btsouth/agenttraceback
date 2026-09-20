# Codex CLI Adapter

Source root: `~/.codex/sessions`, recursively scanned for rollout `.jsonl` files.

The parser handles `session_meta`, `event_msg`, and `response_item` records,
including prompts, responses, shell calls, `apply_patch` writes, and tool results.
Partial final lines are not consumed. A stable source-prefix digest detects file
replacement; truncation restarts from byte zero. Raw source records remain
`REPORTED`.
