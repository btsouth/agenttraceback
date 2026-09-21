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

The Windows MSI uses numeric installer version `0.0.4` for this alpha; the application reports `0.1.0-alpha.4`. This keeps the installer version below the eventual stable `0.1.0` upgrade.

Alpha.2 fixes the desktop startup panic when updater configuration is absent. The
release workflow now launches the packaged Linux desktop and requires a visible
window plus a healthy daemon started through frontend IPC.

On the tested Omarchy/Arch machine with newer NVIDIA drivers, the AppImage also
requires the system Wayland client library to avoid a bundled-library/EGL conflict:

```bash
LD_PRELOAD=/usr/lib/libwayland-client.so.0 ~/Downloads/AgentTraceback_0.1.0-alpha.4_amd64.AppImage
```

This per-launch workaround changes no system files. It was verified locally with
the published AppImage; other Linux distributions can use different library paths.

Alpha.3 adds one-click recorded runs in a system terminal, a native project folder
picker, direct onboarding history import, and working clipboard controls. Capture
information no longer looks like switches that cannot be changed. Choose **Record
session** in the dashboard to launch more runs after onboarding.

Alpha.4 replaces per-agent import buttons with one daemon-owned background import.
Open the dashboard while all detected histories import, watch progress, and retry
unfinished sources together. JSONL histories continue past their first batch, and
Codex resumes without splitting session identity. Legacy services are updated by
the desktop when no recording is active. Closing the desktop does not cancel the
job; stopping the daemon ends it, and a later import resumes from saved cursors.
