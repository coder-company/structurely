# CodeGraph source intersection after hardened ArkUI routing — 2026-07-28

This enforced run uses the same 441-file source intersection, Structurely
commit `c7af33f`, and pinned CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,116.5 ms | 7,745.0 ms | 3.66× faster |
| Query process p50 | 33,754 µs | 322,364 µs | 9.55× faster |
| Query process p95 | 45,291 µs | 377,575 µs | 8.34× faster |
| Init max RSS | 120,416 KiB | 1,039,212 KiB | 88.4% lower |
| Database bytes | 36,904,960 | 44,453,888 | 17.0% smaller |

Both enforced 2× thresholds passed. The raw five index samples and twenty
query-process samples are preserved in `results.json`.

- Structurely binary SHA-256:
  `0b6d800aa7c59b60b3c9adfa91006be9932d4b3e57435ddc3d7cad984bf70a04`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Results SHA-256:
  `fe7a333658db075869f41dce559754706eff325a7058f10270e11a44df4b9149`
