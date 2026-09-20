# ADR 0006: Structured Search Before FTS5

- Status: Accepted
- Date: 2026-09-20

## Context

Search must combine human terms with filters such as agent, model, project, action,
path, risk, evidence, status, session, and time while never indexing unredacted
payloads.

## Decision

Parse structured query syntax into typed terms and filters before constructing SQL.
Persist only redacted previews and normalized metadata in an FTS5 table. Apply
structured filters through parameterized SQL and ordinary indexes. Rank text results
with BM25 and order ties by event timestamp and stable ID.

## Consequences

Query syntax errors return a typed validation error rather than malformed SQL. FTS
contains no raw encrypted content and never requires decrypting every payload. New
filters require a parser addition and an indexed query path. Raw-content search
remains deferred.
