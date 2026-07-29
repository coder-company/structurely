#!/usr/bin/env python3
"""Gate FastAPI router composition on pinned LightRAG and Graphiti sources."""

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
LIGHTRAG_FILES = (
    "lightrag/api/lightrag_server.py",
    "lightrag/api/routers/document_routes.py",
    "lightrag/api/routers/graph_routes.py",
    "lightrag/api/routers/ollama_api.py",
    "lightrag/api/routers/query_routes.py",
)
GRAPHITI_FILES = (
    "server/graph_service/main.py",
    "server/graph_service/routers/ingest.py",
    "server/graph_service/routers/retrieve.py",
)

# These tuples are an executable inventory of every production router decorator in
# the pinned files. Keeping file and handler provenance here prevents a coincidental
# count match from hiding cross-router leakage.
LIGHTRAG_ROUTES = (
    ("POST", "/documents/scan", "scan_for_new_documents", LIGHTRAG_FILES[1]),
    ("POST", "/documents/upload", "upload_to_input_dir", LIGHTRAG_FILES[1]),
    ("POST", "/documents/text", "insert_text", LIGHTRAG_FILES[1]),
    ("POST", "/documents/texts", "insert_texts", LIGHTRAG_FILES[1]),
    ("DELETE", "/documents", "clear_documents", LIGHTRAG_FILES[1]),
    ("GET", "/documents/pipeline_status", "get_pipeline_status", LIGHTRAG_FILES[1]),
    ("GET", "/documents", "documents", LIGHTRAG_FILES[1]),
    ("DELETE", "/documents/delete_document", "delete_document", LIGHTRAG_FILES[1]),
    ("POST", "/documents/clear_cache", "clear_cache", LIGHTRAG_FILES[1]),
    ("GET", "/documents/track_status/{track_id}", "get_track_status", LIGHTRAG_FILES[1]),
    ("POST", "/documents/paginated", "get_documents_paginated", LIGHTRAG_FILES[1]),
    ("GET", "/documents/status_counts", "get_document_status_counts", LIGHTRAG_FILES[1]),
    ("POST", "/documents/reprocess_failed", "reprocess_failed_documents", LIGHTRAG_FILES[1]),
    ("POST", "/documents/cancel_pipeline", "cancel_pipeline", LIGHTRAG_FILES[1]),
    ("GET", "/graph/label/list", "get_graph_labels", LIGHTRAG_FILES[2]),
    ("GET", "/graph/label/popular", "get_popular_labels", LIGHTRAG_FILES[2]),
    ("GET", "/graph/label/search", "search_labels", LIGHTRAG_FILES[2]),
    ("GET", "/graphs", "get_knowledge_graph", LIGHTRAG_FILES[2]),
    ("GET", "/graph/entity/exists", "check_entity_exists", LIGHTRAG_FILES[2]),
    ("POST", "/graph/entity/edit", "update_entity", LIGHTRAG_FILES[2]),
    ("POST", "/graph/relation/edit", "update_relation", LIGHTRAG_FILES[2]),
    ("POST", "/graph/entity/create", "create_entity", LIGHTRAG_FILES[2]),
    ("POST", "/graph/relation/create", "create_relation", LIGHTRAG_FILES[2]),
    ("POST", "/graph/entities/merge", "merge_entities", LIGHTRAG_FILES[2]),
    ("DELETE", "/graph/entity/delete", "delete_entity", LIGHTRAG_FILES[2]),
    ("DELETE", "/graph/relation/delete", "delete_relation", LIGHTRAG_FILES[2]),
    ("GET", "/api/version", "get_version", LIGHTRAG_FILES[3]),
    ("GET", "/api/tags", "get_tags", LIGHTRAG_FILES[3]),
    ("GET", "/api/ps", "get_running_models", LIGHTRAG_FILES[3]),
    ("POST", "/api/generate", "generate", LIGHTRAG_FILES[3]),
    ("POST", "/api/chat", "chat", LIGHTRAG_FILES[3]),
    ("POST", "/query", "query_text", LIGHTRAG_FILES[4]),
    ("POST", "/query/stream", "query_text_stream", LIGHTRAG_FILES[4]),
    ("POST", "/query/data", "query_data", LIGHTRAG_FILES[4]),
)
GRAPHITI_ROUTES = (
    ("POST", "/messages", "add_messages", GRAPHITI_FILES[1]),
    ("POST", "/entity-node", "add_entity_node", GRAPHITI_FILES[1]),
    ("DELETE", "/entity-edge/{uuid}", "delete_entity_edge", GRAPHITI_FILES[1]),
    ("DELETE", "/group/{group_id}", "delete_group", GRAPHITI_FILES[1]),
    ("DELETE", "/episode/{uuid}", "delete_episode", GRAPHITI_FILES[1]),
    ("POST", "/clear", "clear", GRAPHITI_FILES[1]),
    ("POST", "/search", "search", GRAPHITI_FILES[2]),
    ("GET", "/entity-edge/{uuid}", "get_entity_edge", GRAPHITI_FILES[2]),
    ("GET", "/episodes/{group_id}", "get_episodes", GRAPHITI_FILES[2]),
    ("POST", "/get-memory", "get_memory", GRAPHITI_FILES[2]),
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


def copy_files(repository: Path, destination: Path, files: tuple[str, ...]) -> None:
    for relative in files:
        source = repository / relative
        if not source.is_file():
            raise RuntimeError(f"missing pinned corpus file: {source}")
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)


def route_edges(snapshot: dict[str, Any]) -> list[tuple[dict[str, Any], dict[str, Any], dict[str, Any]]]:
    symbols = {symbol["id"]: symbol for symbol in snapshot["symbols"]}
    return [
        (symbols[edge["source_id"]], symbols[edge["target_id"]], edge)
        for edge in snapshot["relationships"]
        if symbols[edge["source_id"]]["kind"] == "route"
        and edge["evidence"]["provenance"] == "framework/fastapi-route"
    ]


def evaluate_repository(
    snapshot: dict[str, Any],
    expected: tuple[tuple[str, str, str, str], ...],
) -> dict[str, Any]:
    production_files = {item[3] for item in expected}
    actual_routes = [
        symbol
        for symbol in snapshot["symbols"]
        if symbol["kind"] == "route" and symbol["file"] in production_files
    ]
    edges = [
        item
        for item in route_edges(snapshot)
        if item[0]["file"] in production_files
    ]
    expected_keys = {(method + " " + path, handler, file) for method, path, handler, file in expected}
    actual_keys: list[tuple[str, str, str]] = []
    malformed: list[str] = []
    for route, handler, edge in edges:
        key = (route["name"], handler["name"], route["file"])
        actual_keys.append(key)
        if handler["file"] != route["file"] or edge["kind"] != "calls":
            malformed.append(f"{route['name']} -> {handler['qualified_name']}")

    missing = sorted(expected_keys - set(actual_keys))
    unexpected = sorted(set(actual_keys) - expected_keys)
    duplicate_edges = len(actual_keys) != len(set(actual_keys))
    route_names = [(route["name"], route["file"]) for route in actual_routes]
    duplicate_routes = len(route_names) != len(set(route_names))

    # Prefix failures are especially dangerous: they create plausible-looking,
    # globally bare routes while the application mounts a different public URL.
    forbidden_bare = {
        (method + " " + path, file)
        for method, path, _, file in expected
        if file.endswith("document_routes.py")
        for path in [path.removeprefix("/documents") or "/"]
    } | {
        (method + " " + path, file)
        for method, path, _, file in expected
        if file.endswith("ollama_api.py")
        for path in [path.removeprefix("/api") or "/"]
    }
    leaked_bare = sorted(set(route_names) & forbidden_bare)
    checks = {
        "exact_route_symbols": len(actual_routes) == len(expected),
        "exact_route_handler_edges": len(edges) == len(expected),
        "exact_route_handler_provenance": not missing and not unexpected,
        "no_duplicate_route_symbols": not duplicate_routes,
        "no_duplicate_route_edges": not duplicate_edges,
        "no_cross_router_targets": not malformed,
        "no_bare_prefixed_routes": not leaked_bare,
    }
    if not all(checks.values()):
        details = {
            "failed": [name for name, passed in checks.items() if not passed],
            "missing": missing,
            "unexpected": unexpected,
            "malformed": malformed,
            "leakedBare": leaked_bare,
        }
        raise RuntimeError(f"FastAPI acceptance failed: {json.dumps(details, sort_keys=True)}")
    return {
        "checks": checks,
        "routes": len(actual_routes),
        "routeHandlerEdges": len(edges),
    }


def index(binary: Path, corpus: Path) -> tuple[dict[str, Any], dict[str, Any], float]:
    started = time.perf_counter_ns()
    initialized = json.loads(run([str(binary), "init", str(corpus)]).stdout)
    wall_ms = (time.perf_counter_ns() - started) / 1_000_000
    snapshot = json.loads(run([str(binary), "snapshot", "--path", str(corpus)]).stdout)
    return initialized, snapshot, wall_ms


def fastapi_route_count(snapshot: dict[str, Any]) -> int:
    return sum(
        symbol["kind"] == "route"
        and any(
            edge["source_id"] == symbol["id"]
            and edge["evidence"]["provenance"] == "framework/fastapi-route"
            for edge in snapshot["relationships"]
        )
        for symbol in snapshot["symbols"]
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", required=True, type=Path)
    parser.add_argument("--lightrag-repository", required=True, type=Path)
    parser.add_argument("--graphiti-repository", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    binary = args.structurely.resolve()
    repositories = {
        "lightrag": args.lightrag_repository.resolve(),
        "graphiti": args.graphiti_repository.resolve(),
    }
    commits = pinned_commits()
    selected_files = {"lightrag": LIGHTRAG_FILES, "graphiti": GRAPHITI_FILES}
    for name, repository in repositories.items():
        verify_repository(name, repository, commits[name], selected_files[name])

    results: dict[str, Any] = {}
    with tempfile.TemporaryDirectory(prefix="structurely-fastapi-") as temporary:
        base = Path(temporary)
        for name, files, expected in (
            ("lightrag", LIGHTRAG_FILES, LIGHTRAG_ROUTES),
            ("graphiti", GRAPHITI_FILES, GRAPHITI_ROUTES),
        ):
            corpus = base / name
            copy_files(repositories[name], corpus, files)
            initialized, snapshot, wall_ms = index(binary, corpus)
            result = evaluate_repository(snapshot, expected)
            result.update(
                {
                    "files": len(snapshot["files"]),
                    "symbols": len(snapshot["symbols"]),
                    "relationships": len(snapshot["relationships"]),
                    "freshIndexWallMs": round(wall_ms, 3),
                    "engineDurationMs": initialized["duration_ms"],
                    "commit": commits[name],
                }
            )
            results[name] = result

        # Router modules describe potential endpoints, but none are publicly
        # deployed without a proven FastAPI application mount chain.
        unmounted_results: dict[str, int] = {}
        for name, files in (
            ("lightrag", LIGHTRAG_FILES[1:]),
            ("graphiti", GRAPHITI_FILES[1:]),
        ):
            corpus = base / f"{name}-unmounted"
            copy_files(repositories[name], corpus, files)
            _, snapshot, _ = index(binary, corpus)
            count = fastapi_route_count(snapshot)
            if count:
                raise RuntimeError(
                    f"expected zero unmounted {name} FastAPI routes, got {count}"
                )
            unmounted_results[name] = count

        # A changed factory mount prefix must propagate to every descendant.
        mounted_corpus = base / "lightrag-mounted-prefix"
        copy_files(repositories["lightrag"], mounted_corpus, LIGHTRAG_FILES)
        server = mounted_corpus / LIGHTRAG_FILES[0]
        original = "app.include_router(create_query_routes(rag, api_key, args.top_k))"
        replacement = (
            "app.include_router(create_query_routes(rag, api_key, args.top_k), "
            'prefix="/mounted")'
        )
        source = server.read_text()
        if source.count(original) != 1:
            raise RuntimeError("pinned LightRAG query-router mount changed")
        server.write_text(source.replace(original, replacement, 1))
        _, mounted_snapshot, _ = index(binary, mounted_corpus)
        mounted_expected = tuple(
            (
                method,
                f"/mounted{path}" if file == LIGHTRAG_FILES[4] else path,
                handler,
                file,
            )
            for method, path, handler, file in LIGHTRAG_ROUTES
        )
        evaluate_repository(mounted_snapshot, mounted_expected)

    total_routes = sum(result["routes"] for result in results.values())
    if total_routes != 44:
        raise RuntimeError(f"expected 44 mounted production routes, got {total_routes}")
    output = {
        "passed": True,
        "checks": {
            "pinnedRepositories": True,
            "exact44MountedProductionRoutes": True,
            "unmountedRoutersPublishZeroRoutes": unmounted_results,
            "factoryMountPrefixPropagates": True,
        },
        "structurelyBinarySha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "totalRoutes": total_routes,
        "repositories": results,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps(output, indent=2))


if __name__ == "__main__":
    main()
