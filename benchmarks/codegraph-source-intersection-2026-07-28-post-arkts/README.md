# CodeGraph source intersection after ArkTS — 2026-07-28

This enforced five-trial run uses the same 441-file source intersection and
pinned CodeGraph 1.5.0 commit `572d22bfbe82602080e457bec655f72e3314f9ef`
after Structurely added ArkTS parsing, ArkUI semantics, and invalid-UTF-8
resilience.

- Structurely source: commit `8563d4b`
- Query: `CodeGraph`, measured in 20 separate processes
- Acceptance thresholds: at least 2× faster indexing and query p50

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 1,801.5 ms | 6,684.1 ms | 3.71× faster |
| Query process p50 | 33,388.5 µs | 306,094.0 µs | 9.17× faster |
| Query process p95 | 45,634 µs | 326,966 µs | 7.16× faster |
| Init max RSS | 101,316 KiB | 1,010,040 KiB | 90.0% lower |
| Database bytes | 36,937,728 | 44,453,888 | 16.9% smaller |

Both enforced 2× speed thresholds passed. Raw samples are preserved in
`results.json`.
