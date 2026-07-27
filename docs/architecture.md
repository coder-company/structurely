# Structurely architecture

Structurely is a local-first semantic code intelligence engine. The Rust
process owns the complete update path:

```text
filesystem scan
    -> language parser
    -> versioned facts
    -> stable symbol identity
    -> relationship resolution
    -> one SQLite transaction
    -> graph epoch
```

The CLI and MCP compatibility surface call the same engine interface. SQLite
is the durable source of truth; WAL mode permits concurrent read snapshots
while one writer publishes an update.

## Invariants

1. A reader observes exactly one committed graph epoch.
2. A symbol's public ID does not depend on its line or byte position.
3. Every inferred relationship has provenance, confidence, location, and an
   explanation.
4. Re-indexing a file replaces all file-owned facts atomically.
5. Incremental and clean indexing of the same source produce the same graph.
6. Storage row IDs are never public symbol identities.
7. Parser failure for one file cannot corrupt the previous committed graph.

## Modules

- `model` owns the versioned graph vocabulary and stable identity algorithm.
- `parser` converts supported source text into file-local facts.
- `store` owns schema migration, transactions, search, and graph epochs.
- `engine` owns scan, incremental invalidation, resolution, and publication.
- `mcp` adapts JSON-RPC/MCP requests to the engine.
- `main` adapts command-line commands to the same engine.

These are deliberately deep modules: callers use a small interface while
parsing, transaction ordering, schema details, and compatibility behavior stay
local to their implementations.

