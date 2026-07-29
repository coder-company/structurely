#!/usr/bin/env python3
"""Gate bounded, source-ordered C macro callback-table resolution."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

MODEL_VERSION = 67
PINNED_CODEGRAPH_COMMIT = "572d22bfbe82602080e457bec655f72e3314f9ef"
PROVENANCE = "dynamic/c-function-pointer-dispatch"
CALLERS = (
    "dispatch_object",
    "dispatch_slot",
    "dispatch_whole",
    "dispatch_included",
    "dispatch_conditional",
    "dispatch_unknown",
    "dispatch_repeated",
    "dispatch_array",
    "dispatch_rows",
    "dispatch_collision",
    "dispatch_constant",
    "dispatch_factory_text",
    "dispatch_converged",
    "dispatch_mutated",
)


def run(command: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command, cwd=cwd, check=True, text=True, capture_output=True
    )


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_headers(root: Path, included_target: str) -> None:
    (root / "defs.h").write_text(
        f"#define INCLUDED {included_target}\n"
        "#define PASS(x) x\n"
        "#define SLOT(x) .run = PASS(x)\n"
        "#define MAKE(x) { .run = PASS(x) }\n"
    )
    (root / "first-branch.h").write_text(
        "#if 1\n#define FIRST_HEADER good_target\n#endif\n"
    )
    (root / "second-branch.h").write_text(
        "#if 0\n#define COLLIDING_HEADER_BAD impossible_target\n#endif\n"
    )
    (root / "repeated.h").write_text(
        "#ifdef REPEAT_SWITCH\n"
        "#define REPEATED repeated_second\n"
        "#else\n"
        "#define REPEATED repeated_first\n"
        "#endif\n"
    )


def source(object_target: str) -> str:
    return f"""\
#include "defs.h"
#include "first-branch.h"
#include "second-branch.h"
#undef REPEAT_SWITCH
#include "repeated.h"
#define REPEAT_SWITCH 1
#include "repeated.h"

typedef int (*Callback)(int);
typedef struct ObjectOps {{ int (*run)(int); }} ObjectOps;
typedef struct SlotOps {{ int (*run)(int); }} SlotOps;
typedef struct WholeOps {{ int (*run)(int); }} WholeOps;
typedef struct IncludedOps {{ int (*run)(int); }} IncludedOps;
typedef struct ConditionalOps {{ int (*run)(int); }} ConditionalOps;
typedef struct UnknownOps {{ int (*run)(int); }} UnknownOps;
typedef struct RepeatedOps {{ int (*run)(int); }} RepeatedOps;
typedef struct RowOps {{ int (*run)(int); }} RowOps;
typedef struct CollisionOps {{ int (*run)(int); }} CollisionOps;
typedef struct ConstantOps {{ int (*run)(int); }} ConstantOps;
typedef struct FactoryTextOps {{ int (*run)(int); }} FactoryTextOps;
typedef struct ConvergedOps {{ int (*run)(int); }} ConvergedOps;
typedef struct MutatedConditionOps {{ int (*run)(int); }} MutatedConditionOps;

static int object_first(int v) {{ return v + 1; }}
static int object_second(int v) {{ return v + 2; }}
static int slot_target(int v) {{ return v + 3; }}
static int whole_target(int v) {{ return v + 4; }}
static int included_first(int v) {{ return v + 5; }}
static int included_second(int v) {{ return v + 6; }}
static int active_target(int v) {{ return v + 7; }}
static int inactive_target(int v) {{ return v - 7; }}
static int unknown_yes(int v) {{ return v + 8; }}
static int unknown_no(int v) {{ return v + 9; }}
static int repeated_first(int v) {{ return v + 10; }}
static int repeated_second(int v) {{ return v + 11; }}
static int array_first(int v) {{ return v + 12; }}
static int array_second(int v) {{ return v + 13; }}
static int row_first(int v) {{ return v + 14; }}
static int row_second(int v) {{ return v + 15; }}
static int good_target(int v) {{ return v + 16; }}
static int impossible_target(int v) {{ return v - 16; }}
static Callback make_callback(void) {{ return good_target; }}

#define OBJECT {object_target}
static ObjectOps object_table = {{ OBJECT }};
static SlotOps slot_table = {{ SLOT(slot_target) }};
static WholeOps whole_table = MAKE(whole_target);
static IncludedOps included_table = {{ INCLUDED }};

#if 0
static ConditionalOps inactive_table = {{ inactive_target }};
#else
static ConditionalOps active_table = {{ active_target }};
#endif

#if EXTERNAL_FEATURE
static UnknownOps unknown_a = {{ unknown_yes }};
#else
static UnknownOps unknown_b = {{ unknown_no }};
#endif

static RepeatedOps repeated_table = {{ REPEATED }};
#define CALLBACKS {{ array_first, array_second }}
static Callback callbacks[] = CALLBACKS;
#define ROWS {{ {{ row_first }}, {{ row_second }} }}
static RowOps rows[] = ROWS;
static CollisionOps collision = {{ COLLIDING_HEADER_BAD }};

#if 0 /* deliberately disabled */
static ConstantOps commented_zero = {{ impossible_target }};
#endif
#if 0 && EXTERNAL_FEATURE
static ConstantOps false_and_unknown = {{ impossible_target }};
#endif
#if defined(EXTERNAL_FEATURE) && 0
static ConstantOps unknown_and_false = {{ impossible_target }};
#endif
#if 0x0
static ConstantOps hexadecimal_zero = {{ impossible_target }};
#endif
#if 00
static ConstantOps octal_zero = {{ impossible_target }};
#endif
#if 0U
static ConstantOps unsigned_zero = {{ impossible_target }};
#endif
#if 0L
static ConstantOps long_zero = {{ impossible_target }};
#endif
#if -0
static ConstantOps negative_zero = {{ impossible_target }};
#endif

#define GET_CALLBACK() make_callback()
static FactoryTextOps factory_text = {{ GET_CALLBACK() }};

#if UNKNOWN_0
#define CONVERGED_0 1
#else
#define CONVERGED_0 1
#endif
#if UNKNOWN_1
#define CONVERGED_1 1
#else
#define CONVERGED_1 1
#endif
#if UNKNOWN_2
#define CONVERGED_2 1
#else
#define CONVERGED_2 1
#endif
#if UNKNOWN_3
#define CONVERGED_3 1
#else
#define CONVERGED_3 1
#endif
#if UNKNOWN_4
#define CONVERGED_4 1
#else
#define CONVERGED_4 1
#endif
#if UNKNOWN_5
#define CONVERGED_5 1
#else
#define CONVERGED_5 1
#endif
#define CONVERGED_TARGET good_target
static ConvergedOps converged = {{ CONVERGED_TARGET }};

#define MUTATED_FLAG 1
#if MUTATED_FLAG
#undef MUTATED_FLAG
static MutatedConditionOps mutated_good = {{ good_target }};
#else
static MutatedConditionOps mutated_bad = {{ impossible_target }};
#endif

int dispatch_object(ObjectOps *ops) {{ return ops->run(1); }}
int dispatch_slot(SlotOps *ops) {{ return ops->run(1); }}
int dispatch_whole(WholeOps *ops) {{ return ops->run(1); }}
int dispatch_included(IncludedOps *ops) {{ return ops->run(1); }}
int dispatch_conditional(ConditionalOps *ops) {{ return ops->run(1); }}
int dispatch_unknown(UnknownOps *ops) {{ return ops->run(1); }}
int dispatch_repeated(RepeatedOps *ops) {{ return ops->run(1); }}
int dispatch_array(unsigned i) {{ return callbacks[i](1); }}
int dispatch_rows(RowOps *ops) {{ return ops->run(1); }}
int dispatch_collision(CollisionOps *ops) {{ return ops->run(1); }}
int dispatch_constant(ConstantOps *ops) {{ return ops->run(1); }}
int dispatch_factory_text(FactoryTextOps *ops) {{ return ops->run(1); }}
int dispatch_converged(ConvergedOps *ops) {{ return ops->run(1); }}
int dispatch_mutated(MutatedConditionOps *ops) {{ return ops->run(1); }}
"""


def write_fixture(root: Path, object_target: str, included_target: str) -> None:
    write_headers(root, included_target)
    (root / "main.c").write_text(source(object_target))


def structurely_snapshot(binary: Path, root: Path, command: str) -> dict[str, Any]:
    started = time.perf_counter_ns()
    report = json.loads(run([str(binary), command, str(root)]).stdout)
    wall_ms = (time.perf_counter_ns() - started) / 1_000_000
    graph = json.loads(
        run([str(binary), "snapshot", "--path", str(root)]).stdout
    )
    if graph.get("graph_model_version") != MODEL_VERSION:
        raise RuntimeError(
            f"acceptance requires model v{MODEL_VERSION}, "
            f"got {graph.get('graph_model_version')}"
        )
    return {"report": report, "wall_ms": wall_ms, "graph": graph}


def structurely_targets(graph: dict[str, Any]) -> dict[str, list[str]]:
    symbols = {symbol["id"]: symbol for symbol in graph["symbols"]}
    targets: dict[str, set[str]] = {caller: set() for caller in CALLERS}
    for relationship in graph["relationships"]:
        if relationship["evidence"]["provenance"] != PROVENANCE:
            continue
        caller = symbols[relationship["source_id"]]["name"]
        if caller in targets:
            targets[caller].add(symbols[relationship["target_id"]]["name"])
    return {caller: sorted(targets[caller]) for caller in CALLERS}


def expected_targets(object_target: str, included_target: str) -> dict[str, list[str]]:
    return {
        "dispatch_object": [object_target],
        "dispatch_slot": ["slot_target"],
        "dispatch_whole": ["whole_target"],
        "dispatch_included": [included_target],
        "dispatch_conditional": ["active_target"],
        "dispatch_unknown": ["unknown_no", "unknown_yes"],
        "dispatch_repeated": ["repeated_second"],
        "dispatch_array": ["array_first", "array_second"],
        "dispatch_rows": ["row_first", "row_second"],
        "dispatch_collision": [],
        "dispatch_constant": [],
        "dispatch_factory_text": [],
        "dispatch_converged": ["good_target"],
        "dispatch_mutated": ["good_target"],
    }


def require_targets(
    graph: dict[str, Any], object_target: str, included_target: str, phase: str
) -> dict[str, list[str]]:
    actual = structurely_targets(graph)
    expected = expected_targets(object_target, included_target)
    if actual != expected:
        raise RuntimeError(f"{phase} targets mismatch: expected {expected}, got {actual}")
    return actual


def codegraph_targets(repository: Path, root: Path) -> dict[str, list[str]]:
    binary = repository / "dist/bin/codegraph.js"
    targets: dict[str, list[str]] = {}
    known = {
        target
        for values in expected_targets("object_first", "included_first").values()
        for target in values
    } | {
        "object_second",
        "included_second",
        "inactive_target",
        "impossible_target",
        "make_callback",
        "repeated_first",
    }
    for caller in CALLERS:
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
                if callee["name"] in known
            }
        )
    return targets


def codegraph_comparison(repository: Path) -> dict[str, Any]:
    binary = repository / "dist/bin/codegraph.js"
    if not binary.is_file():
        raise RuntimeError(f"pinned CodeGraph build does not exist: {binary}")
    version = run(["node", str(binary), "--version"]).stdout.strip()
    commit = run(["git", "rev-parse", "HEAD"], repository).stdout.strip()
    dirty = bool(run(["git", "status", "--porcelain"], repository).stdout.strip())
    if version != "1.5.0" or commit != PINNED_CODEGRAPH_COMMIT or dirty:
        raise RuntimeError(
            f"expected clean CodeGraph 1.5.0 {PINNED_CODEGRAPH_COMMIT}, "
            f"got version={version} commit={commit} dirty={dirty}"
        )
    with tempfile.TemporaryDirectory(prefix="codegraph-c-macros-") as temporary:
        root = Path(temporary)
        write_fixture(root, "object_first", "included_first")
        started = time.perf_counter_ns()
        run(["node", str(binary), "init", "."], root)
        wall_ms = (time.perf_counter_ns() - started) / 1_000_000
        targets = codegraph_targets(repository, root)
    expected = expected_targets("object_first", "included_first")
    intended_resolved = sum(
        len(set(targets[caller]) & set(expected[caller])) for caller in CALLERS
    )
    return {
        "version": version,
        "commit": commit,
        "binary_sha256": sha256(binary),
        "fresh_index_wall_ms": wall_ms,
        "intended_targets_resolved": intended_resolved,
        "intended_targets_total": sum(map(len, expected.values())),
        "targets": targets,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", type=Path, required=True)
    parser.add_argument("--codegraph-repository", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()

    binary = arguments.structurely.resolve()
    if not binary.is_file():
        raise RuntimeError(f"Structurely binary does not exist: {binary}")
    with tempfile.TemporaryDirectory(prefix="structurely-c-macros-") as temporary:
        root = Path(temporary)
        write_fixture(root, "object_first", "included_first")
        initial = structurely_snapshot(binary, root, "init")
        initial_targets = require_targets(
            initial["graph"], "object_first", "included_first", "initial"
        )
        write_fixture(root, "object_second", "included_second")
        changed = structurely_snapshot(binary, root, "sync")
        changed_targets = require_targets(
            changed["graph"], "object_second", "included_second", "changed"
        )
        no_op = structurely_snapshot(binary, root, "sync")
        no_op_targets = require_targets(
            no_op["graph"], "object_second", "included_second", "no-op"
        )
        if changed["report"].get("files_changed", 0) < 2:
            raise RuntimeError("source and included-header edits were not both observed")
        if no_op["report"].get("files_changed") != 0:
            raise RuntimeError("no-op sync did not converge")

    payload: dict[str, Any] = {
        "passed": True,
        "model_version": MODEL_VERSION,
        "structurely": {
            "binary_sha256": sha256(binary),
            "initial_wall_ms": initial["wall_ms"],
            "changed_wall_ms": changed["wall_ms"],
            "no_op_wall_ms": no_op["wall_ms"],
            "initial_report": initial["report"],
            "changed_report": changed["report"],
            "no_op_report": no_op["report"],
            "initial_targets": initial_targets,
            "changed_targets": changed_targets,
            "no_op_targets": no_op_targets,
            "intended_targets_resolved": sum(map(len, initial_targets.values())),
            "intended_targets_total": sum(
                map(
                    len,
                    expected_targets("object_first", "included_first").values(),
                )
            ),
        },
    }
    if arguments.codegraph_repository:
        payload["codegraph"] = codegraph_comparison(
            arguments.codegraph_repository.resolve()
        )
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")
    print(json.dumps(payload, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
