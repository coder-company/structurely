# CodeGraph source intersection after component-language expansion — 2026-07-28

This five-trial run uses the same 441-file corpus and pinned CodeGraph 1.5.0
commit `572d22bfbe82602080e457bec655f72e3314f9ef` after Structurely added Vue and
Svelte component extraction and corrected external-import fanout.

- Structurely source: commit `44ebc5a`
- Query: `CodeGraph`, measured in 20 separate processes
- Acceptance thresholds: at least 2× faster indexing and query p50
- The first Structurely index overlapped other acceptance work; medians remain
  the declared statistic and include five raw samples.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 1,921.0 ms | 7,611.8 ms | 3.96× faster |
| Query process p50 | 34,280.5 µs | 314,878.5 µs | 9.19× faster |
| Query process p95 | 38,790 µs | 339,617 µs | 8.76× faster |
| Init max RSS | 105,788 KiB | 1,038,892 KiB | 89.8% lower |
| Database bytes | 36,802,560 | 44,453,888 | 17.2% smaller |

Both enforced speed thresholds passed.
