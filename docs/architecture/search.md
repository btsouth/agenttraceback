# Search

Normal search reads only redacted previews and normalized metadata. It never decrypts
raw payloads to serve a query.

## Syntax

Text terms combine with implicit AND. Quotes preserve phrases and a leading `-`
negates a term or structured filter. Supported filters are `agent`, `model`,
`project`, `type`, `path`, `risk`, `evidence`, `status`, `session`, `after`, and
`before`.

```text
agent:hermes model:"deepseek-v4.1-flash" path:"src/auth.ts"
risk:high -agent:codex failed
after:2026-09-01 before:2026-10-01 session:01a0
```

## Implementation

The parser produces a typed query. Text terms become an FTS5 MATCH expression against
the redacted FTS table. Structured filters become parameterized SQL over ordinary
indexes. Results use BM25 rank, descending event time, and stable event ID ordering.

Session titles and project names are maintained in FTS through insertion triggers.
Deleting or mutating event rows is blocked by append-only triggers.
