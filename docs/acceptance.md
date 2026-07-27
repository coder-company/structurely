# Production acceptance gates

Structurely is complete only when all gates are evidenced on pinned fixtures.

## Correctness

- Clean and incremental graph snapshots are byte-for-byte deterministic.
- Line-only and comment-only edits preserve symbol IDs.
- Every relationship contains valid evidence and a confidence in `[0, 1]`.
- Differential MCP fixtures preserve required CodeGraph fields.
- Crash injection cannot expose a partially updated graph epoch.

## Quality

- TypeScript, JavaScript, Python, and Rust have held-out semantic fixtures.
- Ambiguous relationship precision and recall are reported per language.
- Framework resolver adapters require adversarial fixtures and two real repos.

## Performance

- Common one-file changes become query-visible within 300 ms on the reference
  fixture.
- Large-repository indexing is at least 2x the pinned CodeGraph baseline or
  delivers a statistically significant semantic-quality improvement.
- Peak memory is sublinear in worker count.
- WAL growth is bounded and checkpointed.

## Product

- CLI and MCP compatibility suites pass on Linux, macOS, and Windows.
- Install, upgrade, uninstall, telemetry policy, and troubleshooting are
  documented.
- Release artifacts have checksums and build provenance.
