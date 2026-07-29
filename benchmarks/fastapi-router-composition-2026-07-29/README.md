# FastAPI router-composition acceptance — 2026-07-29

This gate verifies Structurely commit `569978055ec4f58051ab06dba281ca056d3e8f1b`
against production FastAPI sources from pinned LightRAG commit
`44db36fe080645ba97ded719d44b42c7dee1f54a` and pinned Graphiti commit
`526dcad7a300f3c5c506ff96a68bcdc7ca9f97ed`.

The clean release binary materializes all 53 deployed production endpoints:

- LightRAG: 42 routes and 42 exact route-to-handler relationships across five
  selected files in 166.228 ms wall time;
- Graphiti: 11 routes and 11 exact route-to-handler relationships across three
  selected files in 39.829 ms wall time.

Every relationship has exact handler and source-file provenance. The gate
rejects duplicate symbols and edges, cross-router handler leakage, bare
duplicates of the 14 `/documents` endpoints, and bare duplicates of the five
class-owned `/api` endpoints.

Two metamorphic checks prevent an apparently correct count from masking broken
composition. The same router modules publish zero endpoints when their
FastAPI application files are removed, and adding `/mounted` to the query
factory mount moves every query endpoint beneath that prefix.

Graph model v58 persists file-owned router, alias, factory, mount, and route
facts, then materializes only application-reachable endpoints. Resolution is
lexically scoped, import-proven, cycle/depth/work bounded, package-`__init__`
aware, incremental, and stable under unrelated insertions. Dynamic paths and
prefixes, rebound constructors/routers, ambiguous targets, unsafe factory
returns, and unmounted routers fail closed.

Direct application routes additionally accept bounded, immutable same-file
string bindings with source-order and lexical-scope proof. This recovers
LightRAG's `/webui` and `/webui/` routes without evaluating dynamic Python.

The final gate passes 229 library tests, daemon and persistent MCP process
tests, strict all-target/all-feature Clippy, formatting, and diff checks. The
release binary SHA-256 is
`dfcec39334b245f11853edf2f919907cf1fad1ae2546d0f6ce64f7287a6608bb`;
the raw result SHA-256 is
`20d2418888ddf4d3dc16564057abdeef217081bf5beb913a3c41460b46a5c54c`.

Reproduce with:

```sh
python3 scripts/acceptance_fastapi_router_composition.py \
  --structurely target/release/structurely \
  --lightrag-repository /path/to/pinned/lightrag \
  --graphiti-repository /path/to/pinned/graphiti \
  --output results.json
```
