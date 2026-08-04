#!/usr/bin/env python3
"""Exercise the native Unix installer against an isolated local release."""

from __future__ import annotations

import argparse
import hashlib
import http.server
import os
import platform
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


PLATFORMS = {
    "linux-x86_64": ("Linux", "x86_64"),
    "macos-aarch64": ("Darwin", "arm64"),
    "macos-x86_64": ("Darwin", "x86_64"),
}


def native_platform() -> str:
    system = platform.system()
    machine = platform.machine().lower()
    normalized = "aarch64" if machine in {"arm64", "aarch64"} else "x86_64"
    product = "macos" if system == "Darwin" else "linux" if system == "Linux" else ""
    candidate = f"{product}-{normalized}"
    if candidate not in PLATFORMS:
        raise SystemExit(f"unsupported Unix installer test host: {system}/{machine}")
    return candidate


def publish_release(binary: Path, release: Path, target: str) -> Path:
    run(
        [
            "sh",
            "scripts/package-unix.sh",
            str(binary),
            target,
            str(release),
        ]
    )
    archive = release / f"structurely-{target}.tar.gz"
    (release / "SHA256SUMS").write_text(
        f"{sha256(archive)}  {archive.name}\n", encoding="utf-8"
    )
    return archive


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--platform", choices=sorted(PLATFORMS), default=native_platform())
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        raise SystemExit(f"release binary does not exist: {binary}")

    with tempfile.TemporaryDirectory(prefix="structurely-installer-") as temporary:
        root = Path(temporary)
        release = root / "release"
        install = root / "install"
        release.mkdir()
        archive = publish_release(binary, release, args.platform)

        handler = lambda *values: QuietHandler(  # noqa: E731
            *values, directory=str(release)
        )
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            environment = {
                **os.environ,
                "STRUCTURELY_OS": PLATFORMS[args.platform][0],
                "STRUCTURELY_ARCH": PLATFORMS[args.platform][1],
                "STRUCTURELY_RELEASE_BASE_URL": (
                    f"http://127.0.0.1:{server.server_port}"
                ),
                "STRUCTURELY_INSTALL_DIR": str(install),
            }
            installed = run(["sh", "scripts/install.sh"], env=environment)
            for stage in [
                "[1/4] Detect platform",
                "[2/4] Download release",
                "[3/4] Verify and stage",
                "[4/4] Install atomically",
            ]:
                assert stage in installed.stdout
            assert "verified SHA-256 checksum" in installed.stdout
            assert "Structurely is ready." in installed.stdout
            assert "Start in a repository" in installed.stdout
            assert "structurely setup codex" in installed.stdout
            assert "\x1b" not in installed.stdout
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
            publish_release(fake, release, args.platform)

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
            assert "Dashboard deployment" in cloudflare.stdout
            assert "Static shell only; no repository data" in cloudflare.stdout
            assert "Authenticated provider CLI already installed" in cloudflare.stdout
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
            assert "Structurely remains installed" in failed.stderr
            assert (install / "structurely").is_file()

            local = run(
                ["sh", "scripts/install.sh"],
                env={
                    **fake_environment,
                    "STRUCTURELY_DASHBOARD_SETUP": "local",
                },
            )
            assert "structurely dashboard start" in local.stdout

            unsupported = subprocess.run(
                ["sh", "scripts/install.sh"],
                check=False,
                text=True,
                capture_output=True,
                env={**environment, "STRUCTURELY_OS": "Plan9"},
            )
            assert unsupported.returncode != 0
            assert "No native release is available for Plan9" in unsupported.stderr

            invalid_dashboard = run(
                ["sh", "scripts/install.sh"],
                env={
                    **fake_environment,
                    "STRUCTURELY_DASHBOARD_SETUP": "unknown-provider",
                },
            )
            assert "Ignoring STRUCTURELY_DASHBOARD_SETUP" in (
                invalid_dashboard.stderr
            )

            powershell = Path("install.ps1").read_text(encoding="utf-8")
            for contract in [
                'Write-Step 1 "Detect platform"',
                'Write-Step 2 "Download release"',
                'Write-Step 3 "Verify and stage"',
                'Write-Step 4 "Install atomically"',
                "The existing installation was not changed",
                "Static shell only; no repository data",
                "Structurely is ready.",
                "STRUCTURELY_NO_COLOR",
            ]:
                assert contract in powershell
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    print(
        "native installer round-trip, preservation, and dashboard setup contract passed"
    )


if __name__ == "__main__":
    main()
