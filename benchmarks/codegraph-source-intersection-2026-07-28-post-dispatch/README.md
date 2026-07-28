# CodeGraph source intersection after dynamic-dispatch expansion — 2026-07-28

This five-trial rerun uses the same 441-file corpus and pinned CodeGraph 1.5.0
commit `572d22bfbe82602080e457bec655f72e3314f9ef` after Structurely added React
runtime-flow and bounded interface-implementation dispatch.

- Structurely source: commit `c637f3d`
- Query: `CodeGraph`, measured in 20 separate processes
- Acceptance thresholds: at least 2× faster indexing and query p50

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 1,734.1 ms | 6,682.3 ms | 3.85× faster |
| Query process p50 | 33,532 µs | 304,612.5 µs | 9.08× faster |
| Query process p95 | 41,928 µs | 322,962 µs | 7.70× faster |
| Init max RSS | 106,524 KiB | 1,018,216 KiB | 89.5% lower |
| Database bytes | 36,990,976 | 44,453,888 | 16.8% smaller |

Both enforced speed thresholds passed. A pinned Django acceptance rerun also
preserved 2,972 indexed files, 262,090 relationships, and both route assertions
at 49,246 ms, showing that the interface pass stays cheap when inapplicable.
