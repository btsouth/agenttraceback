# ADR 0001: Rust and pnpm Monorepo

- Status: Accepted
- Date: 2026-09-20

## Context

AgentTraceback needs a cross-platform recorder daemon, CLI, reusable Rust core crates, and a
Tauri desktop frontend. These components share protocol and domain contracts but have
different build and release needs.

## Decision

Use one repository with:

- A Rust 2024 Cargo workspace for all daemon, CLI, core, adapter, platform, and desktop
  backend crates.
- A pnpm workspace for the React desktop frontend.
- A single `xtask` binary for repeatable repository checks and packaging commands.
- Committed `Cargo.lock` and `pnpm-lock.yaml` files.

Core crates do not depend on Tauri, React, or platform UI code.

## Consequences

Protocol changes can be made atomically across the daemon, CLI, and desktop. CI is
larger because it must build both toolchains. The dependency direction and crate
ownership table in the implementation specification remain mandatory.
