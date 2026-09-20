#!/usr/bin/env python3
"""Stage only distributable installers, command-line binaries, and completions."""

import argparse
import hashlib
from pathlib import Path
import shutil
import tarfile
import tempfile
import zipfile


def stage(target: str, name: str) -> None:
    output = Path("release-staged")
    output.mkdir(exist_ok=False)
    bundle = Path("target") / target / "release" / "bundle"
    suffixes = (".AppImage", ".deb", ".rpm", ".dmg", ".msi", ".exe", ".tar.gz", ".zip", ".sig")
    # Final assets live directly in bundle/<format>; deeper files are build
    # internals (for example Debian control.tar.gz and data.tar.gz).
    installers = [path for path in bundle.glob("*/*") if path.is_file() and path.name.endswith(suffixes)]
    if not installers:
        raise RuntimeError(f"No distributable installers found in {bundle}")
    for source in installers:
        destination = output / source.name
        if destination.exists():
            raise RuntimeError(f"Duplicate release asset name: {source.name}")
        shutil.copy2(source, destination)

    windows = target.endswith("windows-msvc")
    extension = ".exe" if windows else ""
    with tempfile.TemporaryDirectory() as temporary:
        archive_root = Path(temporary) / f"agenttraceback-{name}"
        archive_root.mkdir()
        for binary in ("agenttraceback", "agenttracebackd"):
            if target == "universal-apple-darwin":
                source = Path("apps/desktop/src-tauri/binaries") / f"{binary}-{target}"
            else:
                source = Path("target") / target / "release" / f"{binary}{extension}"
            shutil.copy2(source, archive_root / f"{binary}{extension}")
        shutil.copytree("release-completions", archive_root / "completions")
        if windows:
            with zipfile.ZipFile(output / f"{archive_root.name}.zip", "w", zipfile.ZIP_DEFLATED) as archive:
                for source in sorted(archive_root.rglob("*")):
                    if source.is_file():
                        archive.write(source, source.relative_to(archive_root.parent))
        else:
            with tarfile.open(output / f"{archive_root.name}.tar.gz", "w:gz") as archive:
                archive.add(archive_root, arcname=archive_root.name)

    checksums = []
    for asset in sorted(output.iterdir()):
        with asset.open("rb") as stream:
            digest = hashlib.sha256()
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        checksums.append(f"{digest.hexdigest()}  {asset.name}\n")
    (output / f"{name}-SHA256SUMS").write_text("".join(checksums), encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target")
    parser.add_argument("name")
    args = parser.parse_args()
    stage(args.target, args.name)
