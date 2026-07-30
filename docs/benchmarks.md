# Reproduce benchmarks

Use benchmarks to compare clean commits, not working directories. Record the
Structurely commit, binary SHA-256, CodeGraph version and commit, corpus hash,
operating system, and complete sample arrays.

## Create a fair corpus

Give both tools the same source files. Exclude each tool's generated index,
dependency caches, build outputs, and unsupported binary files. Hash the sorted
relative paths and contents so another run can verify the corpus.

Do not compare each tool on its own default file set. Different file counts
make speed, memory, and database-size claims misleading.

## Measure fresh indexing and queries

```bash
cargo build --release --locked
python3 scripts/benchmark_codegraph.py \
  --structurely target/release/structurely \
  --codegraph /path/to/codegraph/dist/bin/codegraph.js \
  --corpus /path/to/corpus \
  --output /tmp/structurely-benchmark \
  --query <representative-query> \
  --trials 5 \
  --queries 20 \
  --minimum-index-speedup 2 \
  --minimum-query-speedup 2
```

The script reports:

- fresh-index wall-clock samples and p50;
- query-process samples, p50, and p95;
- peak resident memory;
- database size;
- indexed graph cardinality;
- enforced speed-gate results.

Use a release build. Keep the machine idle, avoid warm-index reuse, and rerun a
surprising result before publishing it.

## Compare two result files

```bash
python3 scripts/compare_benchmarks.py \
  /path/to/baseline/results.json \
  /path/to/candidate/results.json
```

The comparator fails when configured regressions exceed their budgets. Its unit
tests live in `scripts/test_compare_benchmarks.py`.

## Validate pinned real repositories

The real-repository harness checks semantic behavior and records fresh-index
time, peak RSS, total local storage, content coverage, repeated query latency,
and one-file incremental-sync work:

```bash
python3 scripts/acceptance_repositories.py \
  --structurely target/release/structurely \
  --repository express=/path/to/express \
  --repository lightrag=/path/to/LightRAG \
  --repository graphiti=/path/to/graphiti \
  --repository vue=/path/to/vue \
  --only express --only lightrag --only graphiti --only vue \
  --query-samples 5 \
  --enforce-performance-limits \
  --output /tmp/structurely-real-repositories.json
```

Use clones at the exact commits in `fixtures/real-repositories.json`. The
performance ceilings are deliberately wider than the recorded launch
measurements so ordinary machine variation does not look like a regression.
The script copies each repository before changing its configured incremental
file, so the supplied clones remain untouched.

## Measure compatibility and usefulness

Performance does not establish correctness. Run the differential MCP gate from
[Verify a release](acceptance.md) on the same binary. Preserve normal and
invalid-request predicates, context-usefulness scores, and the pinned upstream
identity.

The consolidated [launch result](../benchmarks/release-hardening-2026-07-29/README.md)
is the only benchmark artifact retained on the current branch. Git history
contains the intermediate development runs.

## Enforce the Perseus acceptance gate

The Perseus gate checks current behavior against the pinned July 29 baseline:

```bash
cargo build --release --locked
python3 scripts/benchmark_perseus_acceptance.py \
  --structurely target/release/structurely \
  --project . \
  --baseline benchmarks/perseus-2026-07-29/results.json \
  --output /tmp/structurely-perseus-acceptance.json
```

It fails unless Structurely exposes every named workflow, indexes at least as
many useful repository files as the pinned Perseus run, has chunk retrieval,
ranks `src/atomic_file.rs` first for “atomic file publication,” beats Perseus
rank-one recall, and exceeds its top-ten recall. This is a
regression gate against a fixed baseline, not a fresh hosted-service latency
comparison.
