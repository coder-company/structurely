# Perseus comparison — 2026-07-29

This comparison measures Structurely 0.2.0 at commit `31d62da` against the
installed Perseus CLI 0.1.196 at commit `f185e0c`. Both tools indexed the same
clean committed Structurely snapshot on an 8-vCPU AMD EPYC 7R13 Linux host.

## Results

| Metric | Structurely | Perseus | Structurely result |
|---|---:|---:|---:|
| Clean index wall p50 | 0.58 s | 10.38 s | 17.90× faster |
| Warm query-process wall p50 | 12.51 ms | 1,660 ms | 132.73× faster |
| Expected file ranked first | 3/5 | 3/5 | tie |
| Expected file within top 10 | 5/5 | 4/5 | +1 query |

Structurely indexed 63 supported source files into 1,297 symbols and 4,873
relationships. Perseus reported 96 repository files and 1,411 chunks. These
counts are not equivalent: Perseus includes repository content that Structurely
does not treat as source.

## Protocol

- Three forced clean indexes per tool.
- Structurely removed `.structurely` before each `init`.
- Perseus used `index --force --no-progress --json` and waited for the hosted
  index to become queryable.
- Structurely query p50 uses 50 fresh CLI processes.
- Perseus query p50 uses five fresh, warm CLI processes against one ready hosted
  index.
- Both queried for MCP dispatch with a limit of 10.
- The five-query relevance check used manually chosen expected files and
  recorded rank-one and top-ten recall.

Perseus performs indexing and retrieval on `perseus.computer`; Structurely runs
locally. Perseus server CPU, memory, storage, and network conditions are not
observable from its CLI, so this comparison does not claim memory or database
advantages. Wall time represents the user's end-to-end CLI wait.

See [`results.json`](results.json) for versions, samples, queries, and ranks.
