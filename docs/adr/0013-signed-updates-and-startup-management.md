# ADR 0013: Signed Updates And Startup Management

The Tauri updater is user initiated and disabled when no signed release endpoint is
configured. Release builds enable updater artifacts only when the signing private key
and updater public key are supplied externally; signatures are verified by the
updater plugin.

Daemon-at-login registration is per-user and reversible: an autostart desktop entry
on Linux, a LaunchAgent on macOS, or a current-user `Run` registry value on Windows.
The application never installs a machine-wide startup service silently.
