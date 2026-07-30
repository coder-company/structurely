#!/usr/bin/env python3
"""Gate Structurely's claimed workflow and retrieval advantages over Perseus."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


RELEVANCE_QUERIES = (
    ("MCP tool dispatch", "src/mcp.rs"),
    ("Windows daemon process detachment", "src/daemon.rs"),
    ("project config custom extensions", "src/project_config.rs"),
    ("atomic file publication", "src/atomic_file.rs"),
    ("benchmark comparator regression gate", "scripts/compare_benchmarks.py"),
)

WORKFLOW_CAPABILITIES = {
    "research": ("research",),
    "session_history": ("session",),
    "recaps": ("recap",),
    "impact_analysis": ("impact",),
    "path_tracing": ("trace",),
    "durable_memory": ("memory",),
    "team_workspaces": ("workspace",),
}


def run(command: list[str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, check=check, text=True, capture_output=True)


def ranked_files(report: dict[str, Any]) -> list[str]:
    """Return research files in retrieval order, not alphabetic presentation order."""
    ranked: list[str] = []
    for finding in report.get("symbol_findings", []):
        path = finding.get("symbol", {}).get("file")
        if isinstance(path, str) and path not in ranked:
            ranked.append(path)
    for finding in report.get("content_findings", []):
        path = finding.get("path")
        if isinstance(path, str) and path not in ranked:
            ranked.append(path)
    return ranked


def workflow_coverage(help_text: str) -> dict[str, bool]:
    normalized = help_text.casefold()
    return {
        capability: any(token in normalized for token in tokens)
        for capability, tokens in WORKFLOW_CAPABILITIES.items()
    }


def rank(path: str, files: list[str]) -> int | None:
    try:
        return files.index(path) + 1
    except ValueError:
        return None


def relevance_gates(
    rank_one: int, top_ten: int, baseline_comparison: dict[str, Any]
) -> dict[str, bool]:
    return {
        "rank_one_better": rank_one
        > baseline_comparison["rank_one"]["perseus"],
        "top_ten_better": top_ten
        > baseline_comparison["top_ten_expected_file_recall"]["perseus"],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", required=True, type=Path)
    parser.add_argument("--project", default=Path("."), type=Path)
    parser.add_argument(
        "--baseline",
        default=Path("benchmarks/perseus-2026-07-29/results.json"),
        type=Path,
    )
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    binary = args.structurely.resolve()
    project = args.project.resolve()
    baseline = json.loads(args.baseline.read_text(encoding="utf-8"))
    perseus = baseline["perseus"]
    baseline_relevance = baseline["comparison"]

    sync = json.loads(run([str(binary), "sync", str(project)]).stdout)
    help_text = run([str(binary), "--help"]).stdout
    workflows = workflow_coverage(help_text)

    relevance: list[dict[str, Any]] = []
    for query, expected in RELEVANCE_QUERIES:
        research = json.loads(
            run(
                [
                    str(binary),
                    "research",
                    query,
                    "--path",
                    str(project),
                    "--max-files",
                    "10",
                ]
            ).stdout
        )
        files = ranked_files(research)
        relevance.append(
            {
                "query": query,
                "expected": expected,
                "rank": rank(expected, files),
                "rankedFiles": files,
            }
        )

    rank_one = sum(item["rank"] == 1 for item in relevance)
    top_ten = sum(
        item["rank"] is not None and item["rank"] <= 10 for item in relevance
    )
    atomic_rank = next(
        item["rank"]
        for item in relevance
        if item["query"] == "atomic file publication"
    )
    gates = {
        "all_workflows": all(workflows.values()),
        "repository_file_coverage": sync["content_files_indexed"]
        >= perseus["indexed_files"],
        "chunk_retrieval_available": sync["content_chunks"] > 0,
        "atomic_publication_rank_one": atomic_rank == 1,
        **relevance_gates(rank_one, top_ten, baseline_relevance),
    }
    report = {
        "passed": all(gates.values()),
        "project": str(project),
        "baseline": {
            "perseusFiles": perseus["indexed_files"],
            "perseusChunks": perseus["chunks"],
            "perseusRankOne": baseline_relevance["rank_one"]["perseus"],
            "perseusTopTen": baseline_relevance[
                "top_ten_expected_file_recall"
            ]["perseus"],
        },
        "structurely": {
            "sourceFiles": sync["files_scanned"] - sync["files_skipped"],
            "repositoryContentFiles": sync["content_files_indexed"],
            "contentChunks": sync["content_chunks"],
            "rankOne": rank_one,
            "topTen": top_ten,
            "workflowCapabilities": workflows,
        },
        "gates": gates,
        "relevance": relevance,
    }
    rendered = json.dumps(report, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    print(rendered, end="")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
