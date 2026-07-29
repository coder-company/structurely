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

The accepted call-result rerun is recorded under
[`codegraph-source-intersection-2026-07-29-post-call-result`](../benchmarks/codegraph-source-intersection-2026-07-29-post-call-result/README.md).
Structurely is 2.687× faster to index, 9.226× faster at query p50, uses 86.19%
less peak memory, and produces an 11.66% smaller database. The
matching
[`MCP differential`](../benchmarks/differential-mcp-2026-07-29-post-call-result/README.md)
retains 22/22 shared compatibility and 1.0000 usefulness for both engines.

The pinned
[`C macro callback-table gate`](../benchmarks/c-macros-2026-07-29/README.md)
records a semantic superiority result rather than a general speed claim:
Structurely resolves all 14 intended targets while CodeGraph 1.5.0 resolves
one. Structurely also rejects every deliberately impossible target, updates
both source- and header-defined aliases incrementally, and converges on a
zero-change sync.
The enforced
[`post-macro performance run`](../benchmarks/codegraph-source-intersection-2026-07-29-post-macros/README.md)
preserves both 2× gates: Structurely is 2.450× faster to index, 9.024× faster
at query p50, and 7.375× faster at query p95 while using 86.44% less peak
memory and a 4.59% smaller database.
The subsequent
[`production-hardening gate`](../benchmarks/hardening-2026-07-29/README.md)
proves that bounded source snapshots, atomic concurrent epoch publication, and
self-healing daemon reconciliation preserve the performance contract:
Structurely is 2.473× faster to index, 9.061× faster at query p50, and 7.656×
faster at query p95 while using 84.83% less peak memory and a 4.56% smaller
database. Its graph-model-v68 snapshot is byte-identical to the independently
built pre-hardening baseline across 441 files, 5,213 symbols, and 38,538
relationships.
The pinned
[`OpenHarmony call-result gate`](../benchmarks/openharmony-call-result-2026-07-29/README.md)
passes 16/16 correlated assertions across 6,995 ETS files. Its new assertion
follows `InputHandler.getInstance()` through a verified import and explicit
nominal return annotation to `InputHandler.insertText`; the inventory records
239 return facts and 72 published call-result relationships.

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

After recovered children-bearing ArkUI Builder modifier chains, the
[`post-Builder differential`](../benchmarks/differential-mcp-2026-07-29-post-arkui-builder/README.md)
keeps both engines at 22/22 shared compatibility and 1.0000 usefulness. The
enforced
[`performance run`](../benchmarks/codegraph-source-intersection-2026-07-29-post-arkui-builder/README.md)
is 3.852× faster to index, 9.435× faster at query p50, and 9.807× faster at
query p95 while using 88.69% less peak memory. The
[`real OpenHarmony gate`](../benchmarks/openharmony-arkui-builder-2026-07-29/README.md)
passes 9/9 assertions and adds the exact
`ActionBarButton.build → ActionBarButton.PopupBuilder` registration.

After exact callback-argument propagation, the
[`post-callback differential`](../benchmarks/differential-mcp-2026-07-29-post-callback-arguments/README.md)
keeps both engines at 22/22 shared compatibility and 1.0000 usefulness. The
enforced
[`performance run`](../benchmarks/codegraph-source-intersection-2026-07-29-post-callback-arguments/README.md)
is 3.252× faster to index, 9.319× faster at query p50, and 8.265× faster at
query p95 while using 87.12% less peak memory. The
[`real OpenHarmony gate`](../benchmarks/openharmony-callback-arguments-2026-07-29/README.md)
passes 10/10 assertions and proves the package-scoped nested-closure callback
flow from `RequestDownload.downloadFile` to `Download.downloadFileCallback`.

After bounded same-file ArkUI `@BuilderParam` assignment resolution, the
[`post-BuilderParam differential`](../benchmarks/differential-mcp-2026-07-29-post-builder-param/README.md)
keeps both engines at 22/22 shared compatibility and 1.0000 usefulness. The
enforced
[`performance run`](../benchmarks/codegraph-source-intersection-2026-07-29-post-builder-param/README.md)
is 3.273× faster to index, 9.358× faster at query p50, and 6.825× faster at
query p95 while using 88.86% less peak memory. The
[`real OpenHarmony gate`](../benchmarks/openharmony-builder-param-2026-07-29/README.md)
passes 11/11 assertions and proves the exact
`HeatHistogramContent.build → HeatHistogramContent.content` assignment with
BuilderParam provenance.

After project-aware BuilderParam Facts and trailing-child/imported-builder
resolution, the
[`post-builder-flow differential`](../benchmarks/differential-mcp-2026-07-29-post-builder-flow/README.md)
keeps both engines at 22/22 shared compatibility and 1.0000 usefulness. The
enforced
[`performance run`](../benchmarks/codegraph-source-intersection-2026-07-29-post-builder-flow/README.md)
is 3.080× faster to index, 9.411× faster at query p50, and 7.755× faster at
query p95 while using 88.62% less peak memory. The
[`real OpenHarmony gate`](../benchmarks/openharmony-builder-flow-2026-07-29/README.md)
passes 13/13 assertions, indexes 48,370 symbols and 88,790 relationships, and
proves both `CodeView.build →` its supplied trailing child and
`TitleExpansionView.build → titleMenu/titleContent` through verified imports.

After exact inline arrow/function BuilderParam adapters and transactional
synthetic-Symbol materialization, the
[`post-inline-builder differential`](../benchmarks/differential-mcp-2026-07-29-post-inline-builder/README.md)
keeps both engines at 22/22 shared compatibility and 1.0000 usefulness. The
enforced
[`performance run`](../benchmarks/codegraph-source-intersection-2026-07-29-post-inline-builder/README.md)
is 3.399× faster to index, 9.460× faster at query p50, and 8.278× faster at
query p95 while using 89.26% less peak memory. The
[`real OpenHarmony gate`](../benchmarks/openharmony-inline-builder-2026-07-29/README.md)
passes 14/14 assertions across 6,995 files and proves
`ListExchange.build →` its exact inline `deductionView` adapter. Rejected
ordinary object callbacks and ambiguous trailing children leave no synthetic
Symbol behind.

After byte-exact callsite correlation and bounded transitive callback
delegation, the
[`post-delegation differential`](../benchmarks/differential-mcp-2026-07-29-post-callback-delegation/README.md)
keeps both engines at 22/22 shared compatibility and 1.0000 usefulness. The
enforced
[`performance run`](../benchmarks/codegraph-source-intersection-2026-07-29-post-callback-delegation/README.md)
is 3.268× faster to index, 8.727× faster at query p50, and 4.392× faster at
query p95 while using 87.53% less peak memory and a 12.06% smaller database.
The
[`OpenHarmony inventory`](../benchmarks/openharmony-callback-delegation-2026-07-29/README.md)
retains 14/14 prior assertions and records 3,412 delegation observations but
zero complete delegated registrations. This negative result is why inline
callback identities, rather than broader speculative forwarding, are next.

After accepted inline callback identities and explicit call-result receiver
resolution, the pinned gates advance through
[`OpenHarmony inline callbacks`](../benchmarks/openharmony-inline-callback-2026-07-29/README.md)
and
[`OpenHarmony call results`](../benchmarks/openharmony-call-result-2026-07-29/README.md),
reaching 16/16 correlated assertions while retaining 22/22 MCP compatibility
and both enforced 2× performance thresholds.

After scope-aware receiver typing and bounded inherited-member lookup, the
[`final differential`](../benchmarks/differential-mcp-2026-07-29-post-inherited-member/README.md)
passes 22/22 with 1.0000 usefulness and an exact pinned baseline match. The
[`final performance run`](../benchmarks/codegraph-source-intersection-2026-07-29-post-inherited-member/README.md)
is 2.668× faster to index and 9.627× faster at query p50 while using 87.20%
less peak memory and a 12.76% smaller database. Its semantic audit finds zero
0.995 removals and zero callback losses. The
[`OpenHarmony inherited-member gate`](../benchmarks/openharmony-inherited-member-2026-07-29/README.md)
passes 17/17 across 6,995 files and records 23 exact inherited relationships
without losing the VoiceCallDemo nullable-receiver callback flows.

The
[`generic receiver and constructor-descriptor gate`](../benchmarks/generic-receiver-emitter-descriptor-2026-07-29/README.md)
then records 34 `LRUCache` confidence upgrades on the 441-file intersection
and recovers all eight real OrangeShopping constructor-built emitter channel
facts (four registrations and four dispatches) on the 6,995-file pin.

The
[`stored-callback gate`](../benchmarks/openharmony-stored-callback-2026-07-29/README.md)
adds two exact 0.96 DistributedRdb callback-argument relationships and their
two inline identities on the same OpenHarmony pin, with zero callback
relationship removals. The underlying inventory contains 48 exact shapes, 36
of which meet the conservative single-assignment/no-escape screen.

The
[`LightRAG Python lambda gate`](../benchmarks/lightrag-python-lambda-2026-07-29/README.md)
materializes 30 exact positional lambda callback flows: 24 production and six
test relationships. The clean 515-file index takes 3.070 seconds wall time and
105,848 KiB peak RSS; keyword callback actuals remain explicitly deferred.

The
[`Python keyword callback gate`](../benchmarks/python-keyword-callback-2026-07-29/README.md)
closes that deferred mapping with 30 exact keyword callback relationships
across pinned LightRAG and Django. LightRAG reaches 46 accepted inline callback
flows in total; Django reaches 90.

The
[`NestJS transport gate`](../benchmarks/nestjs-transport-2026-07-29/README.md)
materializes all 101 inventoried production websocket, microservice, and gRPC
handlers across 32 pinned Nest files. Including two maintained test handlers,
103 route symbols produce 206 exact framework relationships in a 4.400-second
clean index.

The
[`NestJS GraphQL gate`](../benchmarks/nestjs-graphql-2026-07-29/README.md)
materializes all 42 pinned production resolver handlers across 13 files:
18 queries, 11 mutations, six subscriptions, five fields, and two federation
references. Their route and exact handler edges produce 84 framework
relationships in a 4.350-second clean index.

The
[`current CodeGraph comparison`](../benchmarks/codegraph-current-2026-07-29/README.md)
then reconfirms 22/22 shared MCP compatibility and 1.0000 usefulness for both
engines. Structurely is 2.611× faster to index, 9.491× faster at query p50,
uses 87.50% less peak memory, and has a 12.29% smaller database. Its normalized
441-file semantic snapshot has zero additions, removals, or callback losses
against accepted v51.

The
[`FastAPI router-composition gate`](../benchmarks/fastapi-router-composition-2026-07-29/README.md)
materializes all 53 production endpoints across pinned LightRAG and Graphiti:
42 and 11 respectively. It additionally requires zero public routes from
unmounted router-only corpora and proves that a changed factory mount prefix
propagates to every descendant.

The
[`FastAPI dependency gate`](../benchmarks/fastapi-dependencies-2026-07-29/README.md)
resolves exactly 49 production dependency sites across the same pinned
repositories: 36 in LightRAG and 13 in Graphiti. Graphiti's direct edges form
exactly ten endpoint → `get_graphiti` → `get_settings` paths. Every edge has
exact target-file provenance and a distinct nonzero evidence-site identity.

The
[`C/C++ function-pointer gate`](../benchmarks/c-function-pointer-2026-07-29/README.md)
resolves exact `Release → Callback` flows in two pinned OpenHarmony players and
27 libsamplerate dispatch relationships. Its 14 `src_process` edges cover both
dispatch sites and seven exact implementations; four `src_callback_read` edges
cross call argument, formal parameter, stored field, and indirect invocation.
Incremental mutation proves stale-edge removal and exact callback rebinding.
