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


def publish_release(binary: Path, release: Path) -> Path:
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
    (release / "SHA256SUMS").write_text(
        f"{sha256(archive)}  {archive.name}\n", encoding="utf-8"
    )
    return archive


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
        archive = publish_release(binary, release)

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
            assert "Optional private dashboard" not in installed.stdout
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

            fake_log = root / "fake-dashboard.log"
            fake = root / "fake-structurely"
            fake.write_text(
                """#!/bin/sh
if [ "${1:-}" = "--version" ]; then
  echo "structurely 0.0.0-test"
  exit 0
fi
printf '%s\\n' "$*" >> "$STRUCTURELY_TEST_LOG"
if [ "${STRUCTURELY_TEST_DEPLOY_FAIL:-}" = "1" ]; then exit 42; fi
""",
                encoding="utf-8",
            )
            fake.chmod(0o755)
            publish_release(fake, release)

            fake_environment = {
                **environment,
                "STRUCTURELY_TEST_LOG": str(fake_log),
            }
            noninteractive = run(
                ["sh", "scripts/install.sh"], env=fake_environment
            )
            assert "Optional private dashboard" not in noninteractive.stdout
            assert not fake_log.exists()

            cloudflare = run(
                ["sh", "scripts/install.sh"],
                env={
                    **fake_environment,
                    "STRUCTURELY_DASHBOARD_SETUP": "cloudflare",
                },
            )
            assert "will not install npm packages" in cloudflare.stdout
            assert fake_log.read_text(encoding="utf-8").splitlines()[-1] == (
                "dashboard deploy cloudflare"
            )

            failed = run(
                ["sh", "scripts/install.sh"],
                env={
                    **fake_environment,
                    "STRUCTURELY_DASHBOARD_SETUP": "vercel",
                    "STRUCTURELY_TEST_DEPLOY_FAIL": "1",
                },
            )
            assert "Structurely itself remains installed" in failed.stderr
            assert (install / "structurely").is_file()

            local = run(
                ["sh", "scripts/install.sh"],
                env={
                    **fake_environment,
                    "STRUCTURELY_DASHBOARD_SETUP": "local",
                },
            )
            assert "structurely dashboard serve" in local.stdout
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    print(
        "native installer round-trip, preservation, and dashboard setup contract passed"
    )


if __name__ == "__main__":
    main()
