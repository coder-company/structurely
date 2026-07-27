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

