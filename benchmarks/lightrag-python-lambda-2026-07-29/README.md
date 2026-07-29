# LightRAG Python lambda acceptance — 2026-07-29

Clean Structurely commit `073a7a1` advances the graph model to v53 and adds
Python direct positional lambda identities to the existing exact
callback-argument pipeline.

The implementation accepts only a lambda in a direct positional argument slot
whose uniquely resolved project callee invokes that exact formal. Keyword
actuals remain deferred. It fails closed on splats and ambiguous mapping,
variadic formals, default/rest shadowing, assignment, augmented assignment,
walrus binding, loop targets, import/definition aliases, `with`/`except`
aliases, `except*`, and structural pattern captures. Existing 16-level
callback nesting/delegation bounds, transactional cleanup, and rejected-flow
fallback remain unchanged.

The pre-implementation inventory found 206 direct lambda arguments in pinned
LightRAG, but only 25 repo-proven callback candidates: 24 positional and one
keyword. A clean index of commit `44db36f` materializes 30 exact positional
callback flows—24 in production sources and six in tests—because the full
resolver also proves several exact `atomic_write` sites outside the initial
inventory screen.

Every accepted registration has a stable synthetic inline callback identity,
a `dynamic/callback-inline` containment relationship at confidence 1.0, and a
`dynamic/callback-argument` relationship at confidence 0.96. Major production
groups are 15 `LightRAG._run_sync` wrappers, five `_chunk_by_budget` calls, two
production `atomic_write` calls, and two nested
`_run_json_conformance_retry` callbacks.

The clean 515-file run completes in 3.070 seconds wall time with 105,848 KiB
peak RSS and persists 8,203 symbols and 28,737 relationships. The full gate
passes 179 unit tests, daemon and MCP integrations, strict Clippy, and
independent review with no remaining blockers.
