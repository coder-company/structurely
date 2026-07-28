# CodeGraph source intersection after imported emitter values — 2026-07-28

This enforced run compares Structurely commit
`f521fa014b031359007c61b52af4197d402b627c` with pinned CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef` on the same 441 source files.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,185.5 ms | 8,032.5 ms | 3.675× faster |
| Query process p50 | 39,831.5 µs | 383,251.5 µs | 9.622× faster |
| Query process p95 | 139,833 µs | 712,156 µs | 5.093× faster |
| Init max RSS | 104,640 KiB | 1,033,004 KiB | 89.87% lower |
| Database bytes | 36,864,000 | 44,453,888 | 17.07% smaller |

Both enforced 2× speed thresholds passed across five fresh indexes and twenty
fresh query processes.

- Structurely binary SHA-256:
  `4806a464ee3350d03932d4f8aac4062cb3994878288e90a4413b99140b6814aa`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Raw result SHA-256:
  `0cd6ad083b8a1ebbab91699da39121c6f92dbf5047855aa7ab0a69d6cf5ded8f`
