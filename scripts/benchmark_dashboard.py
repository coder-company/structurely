#!/usr/bin/env python3
"""Measure authenticated loopback bridge overhead without uploading project data."""

from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import time
import urllib.request
from pathlib import Path


def percentile(values: list[float], percentile_value: int) -> float:
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, (len(ordered) * percentile_value + 99) // 100 - 1))
    return ordered[index]


def request(
    base_url: str,
    path: str,
    *,
    token: str | None = None,
    payload: dict[str, object] | None = None,
) -> tuple[object, float]:
    data = None if payload is None else json.dumps(payload).encode()
    headers = {"Accept": "application/json"}
    if data is not None:
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = f"Bearer {token}"
    operation = urllib.request.Request(
        f"{base_url}{path}",
        data=data,
        headers=headers,
        method="POST" if data is not None else "GET",
    )
    started = time.perf_counter_ns()
    with urllib.request.urlopen(operation, timeout=10) as response:
        body = json.load(response)
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    return body, elapsed_ms


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--query", default="atomic publication")
    parser.add_argument("--iterations", type=int, default=30)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--max-status-p95-ms", type=float, default=50.0)
    parser.add_argument("--max-search-p95-ms", type=float, default=100.0)
    arguments = parser.parse_args()
    if not 5 <= arguments.iterations <= 1000:
        parser.error("--iterations must be between 5 and 1000")

    process = subprocess.Popen(
        [
            str(arguments.binary),
            "dashboard",
            "serve",
            "--path",
            str(arguments.project),
            "--port",
            "0",
        ],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        assert process.stdout is not None
        ready_line = process.stdout.readline()
        if not ready_line:
            assert process.stderr is not None
            raise RuntimeError(process.stderr.read())
        ready = json.loads(ready_line)
        base_url = f"http://{ready['address']}"
        paired, _ = request(
            base_url,
            "/api/v1/pair",
            payload={"code": ready["pairing_code"]},
        )
        token = paired["token"]
        request(base_url, "/api/v1/status", token=token)
        request(
            base_url,
            "/api/v1/search",
            token=token,
            payload={"query": arguments.query, "limit": 20},
        )

        status_ms = []
        search_ms = []
        for _ in range(arguments.iterations):
            _, elapsed = request(base_url, "/api/v1/status", token=token)
            status_ms.append(elapsed)
            _, elapsed = request(
                base_url,
                "/api/v1/search",
                token=token,
                payload={"query": arguments.query, "limit": 20},
            )
            search_ms.append(elapsed)

        report = {
            "schema": 1,
            "project": str(arguments.project.resolve()),
            "iterations": arguments.iterations,
            "transport": "authenticated-loopback-http",
            "cloud_requests": 0,
            "status_p50_ms": round(statistics.median(status_ms), 3),
            "status_p95_ms": round(percentile(status_ms, 95), 3),
            "search_p50_ms": round(statistics.median(search_ms), 3),
            "search_p95_ms": round(percentile(search_ms, 95), 3),
            "ceilings": {
                "status_p95_ms": arguments.max_status_p95_ms,
                "search_p95_ms": arguments.max_search_p95_ms,
            },
        }
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report, indent=2))
        if report["status_p95_ms"] > arguments.max_status_p95_ms:
            raise RuntimeError("dashboard status p95 exceeded its ceiling")
        if report["search_p95_ms"] > arguments.max_search_p95_ms:
            raise RuntimeError("dashboard search p95 exceeded its ceiling")
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
