# Production acceptance gates

Structurely is complete only when all gates are evidenced on pinned fixtures.

## Correctness

- Clean and incremental graph snapshots are byte-for-byte deterministic.
- Line-only and comment-only edits preserve symbol IDs.
- Every relationship contains valid evidence and a confidence in `[0, 1]`.
- Differential MCP fixtures preserve required CodeGraph fields.
- Injected rollback cannot expose a partially updated graph epoch.

## Quality

- TypeScript, JavaScript, Python, and Rust have executable semantic fixtures.
- Relationship precision and recall are reported in aggregate and per language.
- Framework resolver adapters require adversarial fixtures and two real repos.
- Function-reference and dynamic-dispatch facts have bounded fanout,
  provenance, confidence, and false-positive fixtures.
- Ambiguous call targets are capped at six; heritage links prefer module hints,
  same-file declarations, and explicit imports, and reject non-unique global
  fallbacks.
- TypeScript aliases and workspace packages pass wildcard, ambiguity,
  traversal, scoped-package, and cross-package fixtures.

## Performance

- Common one-file changes become query-visible within 300 ms on the pinned
  semantic fixture in Linux CI.
- Large-repository end-to-end indexing is at least 2x the pinned CodeGraph
  baseline on an identical file intersection.
- Peak memory is sublinear in worker count on the pinned large-repository
  fixture.
- WAL growth is bounded and checkpointed.

## Product

- CLI and MCP compatibility suites pass on Linux, macOS, and Windows.
- Explore and Node enforce documented output budgets and disclose omitted or
  stale context rather than silently truncating it.
- Project configuration precedence, nested repositories, worktrees, and
  malformed configuration have executable behavioral suites.
- Shared-daemon locking, fallback, catch-up, watchdog, and teardown behavior is
  exercised on every supported operating system.
- Supported coding-agent installers are idempotent and preserve unrelated user
  configuration during install, upgrade, and uninstall.
- Install, upgrade, uninstall, telemetry policy, and troubleshooting are
  documented.
- Release artifacts have checksums and build provenance.

Run the pinned representative-repository assertions with
`scripts/acceptance_repositories.py`. The manifest records exact upstream
commits and semantic expectations; the runner uses detached temporary clones
so it cannot accept a dirty or drifting source tree. Current evidence is
checked in under `benchmarks/real-repositories-2026-07-28/`.
The pinned 6,995-file ArkTS acceptance result is recorded separately under
`benchmarks/openharmony-arkts-2026-07-28/`; its event assertion correlates
origin, provenance, confidence, and source line in one relationship record.
The follow-up `benchmarks/openharmony-ohpm-2026-07-28/` gate adds a correlated
cross-package component assertion through a real `oh-package.json5` dependency.
The `benchmarks/openharmony-router-2026-07-28/` gate adds an exact,
evidence-bearing literal page transition and retains all earlier ArkTS
assertions across the same 6,995-file pin.
The `benchmarks/openharmony-arkui-helper-2026-07-28/` gate adds exact global
`@Extend` and component-owned `@Styles` helper edges and passes 6/6 correlated
assertions on that pin.
The `benchmarks/openharmony-ohos-emitter-2026-07-28/` gate adds an exact
`@ohos.events.emitter` dispatch-to-callback relationship and passes 7/7
correlated assertions on that pin. The matching differential and performance
artifacts prove 22/22 shared compatibility, one separately scored
Structurely-only emitter flow, and both enforced 2× speed thresholds at the
same Structurely commit.
The `benchmarks/openharmony-imported-emitter-2026-07-28/` gate adds a real
cross-file immutable event descriptor from KeyManager and passes 8/8
correlated assertions. Matching differential and performance artifacts pin the
same clean Structurely commit.
The `benchmarks/openharmony-arkui-builder-2026-07-29/` gate adds a real
children-bearing `bindPopup` registration and passes 9/9 correlated assertions.
Matching differential and performance artifacts pin the same clean Structurely
commit and binary.
The `benchmarks/openharmony-callback-arguments-2026-07-29/` gate adds exact
formal-to-actual callback propagation through nested closures and passes 10/10
correlated assertions. Matching differential and performance artifacts pin the
same clean Structurely commit and binary.

The authoritative capability inventory is
[`codegraph-parity.md`](codegraph-parity.md). A matching command name or schema
does not by itself satisfy behavioral parity.
