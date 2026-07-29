# OpenHarmony call-result acceptance — 2026-07-29

Structurely commit `8bce9ac6cb2042953bc477e9c57efdbe05bccad3`
indexed exactly 6,995 ETS files from
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- Fresh index: 125.376 seconds
- Source-emitted symbols: 47,955
- Resolver-reported relationships: 84,204
- Persisted symbols/relationships: 48,246 / 125,655
- Database: 202,825,728 bytes
- Correlated assertions: 16/16
- Isolated gate peak RSS: 231,484 KiB
- Explicit nominal return facts: 239
- Immediate call-result candidate sites: 13,042 across 2,509 files
- Published call-result relationships: 72 across 46 targets

The new assertion proves a production imported-singleton flow:
`KeyItem.build` calls `InputHandler.getInstance().insertText()` at line 50.
The factory resolves through the verified import, its explicit
`InputHandler` return annotation resolves to the unique project type, and the
outer call resolves to `InputHandler.insertText` in
`model/KeyboardController.ets`. The edge has
`tree-sitter/name-resolution` provenance and confidence `0.97`.

The call-result confidence distribution is 50 edges at 0.97, 11 at 0.90,
6 at 0.99, and 5 at 0.75. Most candidate sites intentionally fail closed
because the receiver factory has no eligible explicit simple nominal return,
or because a factory, type, or member is ambiguous.

Against the preceding accepted inline-callback database, the persisted graph
has 5,158 removed and 280 added relationship records, for a net reduction of
4,878. Every change has `tree-sitter/name-resolution` provenance. The removed
set contains 4,836 language-wide fallback observations, 193 Harmony-project
fallback observations, and 129 same-file observations. The added set contains
208 imported-package resolutions and the 72 annotation-backed call-result
edges. All preceding 15 correlated assertions remain green.

The snapshot audit initially caught two ArkUI `Circle(...).colorPicker(...)`
inline callbacks being mistaken for ordinary factory call results. The
accepted implementation excludes unshadowed ArkUI intrinsic receivers and
retains project-declared/imported shadows. The corrected full snapshot has zero
removed or added `dynamic/callback-inline` and `dynamic/callback-argument`
records relative to the accepted pre-layer database; both affected callback
containment and invocation pairs are restored.

- Structurely binary SHA-256:
  `b2b0ade8ebbfa907fa25c7b3ae6622c2e6c79606c9f10986f4c0ccdd1cddb91b`
- Raw acceptance SHA-256:
  `7878a2ab1a84255030146beee8b9ba067281982c1ca311c2deac563e023fc5d4`
- Raw init SHA-256:
  `8d713e6d810d863599176b85fa8c6cebb4374484cfdeb0fb6c70f404469ab9fa`
- Timing SHA-256:
  `80797e200d5f5d08966c48ac9ccbae6130ee1e6a73006a6e54cc56b28b9896dd`
