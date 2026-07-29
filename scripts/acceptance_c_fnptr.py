#!/usr/bin/env python3
"""Gate exact C/C++ function-pointer dispatch on pinned OpenHarmony sources."""

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
PROVENANCE = "dynamic/c-function-pointer-dispatch"

AUDIO_BASE = "code/BasicFeature/Media/AudioToVideoSync/entry/src/main/cpp"
AUDIO_HEADER = f"{AUDIO_BASE}/common/SampleInfo.h"
AUDIO_PLAYER_HEADER = f"{AUDIO_BASE}/player/Player.h"
AUDIO_NATIVE_HEADER = f"{AUDIO_BASE}/player/PlayerNative.h"
AUDIO_PLAYER = f"{AUDIO_BASE}/player/Player.cpp"
AUDIO_NATIVE = f"{AUDIO_BASE}/player/PlayerNative.cpp"
AUDIO_FILES = (
    AUDIO_HEADER,
    AUDIO_PLAYER_HEADER,
    AUDIO_NATIVE_HEADER,
    AUDIO_PLAYER,
    AUDIO_NATIVE,
)

AVCODEC_BASE = "code/BasicFeature/Media/AVCodec/entry/src/main/cpp"
AVCODEC_HEADER = f"{AVCODEC_BASE}/common/sample_info.h"
AVCODEC_PLAYER_HEADER = f"{AVCODEC_BASE}/sample/player/Player.h"
AVCODEC_NATIVE_HEADER = f"{AVCODEC_BASE}/sample/player/PlayerNative.h"
AVCODEC_PLAYER = f"{AVCODEC_BASE}/sample/player/Player.cpp"
AVCODEC_NATIVE = f"{AVCODEC_BASE}/sample/player/PlayerNative.cpp"
AVCODEC_FILES = (
    AVCODEC_HEADER,
    AVCODEC_PLAYER_HEADER,
    AVCODEC_NATIVE_HEADER,
    AVCODEC_PLAYER,
    AVCODEC_NATIVE,
)

SAMPLERATE_BASE = (
    "code/AI/MindSporeLiteCDemoASR/entry/src/main/cpp/"
    "third_party/libsamplerate"
)
SAMPLERATE_COMMON = f"{SAMPLERATE_BASE}/src/common.h"
SAMPLERATE_DRIVER = f"{SAMPLERATE_BASE}/src/samplerate.c"
SAMPLERATE_LINEAR = f"{SAMPLERATE_BASE}/src/src_linear.c"
SAMPLERATE_ZOH = f"{SAMPLERATE_BASE}/src/src_zoh.c"
SAMPLERATE_SINC = f"{SAMPLERATE_BASE}/src/src_sinc.c"
SAMPLERATE_PUBLIC = f"{SAMPLERATE_BASE}/include/samplerate.h"
SAMPLERATE_VARISPEED = f"{SAMPLERATE_BASE}/examples/varispeed-play.c"
SAMPLERATE_CALLBACK_TEST = f"{SAMPLERATE_BASE}/tests/callback_test.c"
SAMPLERATE_CALLBACK_HANG = f"{SAMPLERATE_BASE}/tests/callback_hang_test.c"
SAMPLERATE_FILES = (
    SAMPLERATE_COMMON,
    SAMPLERATE_DRIVER,
    SAMPLERATE_LINEAR,
    SAMPLERATE_ZOH,
    SAMPLERATE_SINC,
    SAMPLERATE_PUBLIC,
    SAMPLERATE_VARISPEED,
    SAMPLERATE_CALLBACK_TEST,
    SAMPLERATE_CALLBACK_HANG,
)
SAMPLERATE_PROCESS_TARGETS = frozenset(
    {
        "linear_vari_process",
        "zoh_vari_process",
        "sinc_multichan_vari_process",
        "sinc_hex_vari_process",
        "sinc_quad_vari_process",
        "sinc_stereo_vari_process",
        "sinc_mono_vari_process",
    }
)
SAMPLERATE_CALLBACK_TARGETS = frozenset(
    {
        "src_input_callback",
        "test_callback_func",
        "eos_callback_func",
        "input_callback",
    }
)


def run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, check=True, text=True, capture_output=True)


def openharmony_commit() -> str:
    manifest = json.loads(MANIFEST.read_text())
    matches = [
        repository["commit"]
        for repository in manifest["repositories"]
        if repository["name"] == "openharmony"
    ]
    if len(matches) != 1:
        raise RuntimeError("real-repositories manifest must pin OpenHarmony once")
    return matches[0]


def verify_repository(repository: Path, commit: str, files: tuple[str, ...]) -> None:
    actual = run(["git", "-C", str(repository), "rev-parse", "HEAD"]).stdout.strip()
    if actual != commit:
        raise RuntimeError(f"expected pinned OpenHarmony commit {commit}, got {actual}")
    changed = subprocess.run(
        ["git", "-C", str(repository), "diff", "--quiet", "HEAD", "--", *files],
        check=False,
    )
    if changed.returncode not in {0, 1}:
        raise RuntimeError("could not verify selected OpenHarmony source files")
    if changed.returncode == 1:
        raise RuntimeError("OpenHarmony has modified pinned corpus files")
    missing = [relative for relative in files if not (repository / relative).is_file()]
    if missing:
        raise RuntimeError(f"OpenHarmony is missing pinned corpus files: {missing}")


def copy_files(repository: Path, destination: Path, files: tuple[str, ...]) -> None:
    for relative in files:
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(repository / relative, target)


def invoke_json(binary: Path, *arguments: str) -> tuple[dict[str, Any], float]:
    started = time.perf_counter_ns()
    payload = json.loads(run([str(binary), *arguments]).stdout)
    wall_ms = (time.perf_counter_ns() - started) / 1_000_000
    return payload, wall_ms


def initialize(
    binary: Path, corpus: Path
) -> tuple[dict[str, Any], dict[str, Any], float]:
    report, wall_ms = invoke_json(binary, "init", str(corpus))
    snapshot, _ = invoke_json(binary, "snapshot", "--path", str(corpus))
    return report, snapshot, wall_ms


def synchronize(
    binary: Path, corpus: Path
) -> tuple[dict[str, Any], dict[str, Any], float]:
    report, wall_ms = invoke_json(binary, "sync", str(corpus))
    snapshot, _ = invoke_json(binary, "snapshot", "--path", str(corpus))
    return report, snapshot, wall_ms


def dispatch_edges(
    snapshot: dict[str, Any],
) -> list[tuple[dict[str, Any], dict[str, Any], dict[str, Any]]]:
    symbols = {symbol["id"]: symbol for symbol in snapshot["symbols"]}
    return [
        (symbols[edge["source_id"]], symbols[edge["target_id"]], edge)
        for edge in snapshot["relationships"]
        if edge["evidence"]["provenance"] == PROVENANCE
    ]


def edge_record(
    source: dict[str, Any], target: dict[str, Any], edge: dict[str, Any]
) -> tuple[str, str, str, str, str]:
    return (
        source["qualified_name"],
        target["qualified_name"],
        source["file"],
        target["file"],
        edge["kind"],
    )


def assert_well_formed(
    edges: list[tuple[dict[str, Any], dict[str, Any], dict[str, Any]]],
) -> None:
    malformed = []
    identities = []
    for source, target, edge in edges:
        evidence = edge["evidence"]
        identities.append(
            (source["id"], target["id"], evidence["file"], evidence.get("site"))
        )
        if (
            edge["kind"] != "calls"
            or source["kind"] not in {"function", "method"}
            or target["kind"] not in {"function", "method"}
            or evidence["file"] != source["file"]
            or not isinstance(evidence.get("site"), int)
            or evidence["site"] <= 0
            or not isinstance(evidence.get("line"), int)
            or evidence["line"] <= 0
        ):
            malformed.append(
                {
                    "source": source["qualified_name"],
                    "target": target["qualified_name"],
                    "kind": edge["kind"],
                    "evidence": evidence,
                }
            )
    if malformed:
        raise RuntimeError(
            "malformed C function-pointer dispatch edges: "
            + json.dumps(malformed, sort_keys=True)
        )
    if len(identities) != len(set(identities)):
        raise RuntimeError("duplicate C function-pointer dispatch evidence identities")


def evaluate_play_done(
    snapshot: dict[str, Any],
    player_file: str,
    native_file: str,
    expected_target: str = "Callback",
) -> dict[str, Any]:
    edges = dispatch_edges(snapshot)
    assert_well_formed(edges)
    actual = [edge_record(source, target, edge) for source, target, edge in edges]
    expected = [
        (
            "Release",
            expected_target,
            player_file,
            native_file,
            "calls",
        )
    ]
    if actual != expected:
        raise RuntimeError(
            "playDoneCallback exact dispatch mismatch: "
            + json.dumps(
                {
                    "expected": expected,
                    "actual": actual,
                },
                sort_keys=True,
            )
        )
    source, target, edge = edges[0]
    expected_line = 243 if player_file == AUDIO_PLAYER else 366
    if edge["evidence"]["line"] != expected_line:
        raise RuntimeError(
            f"expected invocation evidence line {expected_line}, "
            f"got {edge['evidence']['line']}"
        )
    return {
        "checks": {
            "exactReleaseToCallbackEdge": True,
            "qualifiedSourceAndTarget": True,
            "sourceAndTargetFiles": True,
            "callsWithOwnedInvocationEvidence": True,
            "stableNonzeroEvidenceSite": True,
            "noLeakageOrDuplicates": True,
        },
        "directEdges": 1,
        "edge": {
            "source": source["qualified_name"],
            "target": target["qualified_name"],
            "sourceFile": source["file"],
            "targetFile": target["file"],
            "evidenceLine": edge["evidence"]["line"],
            "evidenceSite": edge["evidence"]["site"],
            "provenance": edge["evidence"]["provenance"],
        },
    }


def evaluate_samplerate(snapshot: dict[str, Any]) -> dict[str, Any]:
    edges = dispatch_edges(snapshot)
    assert_well_formed(edges)
    process = [
        (source, target, edge)
        for source, target, edge in edges
        if source["qualified_name"] == "src_process"
    ]
    target_names = {target["qualified_name"] for _, target, _ in process}
    if target_names != SAMPLERATE_PROCESS_TARGETS:
        raise RuntimeError(
            "libsamplerate src_process target inventory mismatch: "
            + json.dumps(
                {
                    "missing": sorted(SAMPLERATE_PROCESS_TARGETS - target_names),
                    "unexpected": sorted(target_names - SAMPLERATE_PROCESS_TARGETS),
                },
                sort_keys=True,
            )
        )
    sites = {edge["evidence"]["site"] for _, _, edge in process}
    lines = {edge["evidence"]["line"] for _, _, edge in process}
    # const_process and vari_process are distinct invocation sites. Each can
    # select all seven table-backed implementations.
    if len(process) != 14 or len(sites) != 2 or lines != {138, 140}:
        raise RuntimeError(
            "expected 14 src_process edges at the two exact dispatch sites: "
            + json.dumps(
                {"edges": len(process), "sites": sorted(sites), "lines": sorted(lines)}
            )
        )
    malformed = [
        edge_record(source, target, edge)
        for source, target, edge in process
        if source["file"] != SAMPLERATE_DRIVER
        or target["file"]
        not in {SAMPLERATE_LINEAR, SAMPLERATE_ZOH, SAMPLERATE_SINC}
    ]
    if malformed:
        raise RuntimeError(
            "libsamplerate dispatch crossed an unexpected file boundary: "
            + json.dumps(malformed)
        )
    callback_read = [
        (source, target, edge)
        for source, target, edge in edges
        if source["qualified_name"] == "src_callback_read"
    ]
    callback_targets = {
        target["qualified_name"] for _, target, _ in callback_read
    }
    if callback_targets != SAMPLERATE_CALLBACK_TARGETS:
        raise RuntimeError(
            "libsamplerate callback formal-to-field inventory mismatch: "
            + json.dumps(
                {
                    "missing": sorted(
                        SAMPLERATE_CALLBACK_TARGETS - callback_targets
                    ),
                    "unexpected": sorted(
                        callback_targets - SAMPLERATE_CALLBACK_TARGETS
                    ),
                },
                sort_keys=True,
            )
        )
    if len(callback_read) != 4 or {
        edge["evidence"]["line"] for _, _, edge in callback_read
    } != {195}:
        raise RuntimeError(
            "expected four src_callback_read edges at line 195"
        )
    return {
        "checks": {
            "exactSevenProcessTargetFunctions": True,
            "exactFourteenProcessEdges": True,
            "exactConstAndVariableDispatchSites": True,
            "qualifiedSourceAndTargetFiles": True,
            "stableNonzeroEvidenceSites": True,
            "noDuplicateEvidenceIdentities": True,
            "exactFormalToStoredCallbackTargets": True,
        },
        "srcProcessEdges": len(process),
        "srcProcessTargets": sorted(target_names),
        "callbackReadEdges": len(callback_read),
        "callbackReadTargets": sorted(callback_targets),
        "allCorpusDispatchEdges": len(edges),
    }


def assert_no_dispatch(snapshot: dict[str, Any]) -> None:
    edges = dispatch_edges(snapshot)
    if edges:
        raise RuntimeError(
            "removed binding left stale function-pointer dispatch edges: "
            + json.dumps(
                [edge_record(source, target, edge) for source, target, edge in edges]
            )
        )


def evaluate_incremental_rebinding(
    binary: Path, corpus: Path
) -> dict[str, Any]:
    native = corpus / AUDIO_NATIVE
    original = native.read_text()
    assignment = "sampleInfo.playDoneCallback = &Callback;"
    if original.count(assignment) != 1:
        raise RuntimeError("pinned AudioToVideoSync callback assignment changed")

    native.write_text(original.replace(assignment, "sampleInfo.playDoneCallback = nullptr;"))
    cleanup_report, cleanup_snapshot, cleanup_wall_ms = synchronize(binary, corpus)
    assert_no_dispatch(cleanup_snapshot)

    replacement_definition = (
        "void ReplacementCallback(void *asyncContext)\\n"
        "{\\n"
        "    (void)asyncContext;\\n"
        "}\\n\\n"
    )
    marker = "void Callback(void *asyncContext)"
    if original.count(marker) != 1:
        raise RuntimeError("pinned AudioToVideoSync callback definition changed")
    rebound = original.replace(marker, replacement_definition + marker).replace(
        assignment, "sampleInfo.playDoneCallback = &ReplacementCallback;"
    )
    native.write_text(rebound)
    rebound_report, rebound_snapshot, rebound_wall_ms = synchronize(binary, corpus)
    rebound_result = evaluate_play_done(
        rebound_snapshot,
        AUDIO_PLAYER,
        AUDIO_NATIVE,
        expected_target="ReplacementCallback",
    )

    native.write_text(original)
    restore_report, restore_snapshot, restore_wall_ms = synchronize(binary, corpus)
    evaluate_play_done(restore_snapshot, AUDIO_PLAYER, AUDIO_NATIVE)
    return {
        "checks": {
            "removedBindingCleansDispatch": True,
            "reboundBindingChangesTarget": True,
            "restoredBindingRestoresTarget": True,
            "syncUsesSamePersistentIndex": True,
        },
        "cleanup": {
            "wallMs": round(cleanup_wall_ms, 3),
            "engineDurationMs": cleanup_report["duration_ms"],
            "dispatchEdges": 0,
        },
        "rebind": {
            "wallMs": round(rebound_wall_ms, 3),
            "engineDurationMs": rebound_report["duration_ms"],
            "target": rebound_result["edge"]["target"],
        },
        "restore": {
            "wallMs": round(restore_wall_ms, 3),
            "engineDurationMs": restore_report["duration_ms"],
            "target": "Callback",
        },
    }


def corpus_metrics(
    initialized: dict[str, Any], snapshot: dict[str, Any], wall_ms: float
) -> dict[str, Any]:
    return {
        "files": len(snapshot["files"]),
        "symbols": len(snapshot["symbols"]),
        "relationships": len(snapshot["relationships"]),
        "freshIndexWallMs": round(wall_ms, 3),
        "engineDurationMs": initialized["duration_ms"],
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--structurely", required=True, type=Path)
    parser.add_argument("--openharmony-repository", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    binary = args.structurely.resolve()
    repository = args.openharmony_repository.resolve()
    if not binary.is_file():
        raise SystemExit(f"Structurely binary does not exist: {binary}")
    commit = openharmony_commit()
    selected = (*AUDIO_FILES, *AVCODEC_FILES, *SAMPLERATE_FILES)
    verify_repository(repository, commit, selected)

    with tempfile.TemporaryDirectory(prefix="structurely-c-fnptr-") as temporary:
        base = Path(temporary)

        audio_corpus = base / "audio-to-video-sync"
        copy_files(repository, audio_corpus, AUDIO_FILES)
        audio_init, audio_snapshot, audio_wall_ms = initialize(binary, audio_corpus)
        audio = evaluate_play_done(audio_snapshot, AUDIO_PLAYER, AUDIO_NATIVE)
        audio.update(corpus_metrics(audio_init, audio_snapshot, audio_wall_ms))
        audio["incremental"] = evaluate_incremental_rebinding(binary, audio_corpus)

        avcodec_corpus = base / "avcodec"
        copy_files(repository, avcodec_corpus, AVCODEC_FILES)
        avcodec_init, avcodec_snapshot, avcodec_wall_ms = initialize(
            binary, avcodec_corpus
        )
        avcodec = evaluate_play_done(
            avcodec_snapshot, AVCODEC_PLAYER, AVCODEC_NATIVE
        )
        avcodec.update(
            corpus_metrics(avcodec_init, avcodec_snapshot, avcodec_wall_ms)
        )

        samplerate_corpus = base / "libsamplerate"
        copy_files(repository, samplerate_corpus, SAMPLERATE_FILES)
        samplerate_init, samplerate_snapshot, samplerate_wall_ms = initialize(
            binary, samplerate_corpus
        )
        samplerate = evaluate_samplerate(samplerate_snapshot)
        samplerate.update(
            corpus_metrics(
                samplerate_init, samplerate_snapshot, samplerate_wall_ms
            )
        )

    output = {
        "passed": True,
        "checks": {
            "pinnedOpenHarmonyRepository": True,
            "exactAudioToVideoSyncReleaseDispatch": True,
            "exactAvCodecReleaseDispatch": True,
            "exactLibsamplerateProcessDispatch": True,
            "incrementalCleanupRebindingAndRestore": True,
            "ownedProvenanceAndEvidenceSites": True,
            "noLeakageOrDuplicateEvidence": True,
        },
        "structurelyBinarySha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "repository": {
            "name": "openharmony",
            "commit": commit,
            "selectedFiles": len(selected),
        },
        "corpora": {
            "audioToVideoSync": audio,
            "avCodec": avcodec,
            "libsamplerate": samplerate,
        },
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps(output, indent=2))


if __name__ == "__main__":
    main()
