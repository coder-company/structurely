# CodeGraph source intersection after inline BuilderParam adapters — 2026-07-29

This enforced run compares clean Structurely commit
`6379b44ea5083acd95215f7c8c3aeecf0965ed5a` with pinned CodeGraph 1.5.0
commit `572d22bfbe82602080e457bec655f72e3314f9ef` on the same 441 source
files.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,323.4 ms | 7,896.7 ms | 3.399× faster |
| Query process p50 | 33,988 µs | 321,520 µs | 9.460× faster |
| Query process p95 | 45,678 µs | 378,121 µs | 8.278× faster |
| Init max RSS | 110,052 KiB | 1,024,372 KiB | 89.26% lower |
| Database bytes | 38,514,688 | 44,453,888 | 13.36% smaller |

Both enforced 2× speed thresholds passed across five fresh indexes and twenty
fresh query processes. Structurely emitted 5,177 symbols and 36,822
relationships; CodeGraph emitted 8,957 nodes and 35,801 edges. The graph
vocabularies are not cardinality-equivalent.

- Structurely binary SHA-256:
  `6a3a3acad94fab28cfa13e4b47e2548454f3093c44098f317d62482369c55b32`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Raw result SHA-256:
  `221e335c64c0df9c0918f89a70d84c325f0479d85366f6c280a1ba90835be927`
