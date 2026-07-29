# Production hardening acceptance — 2026-07-29

This gate measures clean Structurely commit `9e6968e` after three production
hardening tranches:

- bounded, stable, no-follow Project source snapshots with explicit oversized
  tombstones and bounded whole-sync retry;
- atomic graph epoch, graph model, and resolution-fingerprint publication with
  serialized concurrent writers and truthful post-commit maintenance warnings;
- capacity-one daemon event coalescing, authoritative polling fallback,
  independent capped watcher/sync backoff, and recoverable degraded state.

The comparison uses the same 441-file source intersection, five fresh indexes,
20 fresh query processes, and pinned CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`.

| Metric | Structurely | CodeGraph | Structurely result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,688.1 ms | 6,646.6 ms | 2.473× faster |
| Query process p50 | 33,543 µs | 303,921.5 µs | 9.061× faster |
| Query process p95 | 42,060 µs | 322,020 µs | 7.656× faster |
| Init max RSS | 153,460 KiB | 1,011,268 KiB | 84.83% lower |
| Database bytes | 42,426,368 | 44,453,888 | 4.56% smaller |

Both enforced 2× speed gates pass. Compared with the accepted pre-hardening
v67 run, Structurely's index p50 improves 1.98%, query p50 improves 0.47%, and
query p95 improves 20.69%. Peak RSS is 9.25% higher and database size is 0.03%
higher; the absolute memory advantage over CodeGraph remains 857,808 KiB.

An independently built pre-hardening graph-model-v68 baseline at commit
`ab92dc7` and the hardened snapshot are byte-identical. Both contain 441 files,
5,213 symbols, and 38,538 relationships. Their shared SHA-256 is
`ca6bc9d28ab05367a6e14509aecb4b7f4ac1a08bcddcec0b5aec70c697a806e9`.
The hardening therefore changes acquisition, publication, and lifecycle
behavior without changing graph semantics on the pinned corpus.

Reproduce the performance gate:

```bash
cargo build --release
python3 scripts/benchmark_codegraph.py \
  --structurely target/release/structurely \
  --codegraph /path/to/codegraph/dist/bin/codegraph.js \
  --corpus /path/to/clean-441-file-intersection \
  --output /tmp/structurely-hardening-results \
  --query CodeGraph \
  --trials 5 \
  --queries 20 \
  --minimum-index-speedup 2 \
  --minimum-query-speedup 2
```
