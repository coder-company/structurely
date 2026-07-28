#!/usr/bin/env python3
"""Benchmark Structurely and CodeGraph against one identical source corpus."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import statistics
import subprocess
import tempfile
import time
from pathlib import Path


def run(command: list[str], *, capture: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        check=True,
        text=True,
        stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
        stderr=subprocess.PIPE if capture else subprocess.DEVNULL,
    )


def fresh_copy(source: Path, destination: Path) -> None:
    shutil.rmtree(destination, ignore_errors=True)
    shutil.copytree(source, destination)


def percentile(samples: list[int], percentile_value: float) -> int:
    ordered = sorted(samples)
    index = min(len(ordered) - 1, int(len(ordered) * percentile_value))
    return ordered[index]


def timed(command: list[str]) -> tuple[float, subprocess.CompletedProcess[str]]:
    started = time.perf_counter_ns()
    result = run(command)
    return (time.perf_counter_ns() - started) / 1_000_000, result


def max_rss(command: list[str], output: Path) -> int:
    run(["/usr/bin/time", "-f", "%M", "-o", str(output), *command])
    return int(output.read_text(encoding="utf-8").strip())


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", required=True, type=Path)
    parser.add_argument("--codegraph", required=True, type=Path)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--query", default="CodeGraph")
    parser.add_argument("--trials", type=int, default=5)
    parser.add_argument("--queries", type=int, default=20)
    args = parser.parse_args()

    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="structurely-codegraph-") as temporary:
        root = Path(temporary)
        structurely_project = root / "structurely"
        codegraph_project = root / "codegraph"
        structurely_wall: list[float] = []
        codegraph_wall: list[float] = []
        structurely_engine: list[int] = []
        codegraph_engine: list[int] = []
        structurely_init: dict[str, int] = {}

        for _ in range(args.trials):
            fresh_copy(args.corpus, structurely_project)
            wall, result = timed(
                [str(args.structurely), "init", str(structurely_project)]
            )
            structurely_wall.append(wall)
            structurely_init = json.loads(result.stdout)
            structurely_engine.append(structurely_init["duration_ms"])

            fresh_copy(args.corpus, codegraph_project)
            wall, result = timed(
                ["node", str(args.codegraph), "init", str(codegraph_project)]
            )
            codegraph_wall.append(wall)
            duration = re.search(r"in ([0-9.]+)s", result.stdout)
            if duration is None:
                raise RuntimeError("CodeGraph did not report its engine duration")
            codegraph_engine.append(round(float(duration.group(1)) * 1000))

        structurely_queries: list[int] = []
        codegraph_queries: list[int] = []
        for _ in range(args.queries):
            started = time.perf_counter_ns()
            run(
                [
                    str(args.structurely),
                    "explore",
                    args.query,
                    "--path",
                    str(structurely_project),
                ],
                capture=False,
            )
            structurely_queries.append((time.perf_counter_ns() - started) // 1000)

            started = time.perf_counter_ns()
            run(
                [
                    "node",
                    str(args.codegraph),
                    "explore",
                    args.query,
                    "--path",
                    str(codegraph_project),
                ],
                capture=False,
            )
            codegraph_queries.append((time.perf_counter_ns() - started) // 1000)

        structurely_status = json.loads(
            run([str(args.structurely), "status", str(structurely_project)]).stdout
        )
        codegraph_status = json.loads(
            run(
                [
                    "node",
                    str(args.codegraph),
                    "status",
                    "--json",
                    str(codegraph_project),
                ]
            ).stdout
        )

        fresh_copy(args.corpus, structurely_project)
        structurely_rss = max_rss(
            [str(args.structurely), "init", str(structurely_project)],
            root / "structurely-rss",
        )
        fresh_copy(args.corpus, codegraph_project)
        codegraph_rss = max_rss(
            ["node", str(args.codegraph), "init", str(codegraph_project)],
            root / "codegraph-rss",
        )

    report = {
        "protocol": {
            "trials": args.trials,
            "query_processes": args.queries,
            "query": args.query,
            "corpus_files": sum(1 for path in args.corpus.rglob("*") if path.is_file()),
        },
        "structurely": {
            "fresh_index_wall_samples_ms": structurely_wall,
            "fresh_index_wall_p50_ms": statistics.median(structurely_wall),
            "reported_engine_samples_ms": structurely_engine,
            "reported_engine_p50_ms": statistics.median(structurely_engine),
            "query_samples_us": structurely_queries,
            "query_p50_us": statistics.median(structurely_queries),
            "query_p95_us": percentile(structurely_queries, 0.95),
            "database_bytes": structurely_status["storage"]["database_bytes"],
            "indexed_files": structurely_status["indexed_files"],
            "symbols": structurely_init["symbols_changed"],
            "relationships": structurely_init["relationships_resolved"],
            "max_rss_kb": structurely_rss,
        },
        "codegraph": {
            "fresh_index_wall_samples_ms": codegraph_wall,
            "fresh_index_wall_p50_ms": statistics.median(codegraph_wall),
            "reported_engine_samples_ms": codegraph_engine,
            "reported_engine_p50_ms": statistics.median(codegraph_engine),
            "query_samples_us": codegraph_queries,
            "query_p50_us": statistics.median(codegraph_queries),
            "query_p95_us": percentile(codegraph_queries, 0.95),
            "database_bytes": codegraph_status["dbSizeBytes"],
            "indexed_files": codegraph_status["fileCount"],
            "nodes": codegraph_status["nodeCount"],
            "edges": codegraph_status["edgeCount"],
            "max_rss_kb": codegraph_rss,
        },
    }
    (args.output / "results.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
