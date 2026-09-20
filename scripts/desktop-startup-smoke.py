#!/usr/bin/env python3
"""Launch the packaged Linux desktop under Xvfb and require a working UI/daemon."""

import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import urllib.request


def main() -> None:
    appimage = Path(sys.argv[1]).resolve()
    appimage.chmod(appimage.stat().st_mode | 0o100)
    with tempfile.TemporaryDirectory(prefix="agenttraceback-desktop-smoke-") as temporary:
        root = Path(temporary)
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        environment = dict(os.environ)
        environment.update(
            AGENTTRACEBACK_DATA_DIR=str(root / "data"),
            AGENTTRACEBACK_CONFIG_DIR=str(root / "config"),
            XDG_RUNTIME_DIR=str(runtime),
            XDG_CACHE_HOME=str(root / "cache"),
            GDK_BACKEND="x11",
        )
        with (root / "desktop.log").open("w+") as log:
            process = subprocess.Popen(
                [str(appimage), "--appimage-extract-and-run"],
                env=environment,
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            try:
                deadline = time.monotonic() + 60
                ready_since = None
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise RuntimeError(f"Desktop exited during startup: {process.returncode}")
                    window = subprocess.run(
                        ["xdotool", "search", "--onlyvisible", "--name", "^AgentTraceback$"],
                        capture_output=True,
                        timeout=5,
                    )
                    metadata_path = runtime / "agenttraceback" / "runtime.json"
                    healthy = False
                    if window.returncode == 0 and metadata_path.exists():
                        metadata = json.loads(metadata_path.read_text())
                        # The frontend's initial IPC request starts the daemon.
                        # A window alone would miss a broken webview or IPC path.
                        try:
                            with urllib.request.urlopen(
                                urllib.request.Request(
                                    f"http://127.0.0.1:{metadata['port']}/api/v1/health",
                                    headers={"Authorization": f"Bearer {metadata['token']}"},
                                ), timeout=2
                            ) as response:
                                healthy = response.status == 200
                        except OSError:
                            pass
                    if healthy:
                        ready_since = ready_since or time.monotonic()
                        if time.monotonic() - ready_since >= 5:
                            print("packaged desktop: visible window and frontend-started daemon healthy")
                            return
                    else:
                        ready_since = None
                    time.sleep(0.5)
                raise RuntimeError("Desktop did not show a window and start a healthy daemon")
            except Exception:
                log.flush()
                log.seek(0)
                print(log.read(), file=sys.stderr)
                raise
            finally:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)


if __name__ == "__main__":
    main()
