# Semantic fixture comparison — 2026-07-27

This is a same-host smoke comparison, not the required large-repository
benchmark. It exists to make the current evidence reproducible and to prevent
small-fixture regressions from being presented as large-scale proof.

- Fixture: `fixtures/semantic` at Structurely commit `362294d`
- CodeGraph: 1.5.0 commit `572d22bfbe82602080e457bec655f72e3314f9ef`
- Host: Linux 6.17 x86-64, 8 vCPU AMD EPYC 7R13, 30 GiB RAM
- Runtimes: Node 22.22.3, Rust 1.92.0
- Index: fresh generated directory, warm filesystem cache
- Query: `tsCaller`, 20 separate CLI processes, p50/p95 wall time
- Memory: `/usr/bin/time` maximum resident set during fresh initialization
- Database size: generated index directory for CodeGraph; SQLite database for
  Structurely

Observed results:

| Metric | Structurely | CodeGraph | Ratio |
|---|---:|---:|---:|
| Fresh index engine time | 10 ms | 357 ms | 35.7× faster |
| Query process p50 | 2,457 µs | 209,776 µs | 85.38× faster |
| Query process p95 | 3,023 µs | 222,714 µs | 73.67× faster |
| Index bytes | 114,688 | 159,973 | 28.3% smaller |
| Init max RSS | 10,324 KiB | 287,428 KiB | 96.4% lower |

Commands used:

```bash
/usr/bin/time -f 'INDEX_SECONDS=%e MAX_RSS_KB=%M' \
  structurely init "$FRESH_FIXTURE"

/usr/bin/time -f 'INDEX_SECONDS=%e MAX_RSS_KB=%M' \
  node dist/bin/codegraph.js init "$FRESH_FIXTURE"
```

Query percentiles were collected by Python `time.perf_counter_ns()` around 20
successful subprocess invocations of each release binary. The CodeGraph and
Structurely outputs were captured rather than printed during timing.

The fixture has only four files. These results establish startup/smoke behavior
and do not satisfy the large-repository acceptance gate by themselves.
