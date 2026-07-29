# CodeGraph source intersection after C macro tables — 2026-07-29

This enforced run compares clean Structurely commit
`d46f09b26e72473a597aa63612c45b21f0e503e0` (graph model v67) with pinned
CodeGraph 1.5.0 commit `572d22bfbe82602080e457bec655f72e3314f9ef`
on exactly 441 source files.

| Metric | Structurely | CodeGraph | Result |
|---|---:|---:|---:|
| Fresh index wall p50 | 2,742.3 ms | 6,719.1 ms | 2.450× faster |
| Query process p50 | 33,703 µs | 304,139 µs | 9.024× faster |
| Query process p95 | 53,034 µs | 391,128 µs | 7.375× faster |
| Init max RSS | 140,464 KiB | 1,035,844 KiB | 86.44% lower |
| Database bytes | 42,414,080 | 44,453,888 | 4.59% smaller |

Both enforced 2× thresholds pass across five fresh indexes and twenty fresh
query processes. Structurely reports 5,177 source symbols and 34,341 resolver
relationships; CodeGraph reports 8,957 nodes and 35,801 edges. Those graph
vocabularies are not cardinality-equivalent.

Against the consolidated v56 run, Structurely's index p50 is 7.29% slower,
query p50 is 5.24% slower, peak RSS is 8.03% higher, and database size is
8.79% higher. The v67 preprocessor fact batches are deliberately persisted
for exact incremental macro replay, including across the corpus's large
generated C parser sources. The required Rust-first performance advantages
remain above their acceptance thresholds.

An independently built clean v66 baseline at commit `e6c7c98` and the v67
snapshot both contain 441 files, 5,213 symbols, and 38,538 persisted
relationships. Removing only `graph_model_version` makes the full snapshots
byte-identical. Normalized relationship identities have zero additions and
zero removals; the 117 callback/function-pointer relationships and the entire
confidence histogram are unchanged. The macro layer therefore adds no
speculative edge to this corpus when no supported macro callback table proves
one.

- Structurely binary SHA-256:
  `8781dcf151e3496f3190618f7f308ce1c15c41c75b53ff8ec8665a7ef3e3fb0e`
- Clean v66 binary SHA-256:
  `4851295e3e76502d1f8e03b94f3de45f5a19bc8fda229390a038dfab669a5ca6`
- CodeGraph executable SHA-256:
  `03e4c791cc0dd91ed264278461bf9a56c0278aa0670d5942fc4732311c66de03`
- Raw performance SHA-256:
  `ed0c21efc3764b0e84b8cfad6a8797ca016ceda61a238ec5dc727ed5e5305041`
- Normalized semantic identity SHA-256:
  `71e50d0de12bfa4cca446d4ff2e173472cfa8b03058f7fd23e4f68a5aeea2c79`

The matching macro-specific correctness gate is
[`c-macros-2026-07-29`](../c-macros-2026-07-29/README.md).
