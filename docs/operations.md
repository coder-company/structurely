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
STRUCTURELY_VERSION=v0.1.0 sh scripts/install.sh
```

```powershell
$env:STRUCTURELY_VERSION = "v0.1.0"
./install.ps1
```

Confirm the installed binary with `structurely --version`.

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
: Stop duplicate writers for the same project. One `watch` process should own
  synchronization; readers can continue through SQLite WAL snapshots.

Search results are stale
: Run `structurely sync /path/to/project`, or keep
  `structurely watch /path/to/project` running. `status` reports pending files.

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

## Backups and recovery

The source tree is authoritative; the graph is reproducible derived data. Back
up source control rather than the index. If an index is corrupt, stop watchers,
move the specific project's `.structurely` directory aside, and run
`structurely init /path/to/project`. Retain the moved database when filing a
bug if it contains no sensitive source-derived data you are unable to share.
