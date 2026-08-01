# Operations

## Install and upgrade

The supported installers download a native release archive, verify its SHA-256
checksum, smoke-test the binary, and publish it atomically into a user-local
binary directory. Rust and administrator access are not required.

macOS and Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/coder-company/structurely/main/scripts/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/coder-company/structurely/main/install.ps1 | iex
```

Rerunning an installer upgrades or repairs the installation. Pin a release
instead of resolving the latest published tag:

```bash
STRUCTURELY_VERSION=v0.4.0 sh scripts/install.sh
```

```powershell
$env:STRUCTURELY_VERSION = "v0.4.0"
./install.ps1
```

Confirm the installed binary with `structurely --version`.

The installer uses four explicit stages: platform detection, release download,
checksum and startup verification, and atomic publication. Rerunning it reports
whether the binary was upgraded, repaired, or freshly installed. A failure
before publication preserves the existing executable.

Customize automation with these environment variables:

- `STRUCTURELY_VERSION` pins a release tag;
- `STRUCTURELY_INSTALL_DIR` selects the binary directory;
- `STRUCTURELY_DASHBOARD_SETUP` selects `vercel`, `cloudflare`, `local`,
  `skip`, or `prompt`;
- `NO_COLOR` or `STRUCTURELY_NO_COLOR` disables terminal styling.

Interactive installers offer an optional private dashboard after the binary is
verified. Redirected input and CI do not prompt. Set
`STRUCTURELY_DASHBOARD_SETUP` to `vercel`, `cloudflare`, `local`, `skip`, or
`prompt` for explicit automation. A dashboard deployment failure does not roll
back the successful binary installation. See the [private dashboard
guide](dashboard.md).

After project setup, run the non-mutating health check:

```bash
structurely doctor --client codex /absolute/project
```

It exits with status `0` when the required project, index, daemon, and selected
agent integration checks pass. Warnings such as a stopped optional dashboard
do not fail the command. Required failures return status `2` and include an
actionable `remedy` in the JSON report.

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

Use one command for the normal first run:

```bash
cd /absolute/project
structurely setup codex
```

To replace an existing CodeGraph entry while preserving unrelated settings:

```bash
structurely setup codex --replace-codegraph
```

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

Remove a native Unix installation:

```bash
rm "${STRUCTURELY_INSTALL_DIR:-${XDG_BIN_HOME:-$HOME/.local/bin}}/structurely"
```

Remove a native Windows installation:

```powershell
Remove-Item "$env:LOCALAPPDATA\Programs\Structurely\bin\structurely.exe"
```

Indexes belong to individual projects. Uninstalling the binary deliberately
does not delete them. To remove an index, delete only that project's
`.structurely` directory after confirming the project path.

## Privacy and telemetry

Structurely has no telemetry, analytics, crash reporting, network indexing, or
remote query service. Source text, symbols, relationships, and query history
are not transmitted by the application. Indexes remain in
`<project>/.structurely/graph.db` and `<project>/.structurely/content.db`.
Durable workspace, session, recap, and memory data remains in
`<project>/.structurely/state.db`.

Structurely does not expose a cloud synchronization command or endpoint.

An optional Vercel or Cloudflare deployment uploads only the dashboard's static
HTML, CSS, JavaScript, and security-header files. Repository data is read from
the token-paired loopback bridge directly by the browser and is not sent to the
hosting provider.

The native installation commands contact GitHub to fetch a release archive and
its checksum. GitHub Actions contacts GitHub services to publish and attest
releases. These build and distribution operations are separate from Structurely
runtime behavior.

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
  `.structurely/graph.db` and `.structurely/content.db`. Do not edit the SQLite
  databases manually. Report reproducible unbounded WAL growth as a bug.

A workspace, session, or memory is missing
: Confirm that `--path` points to the project where the state was created.
  Durable state is stored per project in `.structurely/state.db`; it is not
  synchronized between checkouts or machines in the current release.

Indexing needs a different CPU/memory tradeoff
: Structurely uses the host's available parallelism, capped at eight workers by
  default. Set `STRUCTURELY_PARSE_WORKERS` to a positive integer to override
  it; values are capped at 16 and at the number of changed files. Every index
  report includes `parse_workers`, `staging_ms`, and `resolution_ms`.

## Backups and recovery

The source tree is authoritative for `graph.db` and `content.db`; both are
reproducible derived data. `state.db` is authoritative for workspace history
and memory, so back it up before removing `.structurely`.

```bash
structurely state backup /safe/path/state-backup.db --path /path/to/project
structurely state restore /safe/path/state-backup.db \
  --path /path/to/project --force
```

Do not copy a live SQLite file directly because committed state may still be
in its WAL. The backup command creates a validated standalone snapshot.
Restore validates and stages the snapshot, requires explicit `--force`, and
atomically replaces live state only while no other Structurely process is
using it.

Graph and content indexes detect SQLite corruption when opened for writing.
Structurely preserves the corrupt database and sidecars in a timestamped
recovery directory, rebuilds the derived index, and reports the recovery in
status and sync output. Retain recovered databases when filing a bug only when
they contain no sensitive data you are unable to share.
