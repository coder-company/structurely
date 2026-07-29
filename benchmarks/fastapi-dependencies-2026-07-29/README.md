# FastAPI dependency acceptance — 2026-07-29

This gate verifies Structurely commit
`641a28fe6336a028a958ba19373c2ea9d179039c` against production FastAPI
sources from pinned LightRAG commit
`44db36fe080645ba97ded719d44b42c7dee1f54a` and pinned Graphiti commit
`526dcad7a300f3c5c506ff96a68bcdc7ca9f97ed`.

The clean release binary resolves exactly 49 direct dependency sites:

- LightRAG: 36 direct sites across six files, including 35 decorator
  dependencies and the parameter dependency on `get_status`;
- Graphiti: 13 direct edges across five files, producing exactly ten
  endpoint → `get_graphiti` → `get_settings` paths.

Every edge targets the exact callable symbol and file and carries an exact,
nonzero evidence-site identity. The gate rejects duplicates, cross-project
leakage, and a bare `Depends()` with no callable.

Graph model v59 and relationship schema v2 persist dependency observations and
preserve distinct dependency sites. Resolution supports import/barrel/package
aliases, verified `Annotated` aliases, exact lexical dominance, nested callable
factory returns, and stable incremental cleanup. Spoofed imports, reassigned or
late aliases, ambiguous returns, cycles, dynamic expressions, and arbitrary
annotations fail closed behind depth and work caps.

The final gate passes 233 library tests, daemon and persistent MCP process
tests, strict all-target/all-feature Clippy, formatting, and diff checks. The
release binary SHA-256 is
`6bb01b41cbd408199fe569c69430fd6e5d95255b6966b77dd540761199d640fd`;
the raw result SHA-256 is
`12a5545822c519c0060f1bda19cf62af14ad2b03f3fa00b383f1d20b3ff01051`.

Reproduce with:

```sh
python3 scripts/acceptance_fastapi_dependencies.py \
  --structurely target/release/structurely \
  --lightrag-repository /path/to/pinned/lightrag \
  --graphiti-repository /path/to/pinned/graphiti \
  --output results.json
```
