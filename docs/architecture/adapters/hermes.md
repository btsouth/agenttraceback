# Hermes Agent Adapter

Source: `~/.hermes/state.db`, opened read-only with SQLite `NO_MUTEX`.

The cursor is the highest successfully imported `messages.id`. Sessions join message
metadata including cwd, title, model, parent session, and timestamps. Malformed tool
call JSON is quarantined without blocking message import. Oversized records are
quarantined and reported through warnings.
