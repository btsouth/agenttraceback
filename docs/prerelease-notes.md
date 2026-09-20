This is an experimental prerelease for evaluation. Stable v0.1.0 release gates
remain open, including the seven-day soak and packaged installation, uninstall,
and reboot tests across supported platforms.

Installers may be unsigned. Windows may show an unknown-publisher warning, and
macOS builds may lack Developer ID signing and notarization. Do not disable
platform security controls to install these builds. Use a source build if your
platform rejects an installer. SHA-256 checksum files accompany the artifacts.

Signed automatic updates are not a supported distribution path for this alpha.
Build-time updater hooks exist, but published update metadata and end-to-end
signature verification remain release gates. See `docs/release.md` for details.

The Windows MSI uses numeric installer version `0.0.2` for this alpha; the application reports `0.1.0-alpha.2`. This keeps the installer version below the eventual stable `0.1.0` upgrade.
