# C/C++ function-pointer acceptance — 2026-07-29

This gate verifies Structurely commit
`11b7a49ea65ac8db88772c4f1088d0566ac53157` against 16 files from pinned
OpenHarmony commit `a826ab0e75fe51d028c1c5af58188e908736b53b`.

Graph model v62 adds explicit C/C++ function-pointer facts and an include-aware,
bounded resolver adapter. It recognizes direct function-pointer fields,
function-pointer typedef fields, field assignments, positional and designated
table initializers, chained typed dispatch, bounded field-to-field propagation,
and file-local bare pointer arrays with indexed designators and casts.
Resolution requires a unique include-visible layout or typedef and a real
same-file callable registration; ambiguity, unknown layouts, data typedefs,
dynamic targets, and excessive fanout fail closed.

The production gate proves:

- AudioToVideoSync `Release → Callback` at the exact invocation site;
- AVCodec `Release → Callback` at the exact invocation site;
- 14 `src_process` may-call edges across its two dispatch sites to seven exact
  libsamplerate process implementations;
- 23 total libsamplerate function-table dispatch edges;
- incremental removal, rebinding to `ReplacementCallback`, and restoration
  without stale relationships.

Every relationship carries exact source/target file provenance and a distinct
nonzero byte-site identity. Type-level table fanout is deliberately reported as
a 0.97-confidence may-call relationship rather than a path-sensitive must-call.
Receiver chains are capped at eight members, include traversal at 16 levels,
project work at 100,000 items, and target fanout at 300.

The final gate passes 239 library tests, daemon and persistent MCP process
tests, strict all-target/all-feature Clippy, formatting, and diff checks. The
release binary SHA-256 is
`890fd52214ff87e4c6184caea2699b5cae8403a950235eb30fea7734b338f40b`;
the raw result SHA-256 is
`08c96feae700cd4a929b08d13c94866f163a9d522688895044b9b6c4798ad701`.

Reproduce with:

```sh
python3 scripts/acceptance_c_fnptr.py \
  --structurely target/release/structurely \
  --openharmony-repository /path/to/pinned/openharmony \
  --output results.json
```
