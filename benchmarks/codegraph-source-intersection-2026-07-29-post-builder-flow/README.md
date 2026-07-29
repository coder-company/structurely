# CodeGraph source intersection after project Builder flow — 2026-07-29

This enforced run compares clean Structurely commit
`16c3a0d69ebac8914f551a6474466aa99ce4d897` with pinned CodeGraph 1.5.0
commit `572d22bfbe82602080e457bec655f72e3314f9ef` on the same 441 source
files.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,471.4 ms | 7,611.8 ms | 3.080× faster |
| Query process p50 | 33,981 µs | 319,803 µs | 9.411× faster |
| Query process p95 | 49,067 µs | 380,530 µs | 7.755× faster |
| Init max RSS | 118,592 KiB | 1,042,104 KiB | 88.62% lower |
| Database bytes | 38,465,536 | 44,453,888 | 13.47% smaller |

Both enforced 2× speed thresholds passed across five fresh indexes and twenty
fresh query processes. Structurely emitted 5,177 symbols and 36,822
relationships; CodeGraph emitted 8,957 nodes and 35,801 edges. The graph
vocabularies are not cardinality-equivalent.

- Structurely binary SHA-256:
  `5f9c43c5c9472526a406a164bd8870af867f05e80d0d24de004f10ed46db2270`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Raw result SHA-256:
  `e458774151f0b7c8913c26302ee3d52570c9f9b42759a913b4f465c70ad081c9`
