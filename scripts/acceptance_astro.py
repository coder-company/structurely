#!/usr/bin/env python3
"""Gate Astro semantics on the pinned CodeGraph documentation site."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

PINNED_COMMIT = "572d22bfbe82602080e457bec655f72e3314f9ef"
CORPUS_FILES = (
    "site/src/components/GraphDiagram.astro",
    "site/src/components/SiteTitle.astro",
    "site/src/components/SocialIcons.astro",
    "site/src/pages/index.astro",
    "site/src/lib/github.ts",
)


def run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, check=True, text=True, capture_output=True)


def verify_repository(repository: Path) -> None:
    commit = run(["git", "-C", str(repository), "rev-parse", "HEAD"]).stdout.strip()
    if commit != PINNED_COMMIT:
        raise RuntimeError(f"expected CodeGraph {PINNED_COMMIT}, got {commit}")
    package = json.loads((repository / "package.json").read_text())
    if package.get("version") != "1.5.0":
        raise RuntimeError(f"expected CodeGraph 1.5.0, got {package.get('version')}")


def copy_corpus(repository: Path, destination: Path) -> None:
    for relative in CORPUS_FILES:
        source = repository / relative
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)


def relationship_tuples(snapshot: dict[str, Any]) -> list[dict[str, Any]]:
    symbols = {symbol["id"]: symbol for symbol in snapshot["symbols"]}
    return [
        {
            "source": symbols[relationship["source_id"]],
            "target": symbols[relationship["target_id"]],
            "kind": relationship["kind"],
            "evidence": relationship["evidence"],
        }
        for relationship in snapshot["relationships"]
    ]


def evaluate(snapshot: dict[str, Any], corpus: Path) -> dict[str, Any]:
    symbols = snapshot["symbols"]
    relationships = relationship_tuples(snapshot)
    components = [symbol for symbol in symbols if symbol["kind"] == "component"]
    routes = [symbol for symbol in symbols if symbol["kind"] == "route"]
    expected_components = {"GraphDiagram", "SiteTitle", "SocialIcons", "index"}

    def edges(
        source_name: str,
        target_name: str,
        provenance: str,
        line: int,
    ) -> list[dict[str, Any]]:
        return [
            relationship
            for relationship in relationships
            if relationship["source"]["name"] == source_name
            and relationship["target"]["name"] == target_name
            and relationship["evidence"]["provenance"] == provenance
            and relationship["evidence"]["line"] == line
        ]

    checks = {
        "five_files": len(snapshot["files"]) == 5,
        "four_components": {component["name"] for component in components}
        == expected_components,
        "root_route": len(routes) == 1
        and routes[0]["name"] == "/"
        and routes[0]["file"] == "site/src/pages/index.astro",
        "page_renders_graph": len(
            edges("index", "GraphDiagram", "framework/astro-template", 70)
        )
        == 1,
        "index_calls_stars": len(
            [
                edge
                for edge in edges(
                    "site/src/pages/index.astro",
                    "getStarsLabel",
                    "tree-sitter/name-resolution",
                    13,
                )
                if edge["source"]["kind"] == "file"
            ]
        )
        == 1,
        "social_calls_stars": len(
            [
                edge
                for edge in edges(
                    "site/src/components/SocialIcons.astro",
                    "getStarsLabel",
                    "tree-sitter/name-resolution",
                    9,
                )
                if edge["source"]["kind"] == "file"
            ]
        )
        == 1,
        "utility_flow": len(
            edges("getStarsLabel", "fetchStars", "tree-sitter/name-resolution", 40)
        )
        == 1
        and len(edges("fetchStars", "format", "tree-sitter/name-resolution", 31))
        == 1,
        "no_template_self_edges": not any(
            edge["source"]["id"] == edge["target"]["id"]
            and edge["evidence"]["provenance"].startswith("framework/astro-template")
            for edge in relationships
        ),
        "no_external_default_symbol": not any(
            symbol["name"] == "Default" for symbol in symbols
        ),
        "component_full_byte_extents": all(
            component["start_byte"] == 0
            and component["end_byte"] == (corpus / component["file"]).stat().st_size
            for component in components
        ),
    }
    if not all(checks.values()):
        failed = [name for name, passed in checks.items() if not passed]
        raise RuntimeError(f"Astro acceptance failed: {', '.join(failed)}")
    return {
        "passed": True,
        "checks": checks,
        "files": len(snapshot["files"]),
        "symbols": len(symbols),
        "relationships": len(snapshot["relationships"]),
        "components": len(components),
        "routes": len(routes),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", required=True, type=Path)
    parser.add_argument("--codegraph-repository", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    binary = args.structurely.resolve()
    repository = args.codegraph_repository.resolve()
    verify_repository(repository)
    with tempfile.TemporaryDirectory(prefix="structurely-astro-") as temporary:
        corpus = Path(temporary)
        copy_corpus(repository, corpus)
        started = time.perf_counter_ns()
        initialized = json.loads(run([str(binary), "init", str(corpus)]).stdout)
        wall_ms = (time.perf_counter_ns() - started) / 1_000_000
        snapshot = json.loads(
            run([str(binary), "snapshot", "--path", str(corpus)]).stdout
        )
        result = evaluate(snapshot, corpus)

    result.update(
        {
            "structurelyBinarySha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
            "codegraphVersion": "1.5.0",
            "codegraphCommit": PINNED_COMMIT,
            "freshIndexWallMs": round(wall_ms, 3),
            "engineDurationMs": initialized["duration_ms"],
        }
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
