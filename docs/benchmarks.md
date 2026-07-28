# Benchmark protocol

Performance claims must use pinned source revisions, fresh temporary indexes,
the same host, and at least 20 measured query iterations.

## Structurely report

```bash
structurely benchmark \
  --path /path/to/fixture \
  --query UserService \
  --iterations 20 > structurely.json
```

For a fresh-index measurement, remove only the fixture's generated
`.structurely` directory before running. The report contains the initial index
duration, no-change sync p50/p95, query p50/p95, database size, and graph
cardinality.

## CodeGraph baseline

Pin a CodeGraph release and measure the same checked-out fixture, query, host,
and iteration count. Normalize the result to:

```json
{
  "version": "1.5.0",
  "commit": "pinned-source-commit",
  "fresh_index_ms": 1000,
  "query_p50_us": 10000,
  "database_bytes": 1000000
}
```

Record the exact commands, Node/Rust versions, operating system, CPU, memory,
and whether caches were warm alongside every published result.

## Compare

```bash
python3 scripts/compare_benchmarks.py \
  --structurely structurely.json \
  --codegraph codegraph.json \
  --output comparison.json
```

The comparator reports speedup ratios and preserves the raw normalized values.
Semantic precision and recall must be reported separately; faster incorrect
edges do not satisfy Structurely's acceptance gates.

Checked-in raw comparisons live under `benchmarks/`. The
[`semantic-2026-07-27`](../benchmarks/semantic-2026-07-27/README.md) run compares
Structurely with pinned CodeGraph 1.5.0 on the four-file semantic fixture. It is
explicitly a startup smoke test; large-repository claims require a separate
pinned run.

The
[`codegraph-source-intersection-2026-07-27`](../benchmarks/codegraph-source-intersection-2026-07-27/README.md)
run is the large-repository comparison. Both engines index the same 441 files
from the pinned CodeGraph source tree across five fresh trials. Structurely's
median end-to-end indexing is 3.96× faster, query p50 is 37.66× faster, and
peak initialization RSS is 91.1% lower on that host.

The
[`codegraph-source-intersection-2026-07-28-post-semantics`](../benchmarks/codegraph-source-intersection-2026-07-28-post-semantics/README.md)
rerun covers the expanded semantic model. Structurely remains 3.64× faster to
index and 8.76× faster for query-process p50 while using 89.2% less peak
memory. Pass `--minimum-index-speedup 2 --minimum-query-speedup 2` to make the
benchmark command fail when either required advantage regresses.

## Semantic quality

Quality manifests list expected caller/callee edges by language. Evaluate a
fixture from its indexed graph:

```bash
structurely init fixtures/semantic
structurely quality \
  --path fixtures/semantic \
  --manifest fixtures/semantic/quality.json
```

The command emits aggregate and per-language precision/recall and exits
non-zero for any false positive or false negative. CI runs the checked-in
TypeScript, JavaScript, Python, and Rust fixture on every change.

## Agent-facing context usefulness

`scripts/differential_mcp.py` drives persistent MCP sessions for Structurely
and pinned CodeGraph, then scores required facts, relevant-file recall, file
precision, flow-spine coverage, line-numbered source, and output budget. The
checked-in [`differential-mcp-2026-07-28`](../benchmarks/differential-mcp-2026-07-28/README.md)
run passes all 16 compatibility scenarios and scores Structurely 1.0000 versus
CodeGraph 0.9583 on its flow fixture.
