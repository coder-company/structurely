# CodeGraph source intersection after Harmony emitter resolution — 2026-07-28

This enforced run compares Structurely commit
`8c3972ed31fbf599436a85d0d4226c465df8e320` with pinned CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef` on the same 441 source files.
Five fresh-index processes and twenty fresh query processes were measured.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,116.0 ms | 7,671.2 ms | 3.63× faster |
| Query process p50 | 34,556 µs | 317,039 µs | 9.17× faster |
| Query process p95 | 42,802 µs | 359,571 µs | 8.40× faster |
| Init max RSS | 95,860 KiB | 1,008,668 KiB | 90.5% lower |
| Database bytes | 36,929,536 | 44,453,888 | 16.9% smaller |

Both enforced 2× speed thresholds passed. Structurely indexed 441 files into
5,177 symbols and 36,892 relationships; CodeGraph indexed the same 441 files
into 8,957 nodes and 35,801 edges. The models are not count-equivalent.

- Structurely binary SHA-256:
  `5ac602fabfe64889456191ce244bdeb214c18fe0bbd40efd4174ad2fd7bdfd90`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Raw result SHA-256:
  `5ca559bff86fb0c8872eebcbe61fbeeaad068ac67babd0026776d53c8b931eca`
