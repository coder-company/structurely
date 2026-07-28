# CodeGraph source intersection after semantic expansion — 2026-07-28

This rerun uses the same 441-file intersection and pinned CodeGraph 1.5.0
commit `572d22bfbe82602080e457bec655f72e3314f9ef` after Structurely added function
references, framework adapters, npm/pnpm/Cargo/Go workspace resolution, and
agent-facing freshness behavior.

- Structurely source: commit `d31788f` (the timed source is identical; that
  commit also checks in the representative-repository evidence)
- Protocol: five fresh indexes per engine and 20 separate query processes
- Query: `CodeGraph`
- Same host and runtime setup as the adjacent 2026-07-28 comparison

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 1,849.8 ms | 6,727.2 ms | 3.64× faster |
| Query process p50 | 34,743 µs | 304,299.5 µs | 8.76× faster |
| Query process p95 | 41,163 µs | 328,625 µs | 7.98× faster |
| Init max RSS | 109,548 KiB | 1,014,628 KiB | 89.2% lower |
| Database bytes | 44,781,568 | 44,453,888 | 0.7% larger |

An initial run exposed a function-reference resolution regression: 3,806
references each issued a correlated candidate query, raising resolution time
to 6,741 ms and losing the index-speed gate. Building deterministic candidate
indexes once per graph epoch reduced resolution to 665 ms in the diagnostic
run. The full rerun above preserves the expanded 52,570 relationships and
passes both 2× speed gates.

Reproduce with explicit acceptance thresholds:

```bash
python3 scripts/benchmark_codegraph.py \
  --structurely target/release/structurely \
  --codegraph /path/to/codegraph/dist/bin/codegraph.js \
  --corpus /path/to/441-file-intersection \
  --output /tmp/structurely-codegraph-results \
  --minimum-index-speedup 2 \
  --minimum-query-speedup 2
```
