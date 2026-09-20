# Contributing

AgentTraceback follows the technical contract in
[`AGENTTRACEBACK_IMPLEMENTATION_SPEC.md`](AGENTTRACEBACK_IMPLEMENTATION_SPEC.md). Changes that
replace the locked stack, weaken evidence semantics, add cloud dependencies, or pull
post-v0.1 deep-capture features into the release are out of scope.

## Development

1. Install stable Rust, Node.js 24 LTS, pnpm 10, and the platform Tauri prerequisites.
2. Run `pnpm install`.
3. Run `cargo run -p xtask -- doctor`.
4. Run `cargo run -p xtask -- ci` before submitting a change.

Work milestone by milestone. A milestone is complete only when its acceptance tests
pass and the application remains runnable.

## Pull Requests

- Keep changes focused and explain the user-visible outcome.
- Add tests for behavior changes and regression fixes.
- Update an ADR when a low-level implementation choice needs to be recorded.
- Never modify a released database migration; add a forward-only migration.
- Never include generated data, secrets, real agent history, or unsanitized fixtures.
- Preserve owner-only permissions and local-first behavior.

## Adapter Fixtures

Fixtures must be synthetic, sanitized, and structurally faithful to the source
format. Include malformed and unknown-field cases where the adapter contract requires
them. Do not copy a real user's prompt, response, source code, or credentials into the
repository.

## Commit Hygiene

Use clear commit messages that state the behavior changed. Do not rewrite another
contributor's history without coordination. Generated lockfiles and approved database
migrations are committed.

## License

By contributing, you agree that your contribution is licensed under Apache-2.0.
