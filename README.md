# Structurely

<p align="center">
  <img src="dashboard/favicon.svg" width="112" height="112" alt="Structurely logo">
</p>

<p align="center">
  <strong>Local code intelligence for coding agents.</strong><br>
  A native semantic graph, repository search, and durable project memory—without sending your source anywhere.
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#first-run">First run</a> ·
  <a href="#what-it-does">Capabilities</a> ·
  <a href="#measured-performance">Benchmarks</a> ·
  <a href="docs/dashboard.md">Dashboard</a> ·
  <a href="docs/cli-reference.md">CLI reference</a>
</p>

---

Structurely indexes symbols, calls, routes, callbacks, UI flows, and framework
relationships into a transactional SQLite graph. It also searches repository
content and keeps sessions, recaps, and memory beside the project.

One Rust binary. No hosted index. No source upload.

## Install

**macOS and Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/coder-company/structurely/main/scripts/install.sh | sh
```

**Windows PowerShell**

```powershell
irm https://raw.githubusercontent.com/coder-company/structurely/main/install.ps1 | iex
```

The installer selects the native archive, verifies its SHA-256 checksum,
smoke-tests the binary, and publishes it atomically. It needs neither Rust nor
administrator access. Pin a release with `STRUCTURELY_VERSION=v0.5.0`.

## First run

```bash
cd /path/to/project
structurely setup codex
structurely doctor --client codex
```

Use `claude` or `cursor` instead of `codex` when needed. Setup indexes the
project, starts the background indexer, installs the project-local MCP entry,
and verifies it. Then ask something useful:

```bash
structurely explore "authentication flow"
structurely research "how are releases verified?"
structurely impact publish
```

### Private dashboard

```bash
structurely dashboard serve --path .
```

Open the printed loopback URL and enter its one-time pairing code. The browser
talks directly to the local bridge; repository data, queries, sessions, and
memory do not pass through a hosting provider. Read the
[dashboard guide](docs/dashboard.md) for local and static-shell deployment.

## What it does

| Surface | What you get |
|---|---|
| Graph | Symbols, definitions, references, callers, callees, routes, callbacks, and framework flows |
| Research | Ranked code and repository-content evidence with source locations |
| Change planning | Bounded impact analysis and evidence-backed symbol-to-symbol paths |
| Continuity | Project-local workspaces, sessions, recaps, and searchable memory |
| Agent access | CLI plus MCP contracts for Codex, Claude Code, Cursor, and custom clients |
| Operations | Incremental indexing, background freshness, health checks, backup, and recovery |
| Privacy | Loopback dashboard, local SQLite state, bounded outputs, and no cloud synchronization |

### Supported code

TypeScript, TSX, JavaScript, JSX, Vue, Svelte, Astro, ArkTS, Python, Rust,
Go, Java, C#, C, C++, Dart, Ruby, PHP, Swift, Lua, Kotlin, Scala, and R.

Structurely also resolves tested behavior for React, Express, React Router,
FastAPI, Django, Django REST Framework, NestJS, Vue, Svelte, Astro, ArkUI, and
OpenHarmony. See the [compatibility matrix](docs/codegraph-parity.md) for the
exact contract and intentional limits.

## Replace CodeGraph

```bash
structurely setup codex --replace-codegraph
```

This replaces only the project’s `codegraph` MCP entry. It preserves unrelated
agent settings and does not delete the CodeGraph binary, configuration, or
index. Structurely builds its own graph because the database formats differ.

For a manual MCP connection:

```bash
structurely serve --mcp --path /absolute/path/to/project
```

Only `structurely_explore` is advertised by default. Opt into more tools when
the client benefits from them:

```bash
STRUCTURELY_MCP_TOOLS=explore,research,trace,session,memory,workspace \
  structurely serve --mcp --path /path/to/project
```

## Keep the work

```bash
structurely workspace create "Compiler team"
structurely session start <workspace-id> "Harden atomic publication"
structurely session add <session-id> decision "Keep rename and fsync in one seam."
structurely recap <session-id>
structurely memory remember <workspace-id> \
  "Atomic publication is implemented in src/atomic_file.rs." \
  --tags architecture,storage
```

Commands return JSON for agents and scripts. Interactive `setup` and `doctor`
use a compact terminal view; pass `--json` for the full machine-readable
report. Durable workspace state survives index rebuilds.

## Measured performance

### Against CodeGraph 1.5.0

Pinned July 29 run, same 441-file corpus:

| Metric | Structurely | CodeGraph | Difference |
|---|---:|---:|---:|
| Fresh index p50 | 2.796 s | 6.640 s | **2.375× faster** |
| Query process p50 | 28.836 ms | 303.848 ms | **10.537× faster** |
| Peak index memory | 156.0 MiB | 1,012.8 MiB | **84.60% lower** |
| Database | 40.47 MiB | 42.39 MiB | **4.54% smaller** |
| Targeted MCP checks | 25/25 | 25/25 | parity |

[Methodology and raw artifacts](benchmarks/release-hardening-2026-07-29/README.md) ·
[comparison scope](docs/codegraph-parity.md)

### Against Perseus 0.1.196

Pinned clean 96-file Structurely snapshot:

| Metric | Structurely | Perseus | Difference |
|---|---:|---:|---:|
| Clean index wall p50 | 0.58 s | 10.38 s | **17.90× faster** |
| Warm query wall p50 | 12.51 ms | 1,660 ms | **132.73× faster** |
| Expected file at rank one | 3/5 | 3/5 | tie |
| Expected file in top ten | 5/5 | 4/5 | **+1** |

These are pinned-corpus measurements, not universal claims. Query timings
include process startup; Perseus performs work on its hosted service while
Structurely runs locally. Read the [full protocol](benchmarks/perseus-2026-07-29/README.md)
or the current [acceptance gate](docs/acceptance.md#verify-the-perseus-advantage),
which requires Structurely to beat the pinned baseline.

## Documentation

| Need | Guide |
|---|---|
| Commands and examples | [CLI reference](docs/cli-reference.md) |
| Files, extensions, and exclusions | [Configuration](docs/configuration.md) |
| Installation and troubleshooting | [Operations](docs/operations.md) |
| Private browser console | [Dashboard](docs/dashboard.md) |
| MCP clients and tool contracts | [Compatibility](docs/compatibility.md) |
| Storage and system design | [Architecture](docs/architecture.md) |
| Packaging and provenance | [Releases](docs/releases.md) |
| Executable quality gates | [Acceptance](docs/acceptance.md) |

## Development

```bash
cargo fmt --check
cargo test --all-targets --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --locked
```

[Contributing](CONTRIBUTING.md) · [Security](SECURITY.md) · [MIT License](LICENSE)

Structurely is not affiliated with CodeGraph.
