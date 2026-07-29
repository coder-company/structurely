# Operations

## Install and upgrade

The supported installer currently builds the pinned dependency graph with
Cargo. Rust 1.88 or newer is required.

macOS and Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/coder-company/structurely/main/scripts/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/coder-company/structurely/main/install.ps1 | iex
```

Both installers use `cargo install --locked --force`, so rerunning the same
command upgrades or repairs an installation. Pin a release instead of following
`main`:

```bash
STRUCTURELY_VERSION=v0.1.1 sh scripts/install.sh
```

```powershell
$env:STRUCTURELY_VERSION = "v0.1.1"
./install.ps1
```

Confirm the installed binary with `structurely --version`.

## Shared daemon

Start one lock-protected indexer per project:

```bash
structurely daemon start --path /absolute/project
structurely daemon status --path /absolute/project
structurely daemon stop --path /absolute/project
```

Start and stop are idempotent. Status reports the owning PID, graph epoch,
phase, last update time, and terminal indexing error when present. A failed
watcher releases its lock, records the error, and can be restarted after the
source problem is corrected. State epochs are published with a durable atomic
replacement; a publication failure stops the watcher instead of leaving a
running daemon with silently stale status.

MCP requests prefer the shared daemon. Each tool response reports
`_meta.freshness.mode`, `epoch`, and `daemonPid`. If daemon catch-up exceeds
500 ms, the MCP process attempts a foreground sync and reports
`foreground-fallback`; if that also fails, it serves the last committed graph
with an explicit stale warning.

## Nested repositories and worktrees

`includeIgnored` can opt a submodule or embedded repository into one unified
project graph. Structurely distinguishes its `.git` pointer from a linked
worktree pointer: genuine nested repositories are indexed, while linked
worktrees are skipped so the same checkout is not duplicated.

Indexes are root-local and are never silently borrowed from an initialized
ancestor. Run `structurely init /path/to/worktree` when a linked worktree needs
its own branch-specific graph.

## Coding-agent integrations

Structurely can configure a project-local MCP entry for Codex, Claude Code, or
Cursor:

```bash
structurely integrations install codex --path /absolute/project
structurely integrations install claude --path /absolute/project
structurely integrations install cursor --path /absolute/project
```

Replace `install` with `status` or `uninstall` for the corresponding operation.
Codex uses `.codex/config.toml`; Claude Code uses `.mcp.json`; Cursor uses
`.cursor/mcp.json`. The commands are idempotent, use the absolute installed
Structurely executable and project path, preserve unrelated configuration, and
remove only the `structurely` server entry.

Configuration updates use a unique temporary file in the destination
directory, synchronize it, and atomically replace the previous configuration.
A failed publication preserves the previous file and cleans up its temporary
file.

## Uninstall

Remove the binary managed by Cargo:

```bash
cargo uninstall structurely
```

Indexes belong to individual projects. Uninstalling the binary deliberately
does not delete them. To remove an index, delete only that project's
`.structurely` directory after confirming the project path.

## Privacy and telemetry

Structurely has no telemetry, analytics, crash reporting, network indexing, or
remote query service. Source text, symbols, relationships, and query history
are not transmitted by the application. Indexes remain in
`<project>/.structurely/graph.db`.

The installation commands contact GitHub and the Rust package ecosystem to
fetch source and dependencies. GitHub Actions contacts GitHub services to
publish and attest releases. These build and distribution operations are
separate from Structurely runtime behavior.

## Troubleshooting

`database is locked`
: Run `structurely daemon status --path /absolute/project` and stop an
  unintended writer. The daemon owns one project lock; readers continue through
  SQLite WAL snapshots.

Search results are stale
: Run `structurely sync /path/to/project`, or start the shared daemon.
  `structurely status` reports pending files and MCP responses disclose their
  freshness mode.

The graph model changed
: Run `structurely sync`. Structurely detects the stored model version and
  performs a semantic reindex while preserving the database transaction
  boundary.

An MCP client cannot start the server
: Run `structurely serve --mcp --path /absolute/project/path` in a terminal.
  Fix any initialization error first, then use the same executable and absolute
  path in the client configuration. Protocol messages are written to stdout;
  diagnostics belong on stderr.

Installation fails
: Check `rustc --version` and `cargo --version`, then retry with a pinned tag.
  Corporate proxies must allow GitHub and Cargo dependency downloads.

Disk usage keeps growing
: Stop the writer and run `structurely sync` before inspecting
  `.structurely/graph.db`. Do not edit the SQLite database manually. Report
  reproducible unbounded WAL growth as a bug.

Indexing needs a different CPU/memory tradeoff
: Structurely uses the host's available parallelism, capped at eight workers by
  default. Set `STRUCTURELY_PARSE_WORKERS` to a positive integer to override
  it; values are capped at 16 and at the number of changed files. Every index
  report includes `parse_workers`, `staging_ms`, and `resolution_ms`.

## Backups and recovery

The source tree is authoritative; the graph is reproducible derived data. Back
up source control rather than the index. If an index is corrupt, stop watchers,
move the specific project's `.structurely` directory aside, and run
`structurely init /path/to/project`. Retain the moved database when filing a
bug if it contains no sensitive source-derived data you are unable to share.
