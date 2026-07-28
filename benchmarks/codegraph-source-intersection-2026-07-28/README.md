# CodeGraph source intersection — 2026-07-28

This rerun compares Structurely 0.1.1 with CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`. Both engines received independent
copies of the same 441 source files from that pinned CodeGraph tree. Files in
languages not supported by both engines and generated sources over 1 MiB were
excluded before either engine ran.

- Structurely commit: `41d1d81a6cc67f9919fc6e98e289ef4f5bd8eb66`
- Host: Linux 6.17 x86-64, 8 vCPU AMD EPYC 7R13, 30 GiB RAM
- Runtimes: Node 22.22.3, Rust 1.92.0
- Index: five fresh copies per engine, warm filesystem cache, median reported
- Query: 20 separate `explore CodeGraph` CLI processes per engine
- Memory: `/usr/bin/time` maximum RSS on a separate fresh initialization

| Metric | Structurely | CodeGraph | Improvement |
|---|---:|---:|---:|
| Indexed files | 441 | 441 | identical corpus |
| Fresh index wall p50 | 1,726.4 ms | 7,087.6 ms | 4.11× faster |
| Reported engine p50 | 1,694 ms | 3,500 ms | 2.07× faster |
| Query process p50 | 40,474.5 µs | 319,685.5 µs | 7.90× faster |
| Query process p95 | 63,194 µs | 361,642 µs | 5.72× faster |
| Index bytes | 38,649,856 | 44,453,888 | 13.1% smaller |
| Init max RSS | 90,820 KiB | 1,028,492 KiB | 91.2% lower |

The five raw wall samples include slower fifth trials for both engines; medians
are used rather than selectively dropping those observations. Query timings
cover complete process startup, index synchronization, query execution, and
rendering with output redirected.

Graph cardinalities are not directly comparable semantic-quality scores:
Structurely stores confidence- and evidence-bearing candidate relationships,
while CodeGraph has its own node and edge model. Structurely's separate
four-language semantic manifest passed this run with precision 1.0 and recall
1.0.

Run the checked-in harness with:

```bash
scripts/benchmark_codegraph.py \
  --structurely target/release/structurely \
  --codegraph /path/to/codegraph/dist/bin/codegraph.js \
  --corpus /path/to/441-file-intersection \
  --output /tmp/structurely-codegraph-results
```

The complete timing samples and graph cardinalities are in `results.json`.
