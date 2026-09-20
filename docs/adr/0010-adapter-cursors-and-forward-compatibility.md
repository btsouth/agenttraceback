# ADR 0010: Adapter Cursors And Forward Compatibility

Adapters persist an opaque native cursor alongside source size and modification time.
File sources use bounded JSONL batches and a stable consumed-prefix digest so append,
truncation, and replacement are distinguishable in common cases. SQLite sources use
monotonic row identities and never hold a writer lock.

Unknown fields are ignored. Malformed records are quarantined and reported without
blocking later valid records. A format change may degrade one source, but it cannot
corrupt prior data or cause silent duplication.
