# Verify a release

Structurely uses executable gates for correctness, compatibility, semantic
quality, reliability, and performance. A release is ready only when every
required gate passes from a clean commit.

## Run the required checks

```bash
cargo fmt --check
cargo test --all-targets --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --locked
python3 -m unittest \
  scripts/test_compare_benchmarks.py \
  scripts/test_differential_mcp.py \
  scripts/test_benchmark_perseus_acceptance.py
python3 scripts/check_docs.py
```

CI runs the cross-platform Rust checks on Linux, macOS, and Windows. Linux CI
also verifies the semantic fixture and requires a one-file incremental update
to become visible within 300 milliseconds.

## Verify semantic quality

Use the checked-in manifest:

```bash
cargo run --locked -- init fixtures/semantic
cargo run --locked -- quality \
  --path fixtures/semantic \
  --manifest fixtures/semantic/quality.json
```

The fixture requires exact relationships for JavaScript, Python, Rust, and
TypeScript. The command exits unsuccessfully when expected and predicted edges
do not match.

## Compare the MCP behavior

Build Structurely and CodeGraph 1.5.0 before running the differential gate:

```bash
cargo build --release --locked
python3 scripts/differential_mcp.py \
  --structurely target/release/structurely \
  --codegraph /path/to/codegraph/dist/bin/codegraph.js \
  --fixture fixtures/differential \
  --output /tmp/structurely-differential.json
```

The gate checks discovery, search, callers, callees, impact, bounded node and
explore output, status, files, malformed requests, and missing symbols. It also
scores whether each implementation gives an agent the required files, facts,
and flow context.

## Compare performance

Use one clean source intersection for both implementations:

```bash
python3 scripts/benchmark_codegraph.py \
  --structurely target/release/structurely \
  --codegraph /path/to/codegraph/dist/bin/codegraph.js \
  --corpus /path/to/clean-source-intersection \
  --output /tmp/structurely-performance \
  --query CodeGraph \
  --trials 5 \
  --queries 20 \
  --minimum-index-speedup 2 \
  --minimum-query-speedup 2
```

The runner creates isolated copies, performs five fresh indexes, launches 20
fresh query processes, and fails when either median speedup is below 2×.

## Review the accepted launch result

The [launch acceptance report](../benchmarks/release-hardening-2026-07-29/README.md)
records the pinned commits, binary hash, corpus hash, raw sample values, graph
snapshot identity, and artifact hashes.

At accepted commit `bc708fcf1e40ee18ced7ee6ef92f4e687e3c7add`:

- all 25 shared compatibility predicates pass;
- both implementations score 1.0 for context usefulness;
- semantic precision and recall are 1.0;
- the hardened graph snapshot remains byte-identical;
- Structurely indexes 2.375× faster and queries 10.537× faster at p50;
- Structurely uses 84.60% less peak memory.

Historical benchmark snapshots remain available in Git history. The repository
keeps only the consolidated launch result so new readers do not mistake an
intermediate run for the current product contract.

## Verify the Perseus advantage

Build the candidate and run the checked-in acceptance gate:

```bash
cargo build --release --locked
python3 scripts/benchmark_perseus_acceptance.py \
  --structurely target/release/structurely \
  --project . \
  --baseline benchmarks/perseus-2026-07-29/results.json \
  --output /tmp/structurely-perseus-acceptance.json
```

The command synchronizes the project, runs five fixed research queries, and
exits unsuccessfully unless all gates pass:

- every required workflow is present: research, session history, recaps,
  impact analysis, path tracing, memory, and local team workspaces;
- repository content coverage meets the pinned Perseus file count and produces
  retrievable chunks;
- `src/atomic_file.rs` ranks first for “atomic file publication”;
- rank-one recall is no worse than Perseus and top-ten recall is better.

Cloud synchronization is intentionally outside Structurely's local-first scope.
