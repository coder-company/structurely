# Current Structurely vs CodeGraph 1.5.0 — 2026-07-29

This consolidated gate compares clean Structurely commit `edcb16f` with
identity-verified CodeGraph 1.5.0 commit `572d22b` after graph model v56.

## Compatibility and usefulness

Both engines pass all 22 shared live MCP scenarios and score 1.0000 context
usefulness. Structurely also passes its separately scored Harmony emitter
extension. Its complete useful context is slightly smaller: 2,459 characters
versus CodeGraph's 2,532. The exact accepted differential baseline matches.

## Performance

The performance protocol uses exactly 441 source files, five fresh indexes,
20 fresh query processes, and the query `CodeGraph`.

| Metric | Structurely | CodeGraph | Structurely result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2.556 s | 6.674 s | 2.611× faster |
| Query p50 | 32.025 ms | 303.965 ms | 9.491× faster |
| Query p95 | 39.710 ms | 317.452 ms | 7.994× faster |
| Peak RSS | 130,020 KiB | 1,040,492 KiB | 87.50% less |
| Database | 38,989,824 B | 44,453,888 B | 12.29% smaller |

Both enforced 2× speed thresholds pass.

The source corpus was explicitly cleaned of `.structurely` and `.codegraph`
indexes before the run. An earlier attempt that copied a stale graph database
was discarded because it changed the protocol to 442 files and measured
migration/reindex overhead rather than a fresh source index.

## Semantic regression

The current 441-file persisted graph has 38,522 relationships. Normalizing
stable relationship identity to qualified source/target names, kind,
provenance, evidence file/line, and confidence yields zero additions and zero
removals against the accepted v51 semantic snapshot. In particular, zero
callback relationships are lost. Public IDs changed across the callback fact
model upgrade, but their qualified semantic relationships are identical.
