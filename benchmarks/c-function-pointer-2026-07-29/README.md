# C/C++ function-pointer acceptance — 2026-07-29

This gate verifies Structurely commit
`63ba2930aa9e0c14ea1b4d01dbd41e4fd5312cfd` against 16 files from pinned
OpenHarmony commit `a826ab0e75fe51d028c1c5af58188e908736b53b`.

Graph model v60 adds explicit C/C++ function-pointer facts and an include-aware,
bounded resolver adapter. It recognizes direct function-pointer fields,
function-pointer typedef fields, field assignments, positional and designated
table initializers, and chained typed dispatch. Resolution requires a unique
include-visible layout and a real same-file callable registration; ambiguity,
unknown layouts, dynamic targets, and excessive fanout fail closed.

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

The final gate passes 236 library tests, daemon and persistent MCP process
tests, strict all-target/all-feature Clippy, formatting, and diff checks. The
release binary SHA-256 is
`43302a11e9fced4bb092c2ee351c7b6a7f06c98a7397a351620fbb6361712853`;
the raw result SHA-256 is
`f005bab0f508346bbe321228728149fb6cb2b5411c2628274ab633ce6bdbee77`.

Reproduce with:

```sh
python3 scripts/acceptance_c_fnptr.py \
  --structurely target/release/structurely \
  --openharmony-repository /path/to/pinned/openharmony \
  --output results.json
```
