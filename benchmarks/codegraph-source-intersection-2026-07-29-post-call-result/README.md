# CodeGraph source intersection after call-result resolution — 2026-07-29

This enforced run compares clean Structurely commit
`8bce9ac6cb2042953bc477e9c57efdbe05bccad3` with pinned CodeGraph 1.5.0
commit `572d22bfbe82602080e457bec655f72e3314f9ef` on the same 441 source
files.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 3,020.4 ms | 8,117.0 ms | 2.687× faster |
| Query process p50 | 34,393 µs | 317,298 µs | 9.226× faster |
| Query process p95 | 44,834 µs | 378,669 µs | 8.446× faster |
| Init max RSS | 142,244 KiB | 1,029,732 KiB | 86.19% lower |
| Database bytes | 39,268,352 | 44,453,888 | 11.66% smaller |

Both enforced 2× thresholds passed across five fresh indexes and twenty fresh
query processes. Structurely reported 5,177 source symbols and 35,235 resolved
relationships; CodeGraph reported 8,957 nodes and 35,801 edges. The graph
vocabularies are not cardinality-equivalent.

A clean snapshot comparison against pre-layer commit `cc66d76` found 1,637
removed and 9 added relationships. All changes were ordinary name-resolution
calls. Of the removed observations, 1,564 (95.54%) were confidence 0.75 or
lower speculative fallback candidates. The remaining sampled removals were
same-file/import candidates displaced by a verified imported nominal receiver;
the nine additions were exact imported-package targets such as
`CodeGraph.isInitialized` and `DatabaseConnection.open`. The differential
usefulness gate remained perfect.

- Structurely binary SHA-256:
  `b2b0ade8ebbfa907fa25c7b3ae6622c2e6c79606c9f10986f4c0ccdd1cddb91b`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Raw result SHA-256:
  `dc1c3122575a03191758c17807e1c45e4b3a3d3deabf2a4fdc3fa90036867feb`
