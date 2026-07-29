# CodeGraph source intersection after callback arguments — 2026-07-29

This enforced run compares clean Structurely commit
`85bd25bd30958eac560027ecfbbb02823b0ae723` with pinned CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef` on the same 441 source files.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,263.2 ms | 7,360.4 ms | 3.252× faster |
| Query process p50 | 33,869 µs | 315,639 µs | 9.319× faster |
| Query process p95 | 43,429 µs | 358,924 µs | 8.265× faster |
| Init max RSS | 132,944 KiB | 1,032,296 KiB | 87.12% lower |
| Database bytes | 38,506,496 | 44,453,888 | 13.38% smaller |

Both enforced 2× speed thresholds passed across five fresh indexes and twenty
fresh query processes. Structurely emitted 5,177 symbols and 36,822
relationships; CodeGraph emitted 8,957 nodes and 35,801 edges. The graph
vocabularies are not cardinality-equivalent.

- Structurely binary SHA-256:
  `f0f7cddcab3e22d115af6669ad56454f7fd18cdfed98287e593bf5dda0d3ae78`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Raw result SHA-256:
  `0d5cf6b6eb334522fb621de099bd707f76096d608912e195bb206e60809ff48f`
