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

The connection checkpoints after every published epoch, auto-checkpoints after
256 WAL pages, applies a 16 MiB journal size limit, and waits up to five seconds
for a busy writer. `structurely status` reports database and WAL bytes plus the
configured checkpoint limits so growth is observable without opening SQLite.

Individual source files larger than 1 MiB are excluded before reading or
parsing, matching the pinned CodeGraph safety limit for vendored/generated
artifacts. Sync and status reports expose the skipped-file count. If a
previously indexed file grows beyond the limit, the next atomic epoch removes
its stale graph facts.

## Invariants

1. A reader observes exactly one committed graph epoch.
2. A symbol's public ID does not depend on its line or byte position.
3. Every inferred relationship has provenance, confidence, location, and an
   explanation.
4. Re-indexing a file replaces all file-owned facts atomically.
5. Incremental and clean indexing of the same source produce the same graph.
6. Storage row IDs are never public symbol identities.
7. Parser failure for one file cannot corrupt the previous committed graph.

Call resolution ranks receiver-type evidence ahead of same-file, explicit
import, and language-wide candidates. Locally constructed receivers in
TypeScript/JavaScript, Java, Python, and Rust therefore select their class
method even when another class in the same file has the same method name; the
emitted edge records the winning scope and confidence. Constructor inference
supports `new Type()`, Python `Type()` assignments, declared Java-style local
types, and Rust `Type::new()` values.

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
