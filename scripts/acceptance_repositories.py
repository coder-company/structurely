#!/usr/bin/env python3
"""Run pinned semantic assertions on representative public repositories."""

from __future__ import annotations

import argparse
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


def run(
    command: list[str], *, peak_rss_file: Path | None = None
) -> subprocess.CompletedProcess[str]:
    if (
        peak_rss_file is not None
        and sys.platform.startswith("linux")
        and Path("/usr/bin/time").is_file()
    ):
        command = [
            "/usr/bin/time",
            "-f",
            "%M",
            "-o",
            str(peak_rss_file),
            *command,
        ]
    return subprocess.run(command, check=True, text=True, capture_output=True)


def percentile(samples: list[float], percentage: int) -> float:
    if not samples:
        raise ValueError("cannot calculate a percentile without samples")
    ordered = sorted(samples)
    index = max(0, (len(ordered) * percentage + 99) // 100 - 1)
    return round(ordered[index], 3)


def timed_run(command: list[str]) -> tuple[subprocess.CompletedProcess[str], float]:
    started = time.perf_counter_ns()
    completed = run(command)
    return completed, (time.perf_counter_ns() - started) / 1_000_000


def directory_bytes(root: Path) -> int:
    return sum(
        entry.stat(follow_symlinks=False).st_size
        for entry in root.rglob("*")
        if entry.is_file() and not entry.is_symlink()
    )


def enforce_limits(name: str, metrics: dict[str, float], limits: dict[str, Any]) -> None:
    for metric, maximum in limits.items():
        if metric not in metrics:
            raise ValueError(f"{name}: unknown performance limit {metric!r}")
        if metrics[metric] > maximum:
            raise RuntimeError(
                f"{name}: {metric} was {metrics[metric]}, expected at most {maximum}"
            )


def repositories(values: list[str]) -> dict[str, Path]:
    resolved: dict[str, Path] = {}
    for value in values:
        name, separator, path = value.partition("=")
        if not separator or not name or not path:
            raise ValueError(f"expected NAME=PATH, got {value!r}")
        resolved[name] = Path(path).resolve()
    return resolved


def checked_out_copy(source: Path, destination: Path, commit: str) -> None:
    run(["git", "clone", "--quiet", "--shared", str(source), str(destination)])
    run(["git", "-C", str(destination), "checkout", "--quiet", "--detach", commit])
    actual = run(["git", "-C", str(destination), "rev-parse", "HEAD"]).stdout.strip()
    if actual != commit:
        raise RuntimeError(f"expected {commit}, checked out {actual}")


def assertion_command(
    binary: Path, project: Path, assertion: dict[str, Any]
) -> list[str]:
    command = [
        str(binary),
        assertion["command"],
        assertion["query"],
        "--path",
        str(project),
    ]
    if file := assertion.get("file"):
        command.extend(["--file", file])
    return command


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", required=True, type=Path)
    parser.add_argument(
        "--manifest", type=Path, default=Path("fixtures/real-repositories.json")
    )
    parser.add_argument(
        "--repository",
        action="append",
        default=[],
        metavar="NAME=PATH",
        help="local clone or mirror; repeat for every selected repository",
    )
    parser.add_argument("--only", action="append", default=[])
    parser.add_argument(
        "--query-samples",
        type=int,
        default=5,
        help="subprocess latency samples per semantic assertion (default: 5)",
    )
    parser.add_argument(
        "--enforce-performance-limits",
        action="store_true",
        help="enforce optional performanceLimits from the pinned manifest",
    )
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if args.query_samples < 1:
        parser.error("--query-samples must be at least 1")

    binary = args.structurely.resolve()
    sources = repositories(args.repository)
    manifest = json.loads(args.manifest.read_text())
    selected = [
        repository
        for repository in manifest["repositories"]
        if not args.only or repository["name"] in args.only
    ]
    missing = [repository["name"] for repository in selected if repository["name"] not in sources]
    if missing:
        raise SystemExit(f"missing --repository for: {', '.join(missing)}")

    reports: list[dict[str, Any]] = []
    with tempfile.TemporaryDirectory(prefix="structurely-acceptance-") as temporary:
        root = Path(temporary)
        for repository in selected:
            name = repository["name"]
            project = root / name
            checked_out_copy(sources[name], project, repository["commit"])
            if config := repository.get("structurelyConfig"):
                (project / "structurely.json").write_text(
                    json.dumps(config, indent=2) + "\n"
                )
            rss_file = root / f"{name}-peak-rss-kib"
            started = time.perf_counter_ns()
            initialized = json.loads(
                run(
                    [str(binary), "init", str(project)],
                    peak_rss_file=rss_file,
                ).stdout
            )
            wall_ms = (time.perf_counter_ns() - started) / 1_000_000
            peak_rss_kib = (
                int(rss_file.read_text().strip()) if rss_file.exists() else None
            )
            indexed_files = initialized["files_scanned"] - initialized["files_skipped"]
            if indexed_files < repository["minimumIndexedFiles"]:
                raise RuntimeError(
                    f"{name}: indexed {indexed_files}, expected at least "
                    f"{repository['minimumIndexedFiles']}"
                )
            if indexed_files > repository.get("maximumIndexedFiles", indexed_files):
                raise RuntimeError(
                    f"{name}: indexed {indexed_files}, expected at most "
                    f"{repository['maximumIndexedFiles']}"
                )

            checks = []
            for assertion in repository["assertions"]:
                command = assertion_command(binary, project, assertion)
                completed, first_wall_ms = timed_run(command)
                output = completed.stdout
                query_wall_ms = [first_wall_ms]
                for _ in range(args.query_samples - 1):
                    _, sample_wall_ms = timed_run(command)
                    query_wall_ms.append(sample_wall_ms)
                missing_text = [
                    expected
                    for expected in assertion["contains"]
                    if expected not in output
                ]
                record_text = assertion.get("recordContains", [])
                parsed = json.loads(output)
                records = parsed if isinstance(parsed, list) else [parsed]
                matching_record = not record_text or any(
                    all(expected in json.dumps(record) for expected in record_text)
                    for record in records
                )
                checks.append(
                    {
                        "command": assertion["command"],
                        "query": assertion["query"],
                        "passed": not missing_text,
                        "requiredText": assertion["contains"],
                        "recordContains": record_text,
                        "matchingRecord": matching_record,
                        "latencySamplesMs": [
                            round(sample, 3) for sample in query_wall_ms
                        ],
                        "latencyP50Ms": percentile(query_wall_ms, 50),
                        "latencyP95Ms": percentile(query_wall_ms, 95),
                    }
                )
                if missing_text or not matching_record:
                    raise RuntimeError(
                        f"{name}: {assertion['command']} {assertion['query']!r} "
                        f"missed {missing_text or ['one correlated output record']}"
                    )

            incremental: dict[str, Any] | None = None
            if incremental_file := repository.get("incrementalFile"):
                changed = project / incremental_file
                if not changed.is_file():
                    raise RuntimeError(
                        f"{name}: incremental file does not exist: {incremental_file}"
                    )
                with changed.open("a", encoding="utf-8") as handle:
                    handle.write(os.linesep)
                completed, incremental_wall_ms = timed_run(
                    [str(binary), "sync", str(project)]
                )
                sync = json.loads(completed.stdout)
                if sync["files_changed"] != 1 or sync["files_deleted"] != 0:
                    raise RuntimeError(
                        f"{name}: incremental sync changed {sync['files_changed']} "
                        f"files and deleted {sync['files_deleted']}, expected 1 and 0"
                    )
                incremental = {
                    "file": incremental_file,
                    "wallMs": round(incremental_wall_ms, 3),
                    "engineDurationMs": sync["duration_ms"],
                    "symbolsChanged": sync["symbols_changed"],
                    "relationshipsResolved": sync["relationships_resolved"],
                }

            status = json.loads(run([str(binary), "status", str(project)]).stdout)
            query_p50_values = [check["latencyP50Ms"] for check in checks]
            query_p95_values = [check["latencyP95Ms"] for check in checks]
            metrics: dict[str, float] = {
                "freshIndexWallMs": round(wall_ms, 3),
                "queryP50Ms": round(statistics.median(query_p50_values), 3),
                "queryP95Ms": max(query_p95_values),
                "storageBytes": directory_bytes(project / ".structurely"),
            }
            if peak_rss_kib is not None:
                metrics["freshIndexPeakRssKiB"] = peak_rss_kib
            if incremental is not None:
                metrics["incrementalSyncWallMs"] = incremental["wallMs"]
            if args.enforce_performance_limits:
                enforce_limits(name, metrics, repository.get("performanceLimits", {}))
            reports.append(
                {
                    "name": name,
                    "url": repository["url"],
                    "commit": repository["commit"],
                    "freshIndexWallMs": round(wall_ms, 3),
                    "indexedFiles": indexed_files,
                    "symbols": initialized["symbols_changed"],
                    "relationships": initialized["relationships_resolved"],
                    "contentFiles": initialized["content_files_indexed"],
                    "contentChunks": initialized["content_chunks"],
                    "databaseBytes": status["storage"]["database_bytes"],
                    "storageBytes": metrics["storageBytes"],
                    "freshIndexPeakRssKiB": peak_rss_kib,
                    "queryP50Ms": metrics["queryP50Ms"],
                    "queryP95Ms": metrics["queryP95Ms"],
                    "incrementalSync": incremental,
                    "assertions": checks,
                }
            )

    report = {"passed": True, "repositories": reports}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
