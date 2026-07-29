#!/usr/bin/env python3
"""Gate per-translation-unit C compilation-database include resolution."""

from __future__ import annotations

import argparse
import hashlib
import json
import sqlite3
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

MODEL_VERSION = 66
PROVENANCE = "dynamic/c-function-pointer-dispatch"


def run(command: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command, cwd=cwd, check=True, text=True, capture_output=True
    )


def invoke_json(binary: Path, *arguments: str) -> tuple[dict[str, Any], float]:
    started = time.perf_counter_ns()
    payload = json.loads(run([str(binary), *arguments]).stdout)
    return payload, (time.perf_counter_ns() - started) / 1_000_000


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def normalized_database_sha256(path: Path) -> str:
    entries = json.loads(path.read_text())
    for entry in entries:
        entry["directory"] = "<ROOT>"
    payload = json.dumps(entries, indent=2, sort_keys=True) + "\n"
    return hashlib.sha256(payload.encode()).hexdigest()


def repository_identity(repository: Path) -> dict[str, Any]:
    return {
        "commit": run(["git", "rev-parse", "HEAD"], repository).stdout.strip(),
        "dirty": bool(run(["git", "status", "--porcelain"], repository).stdout.strip()),
    }


def write_fixture(root: Path) -> None:
    for directory in ("src", "include-a", "include-b", "include-shared"):
        (root / directory).mkdir(parents=True, exist_ok=True)
    (root / "include-a/config.h").write_text(
        "typedef int (*callback_t)(int);\n"
        "typedef struct Ops { callback_t run; callback_t stop; } Ops;\n"
    )
    (root / "include-b/config.h").write_text(
        "typedef int (*callback_t)(int);\n"
        "typedef struct Ops { callback_t stop; callback_t run; } Ops;\n"
    )
    (root / "src/a.c").write_text(
        "#include <config.h>\n"
        "static int alpha(int value) { return value + 1; }\n"
        "static int decoy_a(int value) { return value - 1; }\n"
        "static Ops table_a = { alpha, decoy_a };\n"
        "int dispatch_a(Ops *ops) { return ops->run(1); }\n"
    )
    (root / "src/b.c").write_text(
        "#include <config.h>\n"
        "static int beta(int value) { return value + 2; }\n"
        "static int decoy_b(int value) { return value - 2; }\n"
        "static Ops table_b = { decoy_b, beta };\n"
        "int dispatch_b(Ops *ops) { return ops->run(1); }\n"
    )
    (root / "include-shared/shared.h").write_text(
        "typedef struct SharedOps { int (*run)(int); } SharedOps;\n"
    )
    (root / "src/shared.c").write_text(
        "#include <shared.h>\n"
        "static int shared_target(int value) { return value + 3; }\n"
        "static SharedOps shared_table = { shared_target };\n"
        "int dispatch_shared(SharedOps *ops) { return ops->run(1); }\n"
    )


def write_database(root: Path, reverse: bool = False) -> None:
    entries = [
        {
            "directory": str(root),
            "file": "src/a.c",
            "arguments": ["cc", "-Iinclude-a", "-c", "src/a.c"],
            "command": "cc -Iinclude-b -c src/a.c",
        },
        {
            "directory": str(root),
            "file": "src/b.c",
            "arguments": ["cc", "-Iinclude-b", "-c", "src/b.c"],
        },
        {
            "directory": str(root),
            "file": "src/shared.c",
            "arguments": [
                "cc",
                "-isystem",
                "include-shared",
                "-c",
                "src/shared.c",
            ],
        },
    ]
    if reverse:
        entries[:2] = reversed(entries[:2])
    (root / "compile_commands.json").write_text(
        json.dumps(entries, indent=2, sort_keys=True) + "\n"
    )


def snapshot(binary: Path, root: Path, command: str) -> tuple[dict[str, Any], float]:
    report, wall_ms = invoke_json(binary, command, str(root))
    graph, _ = invoke_json(binary, "snapshot", "--path", str(root))
    if graph.get("graph_model_version") != MODEL_VERSION:
        raise RuntimeError(
            f"acceptance requires model v{MODEL_VERSION}, "
            f"got {graph.get('graph_model_version')}"
        )
    return {"report": report, "graph": graph}, wall_ms


def dispatch_targets(graph: dict[str, Any]) -> dict[str, list[str]]:
    symbols = {symbol["id"]: symbol for symbol in graph["symbols"]}
    targets: dict[str, set[str]] = {}
    for relationship in graph["relationships"]:
        evidence = relationship["evidence"]
        if evidence["provenance"] != PROVENANCE:
            continue
        source = symbols[relationship["source_id"]]["qualified_name"]
        target = symbols[relationship["target_id"]]["qualified_name"]
        if source in {"dispatch_a", "dispatch_b", "dispatch_shared"}:
            targets.setdefault(source, set()).add(target)
    return {
        source: sorted(targets.get(source, set()))
        for source in ("dispatch_a", "dispatch_b", "dispatch_shared")
    }


def require_targets(
    graph: dict[str, Any], expected: dict[str, list[str]], phase: str
) -> dict[str, list[str]]:
    actual = dispatch_targets(graph)
    if actual != expected:
        raise RuntimeError(
            f"{phase} targets mismatch: expected {expected}, got {actual}"
        )
    return actual


def codegraph_import_edges(root: Path) -> list[dict[str, Any]]:
    database = root / ".codegraph/codegraph.db"
    with sqlite3.connect(database) as connection:
        rows = connection.execute(
            """
            SELECT source.file_path,target.file_path,edges.line,edges.col,edges.metadata
            FROM edges
            JOIN nodes AS source ON source.id=edges.source
            JOIN nodes AS target ON target.id=edges.target
            WHERE edges.kind='imports'
            ORDER BY source.file_path,target.file_path,edges.line
            """
        ).fetchall()
    return [
        {
            "source": source,
            "target": target,
            "line": line,
            "column": column,
            "metadata": json.loads(metadata) if metadata else {},
        }
        for source, target, line, column, metadata in rows
    ]


def import_targets(edges: list[dict[str, Any]]) -> dict[str, list[str]]:
    sources = ("src/a.c", "src/b.c", "src/shared.c")
    return {
        source: sorted(
            edge["target"] for edge in edges if edge["source"] == source
        )
        for source in sources
    }


def codegraph_comparison(repository: Path) -> dict[str, Any]:
    binary = repository / "dist/bin/codegraph.js"
    if not binary.is_file():
        raise RuntimeError(f"pinned CodeGraph build does not exist: {binary}")
    version = run(["node", str(binary), "--version"]).stdout.strip()
    if version != "1.5.0":
        raise RuntimeError(f"expected CodeGraph 1.5.0, got {version}")
    identity = repository_identity(repository)
    with tempfile.TemporaryDirectory(prefix="codegraph-c-compdb-") as temporary:
        root = Path(temporary)
        write_fixture(root)
        write_database(root)
        run(["node", str(binary), "init", "."], root)
        fresh_imports = codegraph_import_edges(root)
        targets: dict[str, list[str]] = {}
        for caller in ("dispatch_a", "dispatch_b", "dispatch_shared"):
            payload = json.loads(
                run(
                    [
                        "node",
                        str(binary),
                        "callees",
                        caller,
                        "--path",
                        ".",
                        "--json",
                    ],
                    root,
                ).stdout
            )
            targets[caller] = sorted(
                {
                    callee["name"]
                    for callee in payload.get("callees", [])
                    if callee["name"]
                    in {
                        "alpha",
                        "beta",
                        "decoy_a",
                        "decoy_b",
                        "shared_target",
                    }
                }
            )
        write_database(root, reverse=True)
        sync_output = run(["node", str(binary), "sync", "."], root).stdout.strip()
        after_sync_imports = codegraph_import_edges(root)
        run(["node", str(binary), "index", "."], root)
        after_index_imports = codegraph_import_edges(root)
    expected_a_first = {
        "src/a.c": ["include-a/config.h"],
        "src/b.c": ["include-a/config.h"],
        "src/shared.c": ["include-shared/shared.h"],
    }
    expected_b_first = {
        "src/a.c": ["include-b/config.h"],
        "src/b.c": ["include-b/config.h"],
        "src/shared.c": ["include-shared/shared.h"],
    }
    if import_targets(fresh_imports) != expected_a_first:
        raise RuntimeError("CodeGraph fresh project-wide include union changed")
    if import_targets(after_sync_imports) != expected_a_first:
        raise RuntimeError("CodeGraph database-only sync behavior changed")
    if import_targets(after_index_imports) != expected_b_first:
        raise RuntimeError("CodeGraph full-index entry-order behavior changed")
    correct = targets == {
        "dispatch_a": ["alpha"],
        "dispatch_b": ["beta"],
        "dispatch_shared": ["shared_target"],
    }
    return {
        "version": version,
        "commit": identity["commit"],
        "worktreeDirty": identity["dirty"],
        "fixtureDispatchTargets": targets,
        "freshImportEdges": fresh_imports,
        "reorderedDatabase": {
            "syncOutput": sync_output,
            "afterSyncImportEdges": after_sync_imports,
            "afterFullIndexImportEdges": after_index_imports,
        },
        "perTranslationUnitTargetsCorrect": correct,
        "scope": "controlled two-translation-unit fixture only",
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--codegraph-repository", type=Path)
    arguments = parser.parse_args()
    binary = arguments.structurely.resolve()
    repository = Path(__file__).resolve().parents[1]
    identity = repository_identity(repository)
    with tempfile.TemporaryDirectory(prefix="structurely-c-compdb-") as temporary:
        root = Path(temporary)
        write_fixture(root)
        write_database(root)
        fresh, fresh_ms = snapshot(binary, root, "init")
        fresh_targets = require_targets(
            fresh["graph"],
            {
                "dispatch_a": ["alpha"],
                "dispatch_b": ["beta"],
                "dispatch_shared": ["shared_target"],
            },
            "fresh",
        )
        fresh_db_sha = normalized_database_sha256(root / "compile_commands.json")

        write_database(root, reverse=True)
        rebound, rebound_ms = snapshot(binary, root, "sync")
        rebound_targets = require_targets(
            rebound["graph"],
            {
                "dispatch_a": ["alpha"],
                "dispatch_b": ["beta"],
                "dispatch_shared": ["shared_target"],
            },
            "reordered",
        )
        if rebound["report"]["files_changed"] == 0:
            raise RuntimeError("database-only reorder did not trigger semantic refresh")
        no_op, no_op_ms = snapshot(binary, root, "sync")
        if no_op["report"]["files_changed"] != 0:
            raise RuntimeError("database reorder did not converge to a no-op sync")

        write_database(root)
        restored, restored_ms = snapshot(binary, root, "sync")
        restored_targets = require_targets(
            restored["graph"],
            {
                "dispatch_a": ["alpha"],
                "dispatch_b": ["beta"],
                "dispatch_shared": ["shared_target"],
            },
            "restored",
        )
        result: dict[str, Any] = {
            "passed": True,
            "checks": {
                "graphModelVersion66": True,
                "perTranslationUnitIncludeContexts": True,
                "angleIncludeSearchOrder": True,
                "databaseOnlyIncrementalCleanupAndRestore": True,
                "databaseOrderInvariant": True,
                "databaseOnlySyncInvalidation": True,
                "noOpSyncAfterDatabaseRefresh": True,
                "binaryAndSourceIdentityRecorded": True,
            },
            "graphModelVersion": MODEL_VERSION,
            "structurely": {
                "commit": identity["commit"],
                "worktreeDirty": identity["dirty"],
                "binarySha256": sha256(binary),
            },
            "fixture": {
                "freshDatabaseSha256": fresh_db_sha,
                "aSourceSha256": sha256(root / "src/a.c"),
                "bSourceSha256": sha256(root / "src/b.c"),
                "sharedSourceSha256": sha256(root / "src/shared.c"),
            },
            "fresh": {
                "wallMs": round(fresh_ms, 3),
                "engineDurationMs": fresh["report"]["duration_ms"],
                "targets": fresh_targets,
            },
            "incremental": {
                "rebind": {
                    "wallMs": round(rebound_ms, 3),
                    "engineDurationMs": rebound["report"]["duration_ms"],
                    "targets": rebound_targets,
                },
                "noOp": {
                    "wallMs": round(no_op_ms, 3),
                    "engineDurationMs": no_op["report"]["duration_ms"],
                    "filesChanged": no_op["report"]["files_changed"],
                },
                "restore": {
                    "wallMs": round(restored_ms, 3),
                    "engineDurationMs": restored["report"]["duration_ms"],
                    "targets": restored_targets,
                },
            },
        }
        if arguments.codegraph_repository:
            comparison = codegraph_comparison(arguments.codegraph_repository.resolve())
            result["codegraphComparison"] = comparison
            result["checks"]["pinnedCodeGraphFixtureComparison"] = True
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
