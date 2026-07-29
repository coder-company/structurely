# CodeGraph source intersection after inline callbacks — 2026-07-29

This enforced run compares clean Structurely code commit
`2ce5c888e82d522c452245f453f2aa15013a4162` with pinned CodeGraph 1.5.0
commit `572d22bfbe82602080e457bec655f72e3314f9ef` on the same 441 source
files.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,737.0 ms | 8,329.3 ms | 3.043× faster |
| Query process p50 | 36,220 µs | 331,174 µs | 9.143× faster |
| Query process p95 | 49,772 µs | 429,057 µs | 8.620× faster |
| Init max RSS | 121,380 KiB | 1,004,620 KiB | 87.92% lower |
| Database bytes | 40,009,728 | 44,453,888 | 10.00% smaller |

Both enforced 2× thresholds passed across five fresh indexes and twenty fresh
query processes. Structurely reported 5,177 source symbols and 36,881 resolved
relationships; CodeGraph reported 8,957 nodes and 35,801 edges. The graph
vocabularies are not cardinality-equivalent.

The first correct inline implementation reran global call resolution at each
accepted nesting depth and failed the index threshold at 1.592×. The accepted
Implementation retains one global pass, stores exact targets transaction-locally,
and publishes provisional-owned calls through a targeted deferred seam. This
restored indexing by 51.79% while preserving the graph.

- Structurely binary SHA-256:
  `a15b3fc4222fa16bb1aa9f6113e1b61f9525474e4713fb8b50780ae6a9262f4a`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Raw result SHA-256:
  `da56adf9974b02ae0b7ca20a1c7d934c3e8deb3be73db38db00e1d8623b20530`
