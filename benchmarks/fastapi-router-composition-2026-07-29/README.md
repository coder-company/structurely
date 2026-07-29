# FastAPI router-composition acceptance — 2026-07-29

This gate verifies Structurely commit `c5d87fff04aaaec7dea8d02a2da1d56cbcabffe1`
against production FastAPI sources from pinned LightRAG commit
`44db36fe080645ba97ded719d44b42c7dee1f54a` and pinned Graphiti commit
`526dcad7a300f3c5c506ff96a68bcdc7ca9f97ed`.

The clean release binary materializes all 44 mounted production endpoints:

- LightRAG: 34 routes and 34 exact route-to-handler relationships across five
  selected files in 141.333 ms wall time;
- Graphiti: 10 routes and 10 exact route-to-handler relationships across three
  selected files in 39.355 ms wall time.

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

The final gate passes 226 library tests, daemon and persistent MCP process
tests, strict all-target/all-feature Clippy, formatting, and diff checks. The
release binary SHA-256 is
`495bf60770ea7817bfdbf7bc6d723e59705cdd4853c99a68b106f3ce9bd179da`;
the raw result SHA-256 is
`de9934074842db54cd2577a4dde0dab3d19dbb448369659dd5358deb0c830da8`.

Reproduce with:

```sh
python3 scripts/acceptance_fastapi_router_composition.py \
  --structurely target/release/structurely \
  --lightrag-repository /path/to/pinned/lightrag \
  --graphiti-repository /path/to/pinned/graphiti \
  --output results.json
```
