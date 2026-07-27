#!/usr/bin/env python3
"""Compare a Structurely benchmark report with a normalized CodeGraph baseline."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def load(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def positive_number(report: dict[str, Any], key: str) -> float:
    value = report.get(key)
    if not isinstance(value, (int, float)) or value < 0:
        raise ValueError(f"{key} must be a non-negative number")
    return float(value)


def ratio(baseline: float, candidate: float) -> float | None:
    return None if candidate == 0 else baseline / candidate


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", required=True, type=Path)
    parser.add_argument("--codegraph", required=True, type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    structurely = load(args.structurely)
    codegraph = load(args.codegraph)
    initial = structurely.get("initial_sync")
    if not isinstance(initial, dict):
        raise ValueError("Structurely report is missing initial_sync")

    candidate_index_ms = positive_number(initial, "duration_ms")
    candidate_query_us = positive_number(structurely, "query_p50_us")
    candidate_database = positive_number(structurely, "database_bytes")
    baseline_index_ms = positive_number(codegraph, "fresh_index_ms")
    baseline_query_us = positive_number(codegraph, "query_p50_us")
    baseline_database = positive_number(codegraph, "database_bytes")

    comparison = {
        "structurely": str(args.structurely),
        "codegraph": str(args.codegraph),
        "fresh_index_speedup": ratio(baseline_index_ms, candidate_index_ms),
        "query_p50_speedup": ratio(baseline_query_us, candidate_query_us),
        "database_size_ratio": ratio(baseline_database, candidate_database),
        "raw": {
            "structurely": {
                "fresh_index_ms": candidate_index_ms,
                "query_p50_us": candidate_query_us,
                "database_bytes": candidate_database,
            },
            "codegraph": {
                "fresh_index_ms": baseline_index_ms,
                "query_p50_us": baseline_query_us,
                "database_bytes": baseline_database,
            },
        },
    }
    rendered = json.dumps(comparison, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

