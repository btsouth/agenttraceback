# ADR 0002: Authenticated Loopback API

- Status: Accepted
- Date: 2026-09-20

## Context

The desktop application and CLI need access to daemon state without opening a LAN
service or creating an account. Local web content is also a hostile origin.

## Decision

Bind Axum to `127.0.0.1` on an ephemeral port. Generate a 256-bit random token on
first run, require it as a bearer token, validate the HTTP `Host` header against the
active endpoint, and allow browser `Origin` values only for Tauri and the fixed local
development origin.

Runtime connection metadata contains the PID, port, token, daemon version, and API
version. It is written atomically with owner-only permissions on Unix. Secrets are
never read from bootstrap TOML.

## Consequences

CLI clients must read runtime metadata from the platform runtime directory. The
desktop application makes API calls in Rust through Tauri instead of exposing a
browser fetch path. Token rotation must update the runtime file without restarting
unrelated daemon subsystems.
