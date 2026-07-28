#!/usr/bin/env python3
"""Run pinned semantic assertions on representative public repositories."""

from __future__ import annotations

import argparse
import json
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any


def run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, check=True, text=True, capture_output=True)


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
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

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
            started = time.perf_counter_ns()
            initialized = json.loads(run([str(binary), "init", str(project)]).stdout)
            wall_ms = (time.perf_counter_ns() - started) / 1_000_000
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
                output = run(assertion_command(binary, project, assertion)).stdout
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
                    }
                )
                if missing_text or not matching_record:
                    raise RuntimeError(
                        f"{name}: {assertion['command']} {assertion['query']!r} "
                        f"missed {missing_text or ['one correlated output record']}"
                    )

            status = json.loads(run([str(binary), "status", str(project)]).stdout)
            reports.append(
                {
                    "name": name,
                    "url": repository["url"],
                    "commit": repository["commit"],
                    "freshIndexWallMs": round(wall_ms, 3),
                    "indexedFiles": indexed_files,
                    "symbols": initialized["symbols_changed"],
                    "relationships": initialized["relationships_resolved"],
                    "databaseBytes": status["storage"]["database_bytes"],
                    "assertions": checks,
                }
            )

    report = {"passed": True, "repositories": reports}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
