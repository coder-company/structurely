# Command reference

Structurely prints machine-readable JSON unless a command explicitly returns
source-backed text. Run `structurely <command> --help` for the installed
version's exact options.

## Set up a project

```bash
structurely setup <codex|claude|cursor> [project] [--replace-codegraph]
```

`setup` initializes or synchronizes the graph, starts the background indexer,
installs the project-local MCP entry, and verifies the result.
`--replace-codegraph` replaces the existing CodeGraph server entry without
changing unrelated agent settings.

## Create an index

```bash
structurely init [project]
```

`init` scans the project, creates `.structurely/graph.db`, and publishes the
first graph epoch. Run it again to rebuild an existing index safely.

## Update an index

```bash
structurely sync [project]
```

`sync` indexes only changed files and publishes one atomic graph epoch. Start
the daemon instead when you want continuous updates.

## Inspect index health

```bash
structurely status [project]
structurely daemon status --path <project>
```

`status` reports graph freshness, file and symbol counts, storage size, and
recovery warnings. `daemon status` reports background-indexer health.

## Search and explore code

```bash
structurely search <query> --path <project> [--limit <1-100>]
structurely explore <query> --path <project> [--limit <1-100>]
```

`search` returns ranked symbols as JSON. `explore` returns bounded,
line-numbered context for a task or concept.

## Follow relationships

```bash
structurely callers <symbol> --path <project> [--file <relative-path>] [--limit <1-100>]
structurely callees <symbol> --path <project> [--file <relative-path>] [--limit <1-100>]
structurely impact <symbol> --path <project> [--file <relative-path>] [--depth <1-20>]
```

Use `--file` when multiple files declare the same symbol name. `impact`
traverses callers to show what a change may affect. Traversal stops at the
requested depth and at the built-in node and edge safety limits.

## Keep the graph current

```bash
structurely watch <project> [--debounce-ms <milliseconds>]
structurely daemon start --path <project> [--debounce-ms <milliseconds>]
structurely daemon status --path <project>
structurely daemon stop --path <project>
```

`watch` stays attached to your terminal. `daemon start` launches one
lock-protected background indexer for the project. Start and stop are
idempotent.

## Connect a coding agent

```bash
structurely integrations install <codex|claude|cursor> --path <project>
structurely integrations status <codex|claude|cursor> --path <project>
structurely integrations uninstall <codex|claude|cursor> --path <project>
```

These commands update only the project-local `structurely` MCP entry. They
preserve unrelated settings.

## Run the MCP server

```bash
structurely serve --mcp --path <project>
```

The server uses newline-delimited JSON-RPC 2.0 over standard input and output.
Write diagnostics to standard error when you wrap this command.

## Export and verify a graph

```bash
structurely snapshot --path <project>
structurely quality --path <project> --manifest <quality.json>
structurely benchmark --path <project> [--query <query>] [--iterations <1-1000>]
```

`snapshot` emits deterministic graph JSON. `quality` compares the graph with an
expected semantic manifest and exits unsuccessfully on a mismatch. `benchmark`
measures indexing and query latency.
