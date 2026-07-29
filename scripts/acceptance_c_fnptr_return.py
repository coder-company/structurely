#!/usr/bin/env python3
"""Gate same-file C++ function-pointer factory-return dispatch."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

MODEL_VERSION = 65
PROVENANCE = "dynamic/c-function-pointer-dispatch"


def run(command: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command, cwd=cwd, check=True, text=True, capture_output=True
    )


def invoke_json(
    binary: Path, *arguments: str
) -> tuple[dict[str, Any], float]:
    started = time.perf_counter_ns()
    payload = json.loads(run([str(binary), *arguments]).stdout)
    return payload, (time.perf_counter_ns() - started) / 1_000_000


def repository_identity(repository: Path) -> dict[str, Any]:
    commit = run(["git", "rev-parse", "HEAD"], repository).stdout.strip()
    dirty = bool(run(["git", "status", "--porcelain"], repository).stdout.strip())
    return {"commit": commit, "dirty": dirty}


def codegraph_comparison(repository: Path, fixture_source: str) -> dict[str, Any]:
    binary = repository / "dist/bin/codegraph.js"
    if not binary.is_file():
        raise RuntimeError(f"pinned CodeGraph build does not exist: {binary}")
    version = run(["node", str(binary), "--version"]).stdout.strip()
    if version != "1.5.0":
        raise RuntimeError(f"expected CodeGraph 1.5.0, got {version}")
    identity = repository_identity(repository)
    with tempfile.TemporaryDirectory(prefix="codegraph-c-fnptr-return-") as temporary:
        corpus = Path(temporary)
        (corpus / "factory.cpp").write_text(fixture_source)
        run(["node", str(binary), "init", str(corpus)])
        callers: dict[str, list[str]] = {}
        for caller in (
            "local_dispatch",
            "exact_local_dispatch",
            "explicit_local_dispatch",
            "immediate_dispatch",
        ):
            payload = json.loads(
                run(
                    [
                        "node",
                        str(binary),
                        "callees",
                        caller,
                        "--path",
                        str(corpus),
                        "--json",
                    ]
                ).stdout
            )
            callers[caller] = sorted(
                {callee["name"] for callee in payload.get("callees", [])}
            )
    returned_targets = {"alpha", "beta"}
    unexpected = {
        caller: sorted(returned_targets.intersection(callees))
        for caller, callees in callers.items()
        if returned_targets.intersection(callees)
    }
    if unexpected:
        raise RuntimeError(
            "CodeGraph comparison unexpectedly found returned targets: "
            + json.dumps(unexpected, sort_keys=True)
        )
    return {
        "version": version,
        "commit": identity["commit"],
        "worktreeDirty": identity["dirty"],
        "fixtureCallees": callers,
        "returnedFactoryTargetEdges": 0,
        "scope": (
            "CLI callees on this controlled fixture only; direct edges to the "
            "factory itself are retained and are not returned-target edges"
        ),
    }


def source(choose_return: str) -> str:
    return (
        "static int alpha(int value) { return value + 1; }\n"
        "static int beta(int value) { return value + 2; }\n"
        "static int decoy(int value) { return value + 3; }\n"
        "typedef int (*callback_t)(int);\n"
        f"callback_t choose(bool split) {{ {choose_return} }}\n"
        "auto exact_factory() { return &alpha; }\n"
        "int (*explicit_factory(bool split))(int) {\n"
        "  return split ? &alpha : &beta;\n"
        "}\n"
        "int scalar_factory() { return 7; }\n"
        "callback_t unsafe_parameter(callback_t alpha) { return &alpha; }\n"
        "callback_t unsafe_local() { callback_t alpha = nullptr; return &alpha; }\n"
        "callback_t unsafe_uninitialized() { callback_t alpha; return &alpha; }\n"
        "auto lambda_factory() { return []() { return &alpha; }; }\n"
        "int local_dispatch(bool split, int value) {\n"
        "  auto pointer = choose(split);\n"
        "  return pointer(value);\n"
        "}\n"
        "int exact_local_dispatch(int value) {\n"
        "  auto pointer = exact_factory();\n"
        "  return pointer(value);\n"
        "}\n"
        "int explicit_local_dispatch(bool split, int value) {\n"
        "  auto pointer = explicit_factory(split);\n"
        "  return pointer(value);\n"
        "}\n"
        "int immediate_dispatch(bool split, int value) {\n"
        "  return choose(split)(value);\n"
        "}\n"
        "int rejected_scalar(int value) {\n"
        "  auto pointer = scalar_factory();\n"
        "  return pointer(value);\n"
        "}\n"
        "int rejected_parameter(int value) {\n"
        "  auto pointer = unsafe_parameter(&alpha);\n"
        "  return pointer(value);\n"
        "}\n"
        "int rejected_local(int value) {\n"
        "  auto pointer = unsafe_local();\n"
        "  return pointer(value);\n"
        "}\n"
        "int rejected_uninitialized(int value) {\n"
        "  auto pointer = unsafe_uninitialized();\n"
        "  return pointer(value);\n"
        "}\n"
        "int rejected_lambda(int value) { return lambda_factory()(value); }\n"
        "int rejected_killed(bool split, int value) {\n"
        "  auto pointer = choose(split);\n"
        "  pointer = scalar_factory();\n"
        "  return pointer(value);\n"
        "}\n"
    )


def initialize(
    binary: Path, corpus: Path
) -> tuple[dict[str, Any], dict[str, Any], float]:
    report, wall_ms = invoke_json(binary, "init", str(corpus))
    snapshot, _ = invoke_json(binary, "snapshot", "--path", str(corpus))
    assert_model(snapshot)
    return report, snapshot, wall_ms


def synchronize(
    binary: Path, corpus: Path
) -> tuple[dict[str, Any], dict[str, Any], float]:
    report, wall_ms = invoke_json(binary, "sync", str(corpus))
    snapshot, _ = invoke_json(binary, "snapshot", "--path", str(corpus))
    assert_model(snapshot)
    return report, snapshot, wall_ms


def assert_model(snapshot: dict[str, Any]) -> None:
    actual = snapshot.get("graph_model_version")
    if actual != MODEL_VERSION:
        raise RuntimeError(
            f"acceptance requires graph model v{MODEL_VERSION}, got v{actual}"
        )


def dynamic_edges(snapshot: dict[str, Any]) -> list[dict[str, Any]]:
    symbols = {symbol["id"]: symbol for symbol in snapshot["symbols"]}
    edges = []
    for relationship in snapshot["relationships"]:
        evidence = relationship["evidence"]
        if evidence["provenance"] != PROVENANCE:
            continue
        source_symbol = symbols[relationship["source_id"]]
        target_symbol = symbols[relationship["target_id"]]
        edges.append(
            {
                "source": source_symbol["qualified_name"],
                "target": target_symbol["qualified_name"],
                "kind": relationship["kind"],
                "confidence": evidence["confidence"],
                "file": evidence["file"],
                "line": evidence["line"],
                "site": evidence["site"],
                "explanation": evidence["explanation"],
            }
        )
    return sorted(
        edges,
        key=lambda edge: (
            edge["source"],
            edge["target"],
            edge["line"],
            edge["site"],
        ),
    )


def by_source(edges: list[dict[str, Any]]) -> dict[str, list[dict[str, Any]]]:
    grouped: dict[str, list[dict[str, Any]]] = {}
    for edge in edges:
        grouped.setdefault(edge["source"], []).append(edge)
    return grouped


def target_confidences(edges: list[dict[str, Any]]) -> list[tuple[str, float]]:
    return sorted((edge["target"], edge["confidence"]) for edge in edges)


def evaluate(snapshot: dict[str, Any], exact: bool) -> dict[str, Any]:
    edges = dynamic_edges(snapshot)
    grouped = by_source(edges)
    expected_choice = (
        [("beta", 0.995)]
        if exact
        else [("alpha", 0.97), ("beta", 0.97)]
    )
    for caller in ("local_dispatch", "immediate_dispatch"):
        if target_confidences(grouped.get(caller, [])) != expected_choice:
            raise RuntimeError(
                f"{caller} factory-return targets mismatch: "
                + json.dumps(grouped.get(caller, []), sort_keys=True)
            )
    if target_confidences(grouped.get("explicit_local_dispatch", [])) != [
        ("alpha", 0.97),
        ("beta", 0.97),
    ]:
        raise RuntimeError("explicit multi-return factory was not may-call at 0.97")
    if target_confidences(grouped.get("exact_local_dispatch", [])) != [
        ("alpha", 0.995)
    ]:
        raise RuntimeError("single-return factory was not exact at 0.995")
    rejected = (
        "rejected_scalar",
        "rejected_parameter",
        "rejected_local",
        "rejected_uninitialized",
        "rejected_lambda",
        "rejected_killed",
    )
    leaked = {caller: grouped[caller] for caller in rejected if grouped.get(caller)}
    if leaked:
        raise RuntimeError(
            "unsupported factory shapes produced dynamic edges: "
            + json.dumps(leaked, sort_keys=True)
        )
    malformed = [
        edge
        for edge in edges
        if edge["kind"] != "calls"
        or edge["file"] != "factory.cpp"
        or not isinstance(edge["site"], int)
        or edge["site"] <= 0
        or not isinstance(edge["line"], int)
        or edge["line"] <= 0
    ]
    identities = [
        (edge["source"], edge["target"], edge["file"], edge["site"])
        for edge in edges
    ]
    if malformed or len(identities) != len(set(identities)):
        raise RuntimeError("malformed or duplicate factory-return evidence")
    return {
        "checks": {
            "localFactoryDispatch": True,
            "immediateFactoryDispatch": True,
            "singleTargetExactConfidence0995": True,
            "multiTargetMayConfidence097": not exact,
            "unsafeShapesRejected": True,
            "ownedNonzeroEvidenceSites": True,
            "noDuplicateEvidenceIdentities": True,
        },
        "dynamicEdges": len(edges),
        "edges": edges,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", required=True, type=Path)
    parser.add_argument(
        "--structurely-repository",
        type=Path,
        default=Path(__file__).resolve().parents[1],
    )
    parser.add_argument("--codegraph-repository", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    binary = args.structurely.resolve()
    repository = args.structurely_repository.resolve()
    if not binary.is_file():
        raise SystemExit(f"Structurely binary does not exist: {binary}")
    identity = repository_identity(repository)

    initial_source = source("return (&decoy != nullptr) ? &alpha : &beta;")
    rebound_source = source("return &beta;")
    with tempfile.TemporaryDirectory(
        prefix="structurely-c-fnptr-return-"
    ) as temporary:
        corpus = Path(temporary)
        source_file = corpus / "factory.cpp"
        source_file.write_text(initial_source)
        initialized, snapshot, init_wall_ms = initialize(binary, corpus)
        initial = evaluate(snapshot, exact=False)

        source_file.write_text(rebound_source)
        synced, rebound_snapshot, sync_wall_ms = synchronize(binary, corpus)
        rebound = evaluate(rebound_snapshot, exact=True)
        if synced["files_changed"] != 1:
            raise RuntimeError("factory return rewrite did not change exactly one file")

        source_file.write_text(initial_source)
        restored, restored_snapshot, restore_wall_ms = synchronize(binary, corpus)
        restored_result = evaluate(restored_snapshot, exact=False)
        if restored["files_changed"] != 1:
            raise RuntimeError("factory return restoration did not change exactly one file")

    result = {
        "passed": True,
        "checks": {
            "graphModelVersion65": True,
            "sameFileFactoryReturnFlow": True,
            "localAndImmediateDispatch": True,
            "exactAndMayConfidence": True,
            "rejectionCasesFailClosed": True,
            "incrementalTargetCleanupAndRestore": True,
            "binaryAndSourceIdentityRecorded": True,
        },
        "graphModelVersion": MODEL_VERSION,
        "structurely": {
            "commit": identity["commit"],
            "worktreeDirty": identity["dirty"],
            "binarySha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        },
        "fixture": {
            "file": "factory.cpp",
            "initialSha256": hashlib.sha256(initial_source.encode()).hexdigest(),
            "reboundSha256": hashlib.sha256(rebound_source.encode()).hexdigest(),
        },
        "fresh": {
            "wallMs": round(init_wall_ms, 3),
            "engineDurationMs": initialized["duration_ms"],
            **initial,
        },
        "incremental": {
            "rebind": {
                "wallMs": round(sync_wall_ms, 3),
                "engineDurationMs": synced["duration_ms"],
                **rebound,
            },
            "restore": {
                "wallMs": round(restore_wall_ms, 3),
                "engineDurationMs": restored["duration_ms"],
                **restored_result,
            },
        },
    }
    if args.codegraph_repository is not None:
        result["codegraphComparison"] = codegraph_comparison(
            args.codegraph_repository.resolve(), initial_source
        )
        result["checks"]["pinnedCodeGraphFixtureComparison"] = True
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
