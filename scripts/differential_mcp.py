#!/usr/bin/env python3
"""Gate Structurely against CodeGraph 1.5.0 at the MCP stdio boundary."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path
from typing import Any

TOOLS = "explore,node,search,callers,callees,impact,status,files"


def run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, check=True, text=True, capture_output=True)


def prepare(source: Path, destination: Path) -> None:
    shutil.copytree(source, destination, ignore=shutil.ignore_patterns("scenarios.json"))


def session(command: list[str], requests: list[dict[str, Any]]) -> dict[str, Any]:
    environment = os.environ.copy()
    environment["CODEGRAPH_MCP_TOOLS"] = TOOLS
    process = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=environment,
    )
    assert process.stdin is not None
    assert process.stdout is not None
    responses: dict[str, Any] = {}
    try:
        for identifier, request in enumerate(requests, 1):
            wire_request = {"jsonrpc": "2.0", "id": identifier, **request}
            process.stdin.write(json.dumps(wire_request) + "\n")
            process.stdin.flush()
            line = process.stdout.readline()
            if not line:
                stderr = process.stderr.read() if process.stderr else ""
                raise RuntimeError(f"MCP server exited before response: {stderr}")
            responses[request["label"]] = json.loads(line)
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.terminate()
            process.wait(timeout=3)
    return responses


def result_text(response: dict[str, Any]) -> str:
    result = response.get("result", {})
    return "\n".join(
        item.get("text", "")
        for item in result.get("content", [])
        if item.get("type") == "text"
    )


def tool_names(response: dict[str, Any]) -> set[str]:
    return {tool["name"] for tool in response["result"]["tools"]}


def predicates(capture: dict[str, Any]) -> dict[str, bool]:
    expected_tools = {f"codegraph_{name}" for name in TOOLS.split(",")}
    return {
        "initialize": capture["initialize"]["result"]["protocolVersion"]
        in {"2024-11-05", "2025-03-26", "2025-06-18"},
        "tools": expected_tools <= tool_names(capture["tools"]),
        "search-exact": "showUser" in result_text(capture["search-exact"]),
        "search-ambiguous": result_text(capture["search-ambiguous"]).count("duplicate")
        >= 2,
        "callers": "showUser" in result_text(capture["callers"]),
        "callees": not capture["callees"]["result"].get("isError", False),
        "react-rerender": "render"
        in result_text(capture["react-rerender"]).lower(),
        "jsx-child-render": "Child" in result_text(capture["jsx-child-render"]),
        "interface-dispatch": all(
            value in result_text(capture["interface-dispatch"])
            for value in ("handle", "contracts.ts")
        ),
        "impact": "showUser" in result_text(capture["impact"]),
        "node-window": "showUser" in result_text(capture["node-window"]),
        "explore-flow": all(
            value in result_text(capture["explore-flow"])
            for value in ("registerRoutes", "showUser")
        ),
        "status": not capture["status"]["result"].get("isError", False),
        "files": all(
            value in result_text(capture["files"])
            for value in ("handlers.ts", "routes.ts")
        ),
        "missing-required": capture["missing-required"]["result"].get("isError") is True,
        "invalid-limit": "result" in capture["invalid-limit"],
        "missing-symbol": any(
            value in result_text(capture["missing-symbol"]).lower()
            for value in ("not found", "no indexed file or symbol matched")
        ),
    }


def context_usefulness(
    capture: dict[str, Any], expectations: dict[str, Any]
) -> dict[str, Any]:
    text = result_text(capture[expectations["scenario"]])
    required = expectations["requiredFacts"]
    relevant = set(expectations["relevantFiles"])
    irrelevant = set(expectations["irrelevantFiles"])
    headings = set(re.findall(r"\*\*`([^`]+)`", text))
    mentioned_relevant = sorted(file for file in relevant if file in text)
    mentioned_irrelevant = sorted(file for file in irrelevant if file in headings)
    required_recall = sum(fact in text for fact in required) / len(required)
    relevant_file_recall = len(mentioned_relevant) / len(relevant)
    heading_files = headings & (relevant | irrelevant)
    file_precision = (
        len(heading_files & relevant) / len(heading_files) if heading_files else 0.0
    )
    flow_spines = [
        all(fact in text for fact in spine) for spine in expectations["flowSpines"]
    ]
    source_is_line_numbered = bool(re.search(r"(?m)^\d+\t", text)) and "```" in text
    within_budget = len(text) <= expectations["maximumCharacters"]
    components = [
        required_recall,
        relevant_file_recall,
        file_precision,
        sum(flow_spines) / len(flow_spines),
        float(source_is_line_numbered),
        float(within_budget),
    ]
    return {
        "score": round(sum(components) / len(components), 4),
        "requiredFactRecall": round(required_recall, 4),
        "relevantFileRecall": round(relevant_file_recall, 4),
        "filePrecision": round(file_precision, 4),
        "mentionedRelevantFiles": mentioned_relevant,
        "mentionedIrrelevantFiles": mentioned_irrelevant,
        "flowSpines": flow_spines,
        "lineNumberedSource": source_is_line_numbered,
        "characters": len(text),
        "withinBudget": within_budget,
    }


def normalize(value: Any, roots: list[Path]) -> Any:
    if isinstance(value, dict):
        return {
            key: normalize(item, roots)
            for key, item in sorted(value.items())
            if key not in {"_meta", "daemonPid"}
        }
    if isinstance(value, list):
        return [normalize(item, roots) for item in value]
    if isinstance(value, str):
        for root in roots:
            value = value.replace(str(root), "<PROJECT>")
        return value
    return value


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", required=True, type=Path)
    parser.add_argument("--codegraph", required=True, type=Path)
    parser.add_argument(
        "--fixture", type=Path, default=Path("fixtures/differential/mcp-1.5.0")
    )
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--baseline", type=Path)
    args = parser.parse_args()

    manifest = json.loads((args.fixture / "scenarios.json").read_text())
    requests = manifest["requests"]
    with tempfile.TemporaryDirectory(prefix="structurely-differential-") as temporary:
        root = Path(temporary)
        structurely_project = root / "structurely"
        codegraph_project = root / "codegraph"
        prepare(args.fixture, structurely_project)
        prepare(args.fixture, codegraph_project)
        run([str(args.structurely.resolve()), "init", str(structurely_project)])
        run(["node", str(args.codegraph.resolve()), "init", str(codegraph_project)])
        structurely = session(
            [
                str(args.structurely.resolve()),
                "serve",
                "--mcp",
                "--path",
                str(structurely_project),
            ],
            requests,
        )
        codegraph = session(
            [
                "node",
                str(args.codegraph.resolve()),
                "serve",
                "--mcp",
                "--path",
                str(codegraph_project),
            ],
            requests,
        )
        normalized_codegraph = normalize(codegraph, [codegraph_project])
        normalized_structurely = normalize(structurely, [structurely_project])

    structurely_checks = predicates(structurely)
    codegraph_checks = predicates(codegraph)
    structurely_usefulness = context_usefulness(
        structurely, manifest["contextUsefulness"]
    )
    codegraph_usefulness = context_usefulness(codegraph, manifest["contextUsefulness"])
    shared = {
        label: structurely_checks[label] and codegraph_checks[label]
        for label in structurely_checks
    }
    report = {
        "pinnedCodeGraph": manifest["pinnedCodeGraph"],
        "compatibility": {
            "passed": sum(shared.values()),
            "total": len(shared),
            "score": round(sum(shared.values()) / len(shared), 4),
            "predicates": shared,
        },
        "structurely": {
            "passed": sum(structurely_checks.values()),
            "predicates": structurely_checks,
        },
        "codegraph": {
            "passed": sum(codegraph_checks.values()),
            "predicates": codegraph_checks,
        },
        "contextUsefulness": {
            "structurely": structurely_usefulness,
            "codegraph": codegraph_usefulness,
            "atLeastPinnedCodeGraph": (
                structurely_usefulness["score"] >= codegraph_usefulness["score"]
            ),
        },
        "captures": {
            "structurely": normalized_structurely,
            "codegraph": normalized_codegraph,
        },
    }
    if args.baseline:
        baseline = json.loads(args.baseline.read_text())
        report["baselineMatches"] = normalized_codegraph == baseline

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["compatibility"], indent=2))
    if not all(shared.values()) or not report["contextUsefulness"]["atLeastPinnedCodeGraph"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
