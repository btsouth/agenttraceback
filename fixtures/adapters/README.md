# Adapter Fixtures

Sanitized, synthetic, structurally faithful fixtures belong here. Each supported
adapter needs minimal, tool-heavy, subagent, interrupted, large, version-variant,
unknown-field, and malformed/truncated cases as applicable.

Never commit real prompts, source code, paths, identifiers, terminal output, or
credentials.
# Adapter Fixtures

Fixtures in this directory are synthetic, contain no credentials, and model source
shapes rather than captured user data. They cover the five semantic v0.1 adapters and
the Command Code detection-only profile.

- `claude-code/minimal.jsonl`
- `codex/minimal.jsonl`
- `gemini/minimal.json`
- `hermes/minimal.sql`
- `opencode/minimal.sql`
- `command-code/detection.md`

The adapter conformance tests additionally exercise malformed records, unknown fields,
partial final lines, truncation/replacement, and restart idempotency.
