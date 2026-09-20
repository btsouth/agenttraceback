# Release Process

## Versioning

AgentTraceback uses Semantic Versioning. During `0.x`, internal APIs may change, but database
migrations remain forward-only and data-preserving. Event schema, local API version,
and adapter format support are independent version axes.

## Release Gates

Do not publish `v0.1.0` until every functional, safety/privacy, reliability/performance,
and product-quality gate in the implementation specification passes. Unsigned beta
artifacts may be published only with explicit platform warnings and SHA-256 checksums.
Platform security controls must not be disabled to make an artifact install.

## Required Credentials

The release workflow can build unsigned development artifacts without credentials.
Production release activation requires:

- Apple Developer signing and notarization credentials.
- Windows code-signing certificate or trusted signing service.
- Tauri updater signing key stored as a release secret.

Credentials are external inputs. Missing credentials are reported as a release
limitation; they are never faked or committed.

## Release Checklist

1. Confirm every v0.1 release gate has current evidence.
2. Run `cargo run -p xtask -- ci` on a clean checkout.
3. Run packaging and installation smoke tests on all supported platforms.
4. Verify the migration backup and recovery procedure.
5. Confirm redacted exports contain no seeded secrets.
6. Generate checksums and platform package metadata.
7. Publish release notes with migrations, adapter changes, capture gaps, and security
   fixes.
8. Verify signed update metadata and artifact signatures before enabling publication.

## Current Build Commands

```bash
# Full CI including Playwright against a real temporary daemon.
cargo run -p xtask -- ci

# Storage/search benchmark.
cargo run -p xtask -- benchmark --events 1000000

# Installer bundles for the current platform plus the bundled daemon binary.
cargo run -p xtask -- package

# Consistent local database backup and verified restore.
agenttraceback daemon backup
agenttraceback daemon restore /path/to/backup.db --confirm

# Optional per-user startup registration.
agenttraceback daemon startup install
agenttraceback daemon startup status
agenttraceback daemon startup remove

# Redacted support diagnostics.
agenttraceback doctor --json --output support-bundle.json
```

## Updater Inputs

The release workflow enables Tauri updater artifacts only when both
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_UPDATER_PUBLIC_KEY` are configured. The
public key is injected into the build configuration and the private key is never
written to the repository. If either input is absent, the workflow produces unsigned
development installers and reports that limitation instead of faking signatures.

On rolling-release development hosts where Tauri's bundled `linuxdeploy` cannot strip
newer RELR sections, `cargo run -p xtask -- package` emits `.deb` and `.rpm` bundles
with the CLI and daemon sidecars and prints an explicit AppImage skip warning. The
Ubuntu release workflow still attempts the AppImage build in its supported runner
environment.

## Known Release-Candidate Limitations

- The seven-day mixed-session soak has not yet been run to completion in this
  repository revision.
- Integrity chains are unsigned internal-consistency checks. They do not protect
  against wholesale rewriting by a process with the same OS-account access.
  See [Storage and Integrity](architecture/storage.md).
- Packaged install/uninstall/reboot smoke tests require Windows, macOS, and Linux
  hosts and credentials; source builds and Tauri bundle configuration are present.
- Full export is opt-in only (`--full`), contains explicitly decrypted eligible
  content, and is never the default. Redacted JSON and Markdown remain covered by
  seeded-secret tests.
- Process and Git tabs use persisted event projections and show honest degraded or
  empty states when the source did not provide those records.
