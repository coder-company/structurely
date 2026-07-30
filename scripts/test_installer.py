#!/usr/bin/env python3
"""Exercise the native Unix installer against an isolated local release."""

from __future__ import annotations

import argparse
import hashlib
import http.server
import os
import subprocess
import tempfile
import threading
from pathlib import Path


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format: str, *args: object) -> None:
        pass


def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        check=True,
        text=True,
        capture_output=True,
        **kwargs,
    )


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        raise SystemExit(f"release binary does not exist: {binary}")

    with tempfile.TemporaryDirectory(prefix="structurely-installer-") as temporary:
        root = Path(temporary)
        release = root / "release"
        install = root / "install"
        release.mkdir()
        run(
            [
                "sh",
                "scripts/package-unix.sh",
                str(binary),
                "linux-x86_64",
                str(release),
            ]
        )
        archive = release / "structurely-linux-x86_64.tar.gz"
        checksum = sha256(archive)
        (release / "SHA256SUMS").write_text(
            f"{checksum}  {archive.name}\n", encoding="utf-8"
        )

        handler = lambda *values: QuietHandler(  # noqa: E731
            *values, directory=str(release)
        )
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            environment = {
                **os.environ,
                "STRUCTURELY_RELEASE_BASE_URL": (
                    f"http://127.0.0.1:{server.server_port}"
                ),
                "STRUCTURELY_INSTALL_DIR": str(install),
            }
            installed = run(["sh", "scripts/install.sh"], env=environment)
            assert "Installed structurely " in installed.stdout
            destination = install / "structurely"
            assert destination.is_file()
            version = run([str(destination), "--version"]).stdout.strip()
            assert version == run([str(binary), "--version"]).stdout.strip()

            original = sha256(destination)
            (release / "SHA256SUMS").write_text(
                f"{'0' * 64}  {archive.name}\n", encoding="utf-8"
            )
            rejected = subprocess.run(
                ["sh", "scripts/install.sh"],
                check=False,
                text=True,
                capture_output=True,
                env=environment,
            )
            assert rejected.returncode != 0
            assert "Checksum verification failed" in rejected.stderr
            assert sha256(destination) == original
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    print("native installer round-trip and checksum-failure preservation passed")


if __name__ == "__main__":
    main()
