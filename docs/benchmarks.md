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
ranks `src/atomic_file.rs` first for “atomic file publication,” matches or
beats Perseus rank-one recall, and exceeds its top-ten recall. This is a
regression gate against a fixed baseline, not a fresh hosted-service latency
comparison.
