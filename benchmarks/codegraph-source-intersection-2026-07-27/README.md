# CodeGraph source intersection — 2026-07-27

This comparison uses the exact 441-file source intersection indexed by both
engines from CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`. Three generated parser files over
the shared 1 MiB safety limit and languages outside Structurely's current set
are absent from the copied intersection corpus.

- Structurely commit: `a5d3228cfacbdc48b7da9f6da3b91a6297b22366`
- Host: Linux 6.17 x86-64, 8 vCPU AMD EPYC 7R13, 30 GiB RAM
- Runtimes: Node 22.22.3, Rust 1.92.0
- Index trials: five fresh generated directories per engine; warm filesystem
  cache; median reported
- Query: `codegraph_explore`, 20 separate CLI processes; p50/p95 wall time
- Memory: `/usr/bin/time` maximum resident set on a separate fresh trial

| Metric | Structurely | CodeGraph | Improvement |
|---|---:|---:|---:|
| Indexed files | 441 | 441 | identical corpus |
| Fresh index wall p50 | 1,710.9 ms | 6,773.6 ms | 3.96× faster |
| Reported engine p50 | 1,678 ms | 3,200 ms | 1.91× faster |
| Query process p50 | 5,998 µs | 225,892 µs | 37.66× faster |
| Query process p95 | 6,582 µs | 240,773 µs | 36.58× faster |
| Index bytes | 38,596,608 | 44,454,117 | 13.2% smaller |
| Init max RSS | 89,400 KiB | 1,004,016 KiB | 91.1% lower |

Structurely emits more relationships because it retains ambiguous name-based
candidates with low confidence and explicit evidence; raw edge count is not a
quality claim. Correctness remains evaluated by the semantic manifests, where
precision and recall are measured against expected edges.

The CodeGraph CLI reports engine duration rounded to 0.1 seconds; wall medians
come from `time.perf_counter_ns()` around each complete successful process.
Raw samples and pinned revisions are preserved in the adjacent JSON files.

## Worker scaling

Commit `82ab5a7aba9697952df3ef091dbb7df42f0f29b8` was also measured with
`STRUCTURELY_PARSE_WORKERS=1,2,4,8` on the same corpus:

| Workers | Wall time | Max RSS | Staging |
|---:|---:|---:|---:|
| 1 | 3.41 s | 59,108 KiB | 2,630 ms |
| 2 | 1.94 s | 78,280 KiB | 1,149 ms |
| 4 | 1.66 s | 78,700 KiB | 834 ms |
| 8 | 1.61 s | 88,808 KiB | 808 ms |

From one to eight workers, wall time improves 2.12× while peak RSS grows only
1.50×. Memory growth is therefore sublinear in worker count on the pinned
large-repository fixture. The raw report is in `worker-scaling.json`.
