# C/C++ function-pointer acceptance — 2026-07-29

This gate verifies Structurely commit
`f373aba190270a9f661cd9d7455cf61a36a443ec` against 19 files from pinned
OpenHarmony commit `a826ab0e75fe51d028c1c5af58188e908736b53b`.

Graph model v62 adds explicit C/C++ function-pointer facts and an include-aware,
bounded resolver adapter. It recognizes direct function-pointer fields,
function-pointer typedef fields, field assignments, positional and designated
table initializers, chained typed dispatch, bounded field-to-field propagation,
file-local bare pointer arrays with indexed designators and casts, and exact
call-argument → formal → stored-field callback flow.
Resolution requires a unique include-visible layout or typedef and a real
same-file callable registration; ambiguity, unknown layouts, data typedefs,
dynamic targets, and excessive fanout fail closed.

The production gate proves:

- AudioToVideoSync `Release → Callback` at the exact invocation site;
- AVCodec `Release → Callback` at the exact invocation site;
- 14 `src_process` may-call edges across its two dispatch sites to seven exact
  libsamplerate process implementations;
- 23 total libsamplerate function-table dispatch edges;
- four `src_callback_read` edges to the exact callbacks supplied through
  `src_callback_new`, for 27 total libsamplerate dispatch edges;
- incremental removal, rebinding to `ReplacementCallback`, and restoration
  without stale relationships.

Every relationship carries exact source/target file provenance and a distinct
nonzero byte-site identity. Type-level table fanout is deliberately reported as
a 0.97-confidence may-call relationship rather than a path-sensitive must-call.
Receiver chains are capped at eight members, include traversal at 16 levels,
project work at 100,000 items, and target fanout at 300.

The final gate passes 240 library tests, daemon and persistent MCP process
tests, strict all-target/all-feature Clippy, formatting, and diff checks. The
release binary SHA-256 is
`7762661ac833bc132a58201522b0256bb5624436c6ed0052fa11982db7884e94`;
the raw result SHA-256 is
`ec6382d0c81a95dc64831bc68c2e9c49197a403d79a8c9f8b4218b6353228619`.

Reproduce with:

```sh
python3 scripts/acceptance_c_fnptr.py \
  --structurely target/release/structurely \
  --openharmony-repository /path/to/pinned/openharmony \
  --output results.json
```
