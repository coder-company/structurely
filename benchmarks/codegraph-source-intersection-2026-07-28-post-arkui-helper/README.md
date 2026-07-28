# CodeGraph source intersection after ArkUI style helpers — 2026-07-28

This enforced run uses the same 441-file source intersection, Structurely
commit `2bcfc6a`, and pinned CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,071.5 ms | 9,002.1 ms | 4.35× faster |
| Query process p50 | 38,743 µs | 357,572.5 µs | 9.23× faster |
| Query process p95 | 83,676 µs | 595,138 µs | 7.11× faster |
| Init max RSS | 122,536 KiB | 1,016,636 KiB | 88.0% lower |
| Database bytes | 36,892,672 | 44,453,888 | 17.0% smaller |

Both enforced 2× thresholds passed. The raw five index samples and twenty
query-process samples are preserved in `results.json`.

- Structurely binary SHA-256:
  `dd21c2b62f49571f14391d8ca93c9ccbbdb80be2aa6c8e0a3c672ee3326f336a`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
