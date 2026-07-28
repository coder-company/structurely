# CodeGraph source intersection after Harmony package resolution — 2026-07-28

This enforced five-trial run uses the same 441-file source intersection and
pinned CodeGraph 1.5.0 commit `572d22bfbe82602080e457bec655f72e3314f9ef`
after Structurely commit `31838d0` added bounded ohpm workspace discovery.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,019.6 ms | 7,869.7 ms | 3.90× faster |
| Query process p50 | 38,311.5 µs | 375,960.0 µs | 9.81× faster |
| Query process p95 | 84,756 µs | 578,877 µs | 6.83× faster |
| Init max RSS | 103,680 KiB | 1,002,996 KiB | 89.7% lower |
| Database bytes | 36,888,576 | 44,453,888 | 17.0% smaller |

Both enforced 2× speed thresholds passed. The raw five index samples and
twenty query-process samples are preserved in `results.json`.
