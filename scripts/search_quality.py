#!/usr/bin/env python3
"""Enforce a versioned, multi-domain repository retrieval contract."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


def ranked_files(report: dict[str, Any]) -> list[str]:
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


def evaluate(query: dict[str, Any], files: list[str]) -> dict[str, Any]:
    expected = query["expected"]
    actual_rank = files.index(expected) + 1 if expected in files else None
    maximum = query["maximumRank"]
    return {
        **query,
        "rank": actual_rank,
        "passed": actual_rank is not None and actual_rank <= maximum,
        "rankedFiles": files,
    }


def validate_manifest(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    if manifest.get("version") != 1:
        raise ValueError("search-quality manifest version must be 1")
    queries = manifest.get("queries")
    if not isinstance(queries, list) or not queries:
        raise ValueError("search-quality manifest requires queries")
    seen: set[str] = set()
    for query in queries:
        for field in ("query", "expected", "maximumRank", "category"):
            if field not in query:
                raise ValueError(f"search-quality query is missing {field}")
        if query["query"] in seen:
            raise ValueError(f"duplicate search-quality query: {query['query']}")
        if not isinstance(query["maximumRank"], int) or query["maximumRank"] < 1:
            raise ValueError("maximumRank must be a positive integer")
        seen.add(query["query"])
    return queries


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", required=True, type=Path)
    parser.add_argument("--project", default=Path("."), type=Path)
    parser.add_argument(
        "--manifest", default=Path("fixtures/search-quality.json"), type=Path
    )
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    binary = args.structurely.resolve()
    project = args.project.resolve()
    manifest_path = args.manifest.resolve()
    queries = validate_manifest(json.loads(manifest_path.read_text(encoding="utf-8")))
    try:
        manifest_project_path = str(manifest_path.relative_to(project))
    except ValueError:
        manifest_project_path = None
    subprocess.run(
        [str(binary), "sync", str(project)], check=True, capture_output=True, text=True
    )
    results = []
    for query in queries:
        completed = subprocess.run(
            [
                str(binary),
                "research",
                query["query"],
                "--path",
                str(project),
                "--max-files",
                "10",
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        results.append(evaluate(query, ranked_files(json.loads(completed.stdout))))

    query_source_leakage = bool(
        manifest_project_path
        and any(
            manifest_project_path in result["rankedFiles"] for result in results
        )
    )
    report = {
        "passed": all(result["passed"] for result in results)
        and not query_source_leakage,
        "manifestVersion": 1,
        "project": str(project),
        "queries": len(results),
        "rankOne": sum(result["rank"] == 1 for result in results),
        "querySourceLeakage": query_source_leakage,
        "results": results,
    }
    rendered = json.dumps(report, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    print(rendered, end="")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
