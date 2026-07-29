# CodeGraph source intersection after ArkUI Builder recovery — 2026-07-29

This enforced run compares clean Structurely commit
`2fb00eadd928dad0d0e65f2a19e4409c83f7d270` with pinned CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef` on the same 441 source files.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,615.4 ms | 10,074.3 ms | 3.852× faster |
| Query process p50 | 36,195 µs | 341,510 µs | 9.435× faster |
| Query process p95 | 48,148 µs | 472,199 µs | 9.807× faster |
| Init max RSS | 114,700 KiB | 1,013,764 KiB | 88.69% lower |
| Database bytes | 36,851,712 | 44,453,888 | 17.10% smaller |

Both enforced 2× speed thresholds passed across five fresh indexes and twenty
fresh query processes.

- Structurely binary SHA-256:
  `3d0194aeb994222fb1e5363b2536a82dc03c7d87b12787f7e970afddf3bdcd08`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Raw result SHA-256:
  `421101015d938ee66492db3d2d4c3229a0a3dac7d81fc2dabf2f53b1ad68faf3`
