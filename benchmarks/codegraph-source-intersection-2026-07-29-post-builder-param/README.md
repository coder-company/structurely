# CodeGraph source intersection after BuilderParam flow — 2026-07-29

This enforced run compares clean Structurely commit
`ca4d8227289a6b9fd5a9c95cca03e90d3efc4ca8` with pinned CodeGraph 1.5.0
commit `572d22bfbe82602080e457bec655f72e3314f9ef` on the same 441 source
files.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,264.7 ms | 7,413.2 ms | 3.273× faster |
| Query process p50 | 33,583 µs | 314,258.5 µs | 9.358× faster |
| Query process p95 | 48,467 µs | 330,795 µs | 6.825× faster |
| Init max RSS | 113,936 KiB | 1,022,836 KiB | 88.86% lower |
| Database bytes | 38,477,824 | 44,453,888 | 13.44% smaller |

Both enforced 2× speed thresholds passed across five fresh indexes and twenty
fresh query processes. Structurely emitted 5,177 symbols and 36,822
relationships; CodeGraph emitted 8,957 nodes and 35,801 edges. The graph
vocabularies are not cardinality-equivalent.

- Structurely binary SHA-256:
  `d1b54398e9cd64e453892a44d4b0c4698441fc21946da7f91d4bef270a7bcc3c`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Raw result SHA-256:
  `d8bfab3a6cbbbc3c7fd7df06d2571424d5e60ee18f1ac31cc57e1c653d76b052`
