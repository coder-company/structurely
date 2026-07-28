# Benchmark protocol

Performance claims must use pinned source revisions, fresh temporary indexes,
the same host, and at least 20 measured query iterations.

## Structurely report

```bash
structurely benchmark \
  --path /path/to/fixture \
  --query UserService \
  --iterations 20 > structurely.json
```

For a fresh-index measurement, remove only the fixture's generated
`.structurely` directory before running. The report contains the initial index
duration, no-change sync p50/p95, query p50/p95, database size, and graph
cardinality.

## CodeGraph baseline

Pin a CodeGraph release and measure the same checked-out fixture, query, host,
and iteration count. Normalize the result to:

```json
{
  "version": "1.5.0",
  "commit": "pinned-source-commit",
  "fresh_index_ms": 1000,
  "query_p50_us": 10000,
  "database_bytes": 1000000
}
```

Record the exact commands, Node/Rust versions, operating system, CPU, memory,
and whether caches were warm alongside every published result.

## Compare

```bash
python3 scripts/compare_benchmarks.py \
  --structurely structurely.json \
  --codegraph codegraph.json \
  --output comparison.json
```

The comparator reports speedup ratios and preserves the raw normalized values.
Semantic precision and recall must be reported separately; faster incorrect
edges do not satisfy Structurely's acceptance gates.

Checked-in raw comparisons live under `benchmarks/`. The
[`semantic-2026-07-27`](../benchmarks/semantic-2026-07-27/README.md) run compares
Structurely with pinned CodeGraph 1.5.0 on the four-file semantic fixture. It is
explicitly a startup smoke test; large-repository claims require a separate
pinned run.

The
[`codegraph-source-intersection-2026-07-27`](../benchmarks/codegraph-source-intersection-2026-07-27/README.md)
run is the large-repository comparison. Both engines index the same 441 files
from the pinned CodeGraph source tree across five fresh trials. Structurely's
median end-to-end indexing is 3.96× faster, query p50 is 37.66× faster, and
peak initialization RSS is 91.1% lower on that host.

The
[`codegraph-source-intersection-2026-07-28-post-semantics`](../benchmarks/codegraph-source-intersection-2026-07-28-post-semantics/README.md)
rerun covers the expanded semantic model. Structurely remains 3.64× faster to
index and 8.76× faster for query-process p50 while using 89.2% less peak
memory. Pass `--minimum-index-speedup 2 --minimum-query-speedup 2` to make the
benchmark command fail when either required advantage regresses.

The
[`codegraph-source-intersection-2026-07-28-post-dispatch`](../benchmarks/codegraph-source-intersection-2026-07-28-post-dispatch/README.md)
rerun covers React runtime and interface dispatch. Structurely remains 3.85×
faster to index and 9.08× faster for query p50 while using 89.5% less peak
memory.

The
[`codegraph-source-intersection-2026-07-28-post-components`](../benchmarks/codegraph-source-intersection-2026-07-28-post-components/README.md)
rerun covers Vue/Svelte extraction and corrected import fanout. Structurely
remains 3.96× faster to index and 9.19× faster for query p50 while using 89.8%
less peak memory.

The
[`codegraph-source-intersection-2026-07-28-post-arkts`](../benchmarks/codegraph-source-intersection-2026-07-28-post-arkts/README.md)
rerun covers ArkTS/ArkUI extraction and invalid-source resilience. Structurely
passes both enforced 2× gates: 3.71× faster indexing and 9.17× faster query p50,
with 90.0% less peak initialization memory.

## Semantic quality

Quality manifests list expected caller/callee edges by language. Evaluate a
fixture from its indexed graph:

```bash
structurely init fixtures/semantic
structurely quality \
  --path fixtures/semantic \
  --manifest fixtures/semantic/quality.json
```

The command emits aggregate and per-language precision/recall and exits
non-zero for any false positive or false negative. CI runs the checked-in
TypeScript, JavaScript, Python, and Rust fixture on every change.

## Agent-facing context usefulness

`scripts/differential_mcp.py` drives persistent MCP sessions for Structurely
and pinned CodeGraph, then scores required facts, relevant-file recall, file
precision, flow-spine coverage, line-numbered source, and output budget. The
checked-in [`differential-mcp-2026-07-28`](../benchmarks/differential-mcp-2026-07-28/README.md)
run passes all 17 compatibility scenarios and scores Structurely 1.0000 versus
CodeGraph 0.9583 on its flow fixture. The
[`post-ArkTS differential`](../benchmarks/differential-mcp-2026-07-28-post-arkts/README.md)
passes 19/19 scenarios, adding ArkUI event and reactive-state flows; it verifies
the comparator's declared Git commit and package version before running.

Pinned large-repository ArkTS evidence is recorded in
[`openharmony-arkts-2026-07-28`](../benchmarks/openharmony-arkts-2026-07-28/README.md).
The exact 6,995-file ETS corpus indexed in 106.26 seconds and passed a
correlated real ArkUI event-flow assertion.

After Harmony package resolution, the
[`post-ohpm differential`](../benchmarks/differential-mcp-2026-07-28-post-ohpm/README.md)
passes 20/20 scenarios. The pinned
[`OpenHarmony ohpm gate`](../benchmarks/openharmony-ohpm-2026-07-28/README.md)
resolves a real `@ohos/window-component` import, and the enforced
[`post-ohpm performance run`](../benchmarks/codegraph-source-intersection-2026-07-28-post-ohpm/README.md)
remains 3.90× faster to index and 9.81× faster for query p50.

After hardened ArkUI page routing, the
[`post-router differential`](../benchmarks/differential-mcp-2026-07-28-post-router/README.md)
passes 21/21 scenarios for both engines and explicitly requires Structurely
route provenance. Both engines score 1.0000 for context usefulness; Structurely
uses 2,164 response characters versus CodeGraph's 2,560. Adversarial tests
separately cover import binding, lexical shadows, ambiguous entries, bounded
path normalization, and incremental cleanup.
The enforced
[`post-router performance run`](../benchmarks/codegraph-source-intersection-2026-07-28-post-router/README.md)
remains 3.66× faster to index and 9.55× faster for query p50 while using 88.4%
less peak initialization memory.
The pinned
[`OpenHarmony router gate`](../benchmarks/openharmony-router-2026-07-28/README.md)
indexes all 6,995 ETS files in 114.929 seconds and passes four correlated
semantic assertions, including the exact `Index.build → DocumentPage` route.

After decorated ArkUI style-helper resolution, the
[`post-helper differential`](../benchmarks/differential-mcp-2026-07-28-post-arkui-helper/README.md)
passes 22/22 scenarios for both engines and explicitly requires
`framework/arkui-helper` evidence from Structurely.
The enforced
[`post-helper performance run`](../benchmarks/codegraph-source-intersection-2026-07-28-post-arkui-helper/README.md)
remains 4.35× faster to index and 9.23× faster for query p50 while using 88.0%
less peak initialization memory.
The pinned
[`OpenHarmony helper gate`](../benchmarks/openharmony-arkui-helper-2026-07-28/README.md)
indexes all 6,995 ETS files and passes 6/6 assertions, including real global
`@Extend` and component-owned `@Styles` call edges.

After app-scoped Harmony emitter resolution, the
[`post-emitter differential`](../benchmarks/differential-mcp-2026-07-28-post-ohos-emitter/README.md)
keeps both engines at 22/22 shared compatibility predicates and 1.0000 context
usefulness. Structurely separately passes a Structurely-only emitter predicate
that pinned CodeGraph does not resolve.
The enforced
[`post-emitter performance run`](../benchmarks/codegraph-source-intersection-2026-07-28-post-ohos-emitter/README.md)
is 3.63× faster to index and 9.17× faster for query p50 while using 90.5% less
peak initialization memory.
The pinned
[`OpenHarmony emitter gate`](../benchmarks/openharmony-ohos-emitter-2026-07-28/README.md)
indexes all 6,995 ETS files and passes 7/7 assertions, including an exact
app-scoped `PageThreadModel.build → PageThreadModel.build.callback` dispatch
edge with emitter provenance.

The follow-up
[`imported-emitter differential`](../benchmarks/differential-mcp-2026-07-28-post-imported-emitter/README.md)
retains 22/22 shared compatibility and 1.0000 usefulness for both engines.
Its enforced
[`performance run`](../benchmarks/codegraph-source-intersection-2026-07-28-post-imported-emitter/README.md)
is 3.675× faster to index and 9.622× faster at query p50 while using 89.87%
less peak memory. The
[`real OpenHarmony gate`](../benchmarks/openharmony-imported-emitter-2026-07-28/README.md)
passes 8/8 assertions and adds the cross-file imported KeyManager event
descriptor flow.
