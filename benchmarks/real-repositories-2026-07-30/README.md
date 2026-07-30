# Real-repository launch acceptance — 2026-07-30

This run used Structurely commit `bd2faf0`, release-binary SHA-256
`1422b9f5462003de8f3e1f0f952327a9c60f9a9329660863a4945e716c6668d3`,
Rust 1.92.0, and Linux 6.17 x86-64. Every repository was checked out at the
commit recorded in `results.json`. All semantic assertions and configured
resource ceilings passed.

| Repository | Source files | Content files / chunks | Fresh index | Peak RSS | Query p50 / p95 | One-file sync |
|---|---:|---:|---:|---:|---:|---:|
| Express | 141 | 165 / 425 | 0.754 s | 29.2 MiB | 6.04 / 13.47 ms | 173 ms |
| LightRAG | 514 | 673 / 3,548 | 9.190 s | 92.2 MiB | 8.00 / 11.97 ms | 308 ms |
| Graphiti | 255 | 326 / 1,120 | 2.310 s | 37.9 MiB | 6.43 / 7.51 ms | 148 ms |
| Vue | 537 | 630 / 2,605 | 5.611 s | 81.1 MiB | 5.01 / 5.75 ms | 238 ms |

The incremental case appends a newline to the configured source file. Every
run reported one changed file, zero deleted files, zero changed symbols, and
zero rematerialized relationships. Query timings include CLI process startup.
These measurements characterize this machine and pinned corpus; the wider
manifest ceilings are the portable regression contract.

Reproduce the run with the command in `docs/benchmarks.md`. The compact raw
metrics are retained in `results.json`; semantic assertions and exact clone
URLs live in `fixtures/real-repositories.json`.
