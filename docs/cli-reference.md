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
structurely research <query> --path <project> [--max-files <1-100>]
```

`search` returns ranked symbols as JSON. `explore` returns bounded,
line-numbered symbol context for a task or concept. `research` combines that
semantic context with ranked chunks from source, documentation, configuration,
and other useful repository files. Its response identifies the graph epoch and
keeps findings bounded by distinct file count.

## Follow relationships

```bash
structurely callers <symbol> --path <project> [--file <relative-path>] [--limit <1-100>]
structurely callees <symbol> --path <project> [--file <relative-path>] [--limit <1-100>]
structurely impact <symbol> --path <project> [--file <relative-path>] [--depth <1-20>]
structurely trace <source> <target> --path <project> \
  [--source-file <relative-path>] [--target-file <relative-path>] [--depth <1-20>]
```

Use `--file` when multiple files declare the same symbol name. `impact`
traverses callers to show what a change may affect. Traversal stops at the
requested depth and at the built-in node and edge safety limits. `trace`
returns the shortest relationship path it can prove between two symbols,
including the evidence attached to each edge. Use the file options to
disambiguate repeated names.

## Preserve team context

Workspace IDs scope sessions and memories:

```bash
structurely workspace create <name> [--path <project>]
structurely workspace list [--path <project>] [--limit <1-100>]
structurely workspace rename <id> <name> [--path <project>]
```

Record an ordered session history and close it when the work is complete:

```bash
structurely session start <workspace-id> <title> [--path <project>]
structurely session add <session-id> <kind> <body> [--path <project>]
structurely session show <session-id> [--path <project>] [--limit <1-100>]
structurely session list [--workspace <workspace-id>] [--limit <1-100>]
structurely session end <session-id> [--path <project>]
structurely recap <session-id> [--path <project>]
```

An event `kind` is a short caller-defined category such as `decision`,
`finding`, or `result`. Events can be added only while a session is active.
`recap` deterministically summarizes the ordered event history and can be run
again after more events are added.

Store knowledge that should survive individual sessions:

```bash
structurely memory remember <workspace-id> <body> \
  [--tags <comma-separated-tags>] [--path <project>]
structurely memory recall <workspace-id> <query> \
  [--path <project>] [--limit <1-100>]
structurely memory forget <memory-id> [--path <project>]
```

Recall is full-text ranked and remains scoped to one workspace. `forget`
permanently removes the selected memory.

Create or restore a consistent backup of the authoritative state database:

```bash
structurely state backup /safe/path/state-backup.db [--path <project>] [--force]
structurely state restore /safe/path/state-backup.db --path <project> --force
```

Backup includes committed WAL state. Restore validates the schema, SQLite
integrity, and foreign keys before atomically replacing the live database, and
refuses to race another process using project state.

Workspaces, sessions, recaps, and memories remain local to the project.
Structurely does not expose a cloud synchronization command.

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
