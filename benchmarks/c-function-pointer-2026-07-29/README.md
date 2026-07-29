# C/C++ function-pointer acceptance — 2026-07-29

This gate verifies Structurely commit
`a754a2572370d37445d030006a95aa256dc320e4` against 19 files from pinned
OpenHarmony commit `a826ab0e75fe51d028c1c5af58188e908736b53b`.

Graph model v64 adds explicit C/C++ function-pointer facts and an include-aware,
bounded resolver adapter. It recognizes direct function-pointer fields,
function-pointer typedef fields, field assignments, positional and designated
table initializers, chained typed dispatch, bounded field-to-field propagation,
file-local bare pointer arrays with indexed designators and casts, and exact
call-argument → formal → stored-field callback flow. C++ local aliases resolve
from explicit address-of declarations and assignments using source-ordered,
lexically scoped reaching definitions. Sequential kills and rebinds, nested
shadowing, declarations in C++17 `if` initializers, conditional may-call unions,
and calls before a declaration are covered by adversarial tests.
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

The final gate passes 242 library tests, daemon and persistent MCP process
tests, strict all-target/all-feature Clippy, formatting, and diff checks. The
release binary SHA-256 is
`85ceb0a74300cd49c7eccc0a75c53473ec878fb7d9af4c7df0090f97f0e2ce82`;
the raw result SHA-256 is
`10971571fd50c6f65d7eab1a3752762c1aeb3ebf2059cfcdbdad5cd5ef24f55c`.

Reproduce with:

```sh
python3 scripts/acceptance_c_fnptr.py \
  --structurely target/release/structurely \
  --openharmony-repository /path/to/pinned/openharmony \
  --output results.json
```
