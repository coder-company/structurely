# CodeGraph source intersection after callback delegation — 2026-07-29

This enforced run compares clean Structurely commit
`23def0279930028a9cbbe1498b9cd4c1885a099e` with pinned CodeGraph 1.5.0
commit `572d22bfbe82602080e457bec655f72e3314f9ef` on the same 441 source
files.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,612.1 ms | 8,536.3 ms | 3.268× faster |
| Query process p50 | 43,979 µs | 383,815 µs | 8.727× faster |
| Query process p95 | 122,869 µs | 539,605 µs | 4.392× faster |
| Init max RSS | 126,580 KiB | 1,014,916 KiB | 87.53% lower |
| Database bytes | 39,092,224 | 44,453,888 | 12.06% smaller |

Both enforced 2× speed thresholds passed across five fresh indexes and twenty
fresh query processes. Structurely emitted 5,177 symbols and 36,824
relationships; CodeGraph emitted 8,957 nodes and 35,801 edges. The graph
vocabularies are not cardinality-equivalent.

The exact-callsite target map is transaction-local rather than durable. This
recovered 3,801,088 bytes from the first implementation run while retaining
identical graph cardinality.

- Structurely binary SHA-256:
  `f9720aef8d3ed6818ca811beaec4c4724ee71ddd42591ae339ed1a1ecb50b1a4`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Raw result SHA-256:
  `710976deac6deefe73fd31457955e349b9a04de0bdd1d2fe142f534fd70b5977`
