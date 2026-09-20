# Claude Code Adapter

Source root: `~/.claude/projects`, recursively scanned for `.jsonl` session logs.

The parser normalizes user prompts, assistant messages, tool calls, and tool results.
File and command actions remain `REPORTED` because the log is agent-controlled.

Optional hooks use `~/.claude/settings.json`. Planning shows the exact merged JSON
and digest. Installation checks the before-hash, creates a timestamped backup, tags
AgentTraceback-owned entries with `_agenttracebackId`, and writes atomically.
Uninstall removes only owned entries and preserves unrelated configuration.
