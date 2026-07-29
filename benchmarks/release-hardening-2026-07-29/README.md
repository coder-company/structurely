# Release hardening acceptance — 2026-07-29

This gate measures clean Structurely commit
`bc708fcf1e40ee18ced7ee6ef92f4e687e3c7add` after the resource-budget,
configuration, migration, storage-recovery, and read-only query hardening
tranches. The comparison pins CodeGraph 1.5.0 at commit
`572d22bfbe82602080e457bec655f72e3314f9ef`.

## Result

| Metric | Structurely | CodeGraph | Structurely result |
|---|---:|---:|---:|
| MCP compatibility predicates | 25/25 | 25/25 | parity |
| Context-usefulness score | 1.000 | 1.000 | parity |
| Context characters | 2,459 | 2,532 | 2.88% fewer |
| Fresh index wall p50 | 2,795.9 ms | 6,640.0 ms | 2.375× faster |
| Query process p50 | 28,836 µs | 303,848 µs | 10.537× faster |
| Query process p95 | 37,382 µs | 318,819 µs | 8.529× faster |
| Init max RSS | 159,752 KiB | 1,037,072 KiB | 84.60% lower |
| Database bytes | 42,434,560 | 44,453,888 | 4.54% smaller |

Both enforced 2× performance gates pass. Compatibility includes normal
requests, missing required arguments, invalid limits, missing symbols, bounded
node windows, bounded exploration, impact analysis, and resource discovery.
Structurely's additional OpenHarmony emitter predicate also passes.

The graph snapshot is byte-identical to the accepted model-v68 baseline:
441 files, 5,213 symbols, 38,538 relationships, and SHA-256
`ca6bc9d28ab05367a6e14509aecb4b7f4ac1a08bcddcec0b5aec70c697a806e9`.
The hardening therefore preserves graph semantics on the pinned corpus.

The semantic fixture gate passes with precision and recall of 1.0 for all four
covered languages: JavaScript, Python, Rust, and TypeScript. The full Rust test
suite passed at this commit (280 library tests plus the CLI, daemon CLI, and MCP
CLI suites), as did strict all-target, all-feature Clippy.

## Protocol

- Canonical corpus: 441 files; source SHA-256
  `9ed866de29157f0a365f17101591ad66608436cd9d2b5c3edff401d701ddefc3`.
- Performance: five isolated fresh indexes and 20 fresh query processes per
  implementation.
- Differential: two independent captures; the second uses the first as its
  baseline and reports `baselineMatches: true`.
- Structurely release binary SHA-256:
  `76ecc99dd5732d7b12bc14a8e653ec0fee183fe33c3a6d4f7b1e125b449ac5b0`.
- Raw acceptance artifact hashes are recorded in `results.json`; the temporary
  raw files are not committed because the differential capture is 128 KiB and
  mostly duplicates protocol payloads.

Reproduce the performance gate:

```bash
cargo build --release
python3 scripts/benchmark_codegraph.py \
  --structurely target/release/structurely \
  --codegraph /path/to/codegraph/dist/bin/codegraph.js \
  --corpus /path/to/clean-441-file-intersection \
  --output /tmp/structurely-release-hardening-results \
  --query CodeGraph \
  --trials 5 \
  --queries 20 \
  --minimum-index-speedup 2 \
  --minimum-query-speedup 2
```
