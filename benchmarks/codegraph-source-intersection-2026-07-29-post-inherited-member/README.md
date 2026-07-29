# CodeGraph source intersection after inherited-member resolution — 2026-07-29

This enforced run compares clean Structurely commit
`d28838f4ce6d49bb558536d50898e3f6b72ae1f0` with pinned CodeGraph 1.5.0
commit `572d22bfbe82602080e457bec655f72e3314f9ef` on exactly 441 source
files.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,915.6 ms | 7,778.0 ms | 2.668× faster |
| Query process p50 | 33,022 µs | 317,904 µs | 9.627× faster |
| Query process p95 | 45,257 µs | 354,738 µs | 7.839× faster |
| Init max RSS | 130,360 KiB | 1,018,656 KiB | 87.20% lower |
| Database bytes | 38,780,928 | 44,453,888 | 12.76% smaller |

Both enforced 2× thresholds passed across five fresh indexes and twenty fresh
query processes. Structurely reported 5,177 source symbols and 34,391 resolved
relationships; CodeGraph reported 8,957 nodes and 35,801 edges. These graph
vocabularies are not cardinality-equivalent.

The clean semantic snapshot audit against accepted commit `8bce9ac` records
1,018 removed and 174 added semantic identities, net -844. Of the removals,
987 are confidence-0.35 speculative fallbacks, eight are confidence 0.75, and
23 are confidence-0.99 same-file edges. There are zero confidence-0.995
semantic removals. All 23 remaining high-confidence removals are proven
incorrect baseline targets: 17 are replaced by exact receiver targets at
0.995, while six incorrectly targeted an unrelated same-file/free function.

All previously flagged `FileWatcher` and `MCPSession.stop` relationships are
restored. The audit finds zero removed callback identities and twelve callback
additions. Inherited-member prevalence is intentionally measured on the
OpenHarmony corpus rather than this intersection.

- Structurely binary SHA-256:
  `3dde38c72a32491f6422722ab1968df86b88562d03835271f4190834e57b0f5c`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Raw performance SHA-256:
  `001eb34aafc7c9b72940881e987de7abef92b5558063e3b6eb68ca82d2958cce`
- Snapshot audit SHA-256:
  `45424acf348eaceba1371c96e7cc09bc31b68393c682b11d323b825f77c25757`
