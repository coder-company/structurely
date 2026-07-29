#!/usr/bin/env python3
"""Gate exact FastAPI dependency graphs on pinned LightRAG and Graphiti sources."""

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

MANIFEST = Path(__file__).resolve().parents[1] / "fixtures/real-repositories.json"
PROVENANCE = "framework/fastapi-dependency"

LIGHTRAG_SERVER = "lightrag/api/lightrag_server.py"
LIGHTRAG_UTILS = "lightrag/api/utils_api.py"
LIGHTRAG_DOCUMENTS = "lightrag/api/routers/document_routes.py"
LIGHTRAG_GRAPH = "lightrag/api/routers/graph_routes.py"
LIGHTRAG_OLLAMA = "lightrag/api/routers/ollama_api.py"
LIGHTRAG_QUERY = "lightrag/api/routers/query_routes.py"
LIGHTRAG_FILES = (
    LIGHTRAG_SERVER,
    LIGHTRAG_UTILS,
    LIGHTRAG_DOCUMENTS,
    LIGHTRAG_GRAPH,
    LIGHTRAG_OLLAMA,
    LIGHTRAG_QUERY,
)

GRAPHITI_CONFIG = "server/graph_service/config.py"
GRAPHITI_ZEP = "server/graph_service/zep_graphiti.py"
GRAPHITI_MAIN = "server/graph_service/main.py"
GRAPHITI_INGEST = "server/graph_service/routers/ingest.py"
GRAPHITI_RETRIEVE = "server/graph_service/routers/retrieve.py"
GRAPHITI_FILES = (
    GRAPHITI_CONFIG,
    GRAPHITI_ZEP,
    GRAPHITI_MAIN,
    GRAPHITI_INGEST,
    GRAPHITI_RETRIEVE,
)

# Every tuple is (source function, target name, target qualified name, source
# file, target file). This is intentionally an exact production inventory
# rather than a count assertion. In particular, LightRAG's assignment aliases
# must resolve through their factories to the callable symbols actually
# returned by those factories.
LIGHTRAG_DEPENDENCIES = (
    *(
        (
            name,
            "combined_dependency",
            "get_combined_auth_dependency.combined_dependency",
            LIGHTRAG_DOCUMENTS,
            LIGHTRAG_UTILS,
        )
        for name in (
            "scan_for_new_documents",
            "upload_to_input_dir",
            "insert_text",
            "insert_texts",
            "clear_documents",
            "get_pipeline_status",
            "documents",
            "delete_document",
            "clear_cache",
            "get_track_status",
            "get_documents_paginated",
            "get_document_status_counts",
            "reprocess_failed_documents",
            "cancel_pipeline",
        )
    ),
    *(
        (
            name,
            "combined_dependency",
            "get_combined_auth_dependency.combined_dependency",
            LIGHTRAG_GRAPH,
            LIGHTRAG_UTILS,
        )
        for name in (
            "get_graph_labels",
            "get_popular_labels",
            "search_labels",
            "get_knowledge_graph",
            "check_entity_exists",
            "update_entity",
            "update_relation",
            "create_entity",
            "create_relation",
            "merge_entities",
            "delete_entity",
            "delete_relation",
        )
    ),
    *(
        (
            name,
            "combined_dependency",
            "get_combined_auth_dependency.combined_dependency",
            LIGHTRAG_OLLAMA,
            LIGHTRAG_UTILS,
        )
        for name in (
            "get_version",
            "get_tags",
            "get_running_models",
            "generate",
            "chat",
        )
    ),
    *(
        (
            name,
            "combined_dependency",
            "get_combined_auth_dependency.combined_dependency",
            LIGHTRAG_QUERY,
            LIGHTRAG_UTILS,
        )
        for name in (
            "query_text",
            "query_text_stream",
            "query_data",
        )
    ),
    (
        "get_status",
        "combined_dependency",
        "get_combined_auth_dependency.combined_dependency",
        LIGHTRAG_SERVER,
        LIGHTRAG_UTILS,
    ),
    (
        "get_status",
        "auth_status_dependency",
        "get_auth_status_dependency.auth_status_dependency",
        LIGHTRAG_SERVER,
        LIGHTRAG_UTILS,
    ),
)

GRAPHITI_ENDPOINTS = (
    ("add_messages", GRAPHITI_INGEST),
    ("add_entity_node", GRAPHITI_INGEST),
    ("delete_entity_edge", GRAPHITI_INGEST),
    ("delete_group", GRAPHITI_INGEST),
    ("delete_episode", GRAPHITI_INGEST),
    ("clear", GRAPHITI_INGEST),
    ("search", GRAPHITI_RETRIEVE),
    ("get_entity_edge", GRAPHITI_RETRIEVE),
    ("get_episodes", GRAPHITI_RETRIEVE),
    ("get_memory", GRAPHITI_RETRIEVE),
)
GRAPHITI_DEPENDENCIES = (
    *(
        (name, "get_graphiti", "get_graphiti", file, GRAPHITI_ZEP)
        for name, file in GRAPHITI_ENDPOINTS
    ),
    (
        "_create_graphiti_client",
        "get_settings",
        "get_settings",
        GRAPHITI_ZEP,
        GRAPHITI_CONFIG,
    ),
    (
        "get_graphiti",
        "get_settings",
        "get_settings",
        GRAPHITI_ZEP,
        GRAPHITI_CONFIG,
    ),
    (
        "initialize_graphiti",
        "get_settings",
        "get_settings",
        GRAPHITI_ZEP,
        GRAPHITI_CONFIG,
    ),
)


def run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, check=True, text=True, capture_output=True)


def pinned_commits() -> dict[str, str]:
    manifest = json.loads(MANIFEST.read_text())
    return {
        repository["name"]: repository["commit"]
        for repository in manifest["repositories"]
        if repository["name"] in {"lightrag", "graphiti"}
    }


def verify_repository(
    name: str, repository: Path, commit: str, files: tuple[str, ...]
) -> None:
    actual = run(["git", "-C", str(repository), "rev-parse", "HEAD"]).stdout.strip()
    if actual != commit:
        raise RuntimeError(f"expected pinned {name} commit {commit}, got {actual}")
    changed = subprocess.run(
        ["git", "-C", str(repository), "diff", "--quiet", "HEAD", "--", *files],
        check=False,
    )
    if changed.returncode not in {0, 1}:
        raise RuntimeError(f"could not verify selected {name} source files")
    if changed.returncode == 1:
        raise RuntimeError(f"{name} has modified pinned corpus files")
    missing = [relative for relative in files if not (repository / relative).is_file()]
    if missing:
        raise RuntimeError(f"{name} is missing pinned corpus files: {missing}")


def copy_files(repository: Path, destination: Path, files: tuple[str, ...]) -> None:
    for relative in files:
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(repository / relative, target)


def index(binary: Path, corpus: Path) -> tuple[dict[str, Any], dict[str, Any], float]:
    started = time.perf_counter_ns()
    initialized = json.loads(run([str(binary), "init", str(corpus)]).stdout)
    wall_ms = (time.perf_counter_ns() - started) / 1_000_000
    snapshot = json.loads(run([str(binary), "snapshot", "--path", str(corpus)]).stdout)
    return initialized, snapshot, wall_ms


def dependency_edges(
    snapshot: dict[str, Any],
) -> list[tuple[dict[str, Any], dict[str, Any], dict[str, Any]]]:
    symbols = {symbol["id"]: symbol for symbol in snapshot["symbols"]}
    return [
        (symbols[edge["source_id"]], symbols[edge["target_id"]], edge)
        for edge in snapshot["relationships"]
        if edge["evidence"]["provenance"] == PROVENANCE
    ]


def evaluate_exact_edges(
    snapshot: dict[str, Any],
    expected: tuple[tuple[str, str, str, str, str], ...],
) -> tuple[dict[str, Any], list[tuple[dict[str, Any], dict[str, Any], dict[str, Any]]]]:
    expected_set = set(expected)
    # The corpus contains only the selected production modules, so inspect
    # every framework dependency edge. Filtering to expected source files
    # would let leakage from a helper, import, or application module escape.
    edges = dependency_edges(snapshot)
    actual = [
        (
            source["name"],
            target["name"],
            target["qualified_name"],
            source["file"],
            target["file"],
        )
        for source, target, _ in edges
    ]
    actual_set = set(actual)
    malformed = [
        {
            "source": source["qualified_name"],
            "target": target["qualified_name"],
            "kind": edge["kind"],
            "evidenceFile": edge["evidence"]["file"],
        }
        for source, target, edge in edges
        if edge["kind"] != "calls"
        or edge["evidence"]["file"] != source["file"]
        or source["kind"] not in {"function", "method"}
        or target["kind"] not in {"function", "method"}
    ]
    missing = sorted(expected_set - actual_set)
    unexpected = sorted(actual_set - expected_set)
    duplicates = len(actual) != len(actual_set)
    evidence_sites = [
        (source["id"], edge["evidence"].get("site"))
        for source, _, edge in edges
    ]
    invalid_sites = [
        site for _, site in evidence_sites if not isinstance(site, int) or site <= 0
    ]
    duplicate_sites = len(evidence_sites) != len(set(evidence_sites))
    checks = {
        "exactDirectDependencyEdges": len(edges) == len(expected),
        "exactSourceTargetSites": not missing and not unexpected,
        "callsWithOwnedEvidence": not malformed,
        "noDuplicateEdges": not duplicates,
        "stableDistinctEvidenceSites": not invalid_sites and not duplicate_sites,
    }
    if not all(checks.values()):
        raise RuntimeError(
            "FastAPI dependency acceptance failed: "
            + json.dumps(
                {
                    "failed": [name for name, passed in checks.items() if not passed],
                    "missing": missing,
                    "unexpected": unexpected,
                    "malformed": malformed,
                },
                sort_keys=True,
            )
        )
    return {"checks": checks, "directEdges": len(edges)}, edges


def evaluate_lightrag(snapshot: dict[str, Any]) -> dict[str, Any]:
    result, edges = evaluate_exact_edges(snapshot, LIGHTRAG_DEPENDENCIES)
    login_edges = [edge for source, _, edge in edges if source["name"] == "login"]
    if login_edges:
        raise RuntimeError("bare login Depends() unexpectedly emitted a callable edge")
    result["checks"]["bareLoginDependsEmitsNoCallableEdge"] = True
    result["loginDependencyEdges"] = 0
    return result


def evaluate_graphiti(snapshot: dict[str, Any]) -> dict[str, Any]:
    result, edges = evaluate_exact_edges(snapshot, GRAPHITI_DEPENDENCIES)
    adjacency: set[tuple[str, str, str]] = {
        (source["name"], target["name"], source["file"]) for source, target, _ in edges
    }
    paths = [
        {
            "endpoint": endpoint,
            "file": file,
            "path": [endpoint, "get_graphiti", "get_settings"],
        }
        for endpoint, file in GRAPHITI_ENDPOINTS
        if (endpoint, "get_graphiti", file) in adjacency
        and ("get_graphiti", "get_settings", GRAPHITI_ZEP) in adjacency
    ]
    if len(paths) != 10:
        raise RuntimeError(
            f"expected 10 endpoint->get_graphiti->get_settings paths, got {len(paths)}"
        )
    result["checks"]["exactTenEndpointToSettingsPaths"] = True
    result["transitivePaths"] = paths
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", required=True, type=Path)
    parser.add_argument("--lightrag-repository", required=True, type=Path)
    parser.add_argument("--graphiti-repository", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    binary = args.structurely.resolve()
    if not binary.is_file():
        raise SystemExit(f"Structurely binary does not exist: {binary}")
    repositories = {
        "lightrag": args.lightrag_repository.resolve(),
        "graphiti": args.graphiti_repository.resolve(),
    }
    commits = pinned_commits()
    selected_files = {"lightrag": LIGHTRAG_FILES, "graphiti": GRAPHITI_FILES}
    for name, repository in repositories.items():
        verify_repository(name, repository, commits[name], selected_files[name])

    results: dict[str, Any] = {}
    with tempfile.TemporaryDirectory(
        prefix="structurely-fastapi-dependencies-"
    ) as temporary:
        base = Path(temporary)
        for name, evaluator in (
            ("lightrag", evaluate_lightrag),
            ("graphiti", evaluate_graphiti),
        ):
            corpus = base / name
            copy_files(repositories[name], corpus, selected_files[name])
            initialized, snapshot, wall_ms = index(binary, corpus)
            result = evaluator(snapshot)
            result.update(
                {
                    "commit": commits[name],
                    "files": len(snapshot["files"]),
                    "symbols": len(snapshot["symbols"]),
                    "relationships": len(snapshot["relationships"]),
                    "freshIndexWallMs": round(wall_ms, 3),
                    "engineDurationMs": initialized["duration_ms"],
                }
            )
            results[name] = result

    if results["lightrag"]["directEdges"] != 36:
        raise RuntimeError(
            "LightRAG dependency inventory must contain exactly 36 edges"
        )
    if results["graphiti"]["directEdges"] != 13:
        raise RuntimeError(
            "Graphiti dependency inventory must contain exactly 13 edges"
        )
    output = {
        "passed": True,
        "checks": {
            "pinnedRepositories": True,
            "exactLightRag36DirectSites": True,
            "exactGraphiti13DirectEdges": True,
            "exactGraphiti10TransitivePaths": True,
            "noBareDependsCallableEdge": True,
            "noCrossLeakage": True,
        },
        "structurelyBinarySha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "repositories": results,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps(output, indent=2))


if __name__ == "__main__":
    main()
