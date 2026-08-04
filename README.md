# Structurely

Structurely gives coding agents a local semantic map of your codebase. It
indexes symbols, calls, routes, callbacks, UI flows, and framework relationships
into a transactional SQLite graph. It also indexes useful repository content
into bounded chunks and stores agent sessions, recaps, and memory locally.
Your source code stays on your machine.

Structurely is a native Rust binary with its own CLI and MCP tools.

## Install Structurely

macOS and Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/coder-company/structurely/main/scripts/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/coder-company/structurely/main/install.ps1 | iex
```

Confirm the installation:

```bash
structurely --version
```

The installer detects your platform, downloads the latest native release,
verifies its SHA-256 checksum, smoke-tests it, and publishes it atomically for
your user. Rerun the same command to upgrade or repair an installation; a
failed download or checksum never replaces the working binary. You do not
need Rust or administrator access. Set
`STRUCTURELY_VERSION=v0.5.0` before the command to install a specific release.

## Set up your project

```bash
cd /path/to/project
structurely setup codex
structurely doctor --client codex
```

Use `claude` or `cursor` instead of `codex` when needed. This one command
indexes the project, starts the background indexer, installs the project-local
MCP entry, and verifies that everything is ready.
`doctor` returns one machine-readable health report for the index, freshness
daemon, coding-agent integration, and optional dashboard.

Interactive setup also offers an optional private dashboard. Deploy only its
static shell to Vercel or Cloudflare Pages, or run it entirely on localhost:

```bash
structurely dashboard serve --path .
```

The browser pairs directly with a token-protected loopback bridge. Source,
indexes, queries, sessions, recaps, and memory never pass through the hosting
provider. See the [private dashboard guide](docs/dashboard.md).

```bash
structurely explore "authentication flow"
structurely research "how releases are verified"
```

## Replace CodeGraph

From a project that uses CodeGraph:

```bash
structurely setup codex --replace-codegraph
```

This replaces only the existing `codegraph` MCP server entry while preserving
unrelated agent settings. It builds a new Structurely graph because the database
formats are intentionally different. It does not delete CodeGraph's binary,
configuration, or index.

Restart your coding agent after setup. Structurely provides graph search,
repository research, callers, callees, impact analysis, path tracing, durable
sessions, recaps, memory, and team workspace namespaces over MCP.

To configure another MCP client manually, run:

```bash
structurely serve --mcp --path /absolute/path/to/project
```

Structurely advertises only `structurely_explore` by default. Set
`STRUCTURELY_MCP_TOOLS` to advertise more tools:

```bash
STRUCTURELY_MCP_TOOLS=explore,research,trace,session,memory,workspace \
  structurely serve --mcp --path /path/to/project
```

## Keep agent work across sessions

Create a workspace, record important work, and generate a deterministic recap:

```bash
structurely workspace create "Compiler team"
structurely session start <workspace-id> "Harden atomic publication"
structurely session add <session-id> decision "Keep rename and fsync in one seam."
structurely recap <session-id>
structurely memory remember <workspace-id> \
  "Atomic publication is implemented in src/atomic_file.rs." \
  --tags architecture,storage
```

Commands return JSON for reliable agent and script consumption. Interactive
`setup` and `doctor` runs use a concise terminal summary; pass `--json` to
either command to force the machine-readable report. Workspace state is
project-local and survives index rebuilds. Use
`structurely trace <source> <target>` for a bounded,
evidence-backed relationship path and `structurely impact <symbol>` before
changing a shared symbol.

## Benchmarks against CodeGraph

This pinned July 29 run used the same 441-file corpus for Structurely commit
`bc708fc` and CodeGraph 1.5.0 commit `572d22b`.

| Metric | Structurely | CodeGraph 1.5.0 | Result |
|---|---:|---:|---:|
| Fresh indexing p50 | 2.796 s | 6.640 s | 2.375× faster |
| Query-process p50 | 28.836 ms | 303.848 ms | 10.537× faster |
| Peak indexing memory | 156.0 MiB | 1,012.8 MiB | 84.60% lower |
| Database size | 40.47 MiB | 42.39 MiB | 4.54% smaller |
| Targeted MCP checks | 25/25 | 25/25 | parity |

These results describe this pinned corpus and protocol, not every repository or
feature. Query timing includes process startup. Review the
[raw results and methodology](benchmarks/release-hardening-2026-07-29/README.md)
and [comparison scope](docs/codegraph-parity.md).

## Benchmarks against Perseus

This local-versus-hosted comparison used the same clean 96-file Structurely
snapshot with Structurely 0.2.0 and Perseus 0.1.196.

| Metric | Structurely | Perseus | Result |
|---|---:|---:|---:|
| Clean index wall p50 | 0.58 s | 10.38 s | 17.90× faster |
| Warm query-process wall p50 | 12.51 ms | 1,660 ms | 132.73× faster |
| Expected file ranked first | 3/5 | 3/5 | tie |
| Expected file within top 10 | 5/5 | 4/5 | Structurely +1 |

Perseus performs work on its hosted service, while Structurely runs locally.
The table compares end-to-end CLI wait, not server resource consumption. See
the [full Perseus protocol and results](benchmarks/perseus-2026-07-29/README.md).
The checked-in [Perseus acceptance gate](docs/acceptance.md#verify-the-perseus-advantage)
also enforces repository-wide content coverage, chunk retrieval, workflow
availability, and the expected first-place result for “atomic file
publication.” The current gate ranks the expected file first for 4/5 queries
and within the top ten for 5/5, beating the pinned Perseus result of 3/5 and
4/5 respectively.

## Supported languages

Structurely indexes TypeScript, TSX, JavaScript, JSX, Vue, Svelte, Astro,
ArkTS, Python, Rust, Go, Java, C#, C, C++, Dart, Ruby, PHP, Swift, Lua, Kotlin,
Scala, and R.

It also resolves selected framework behavior for React, Express, React Router,
FastAPI, Django, Django REST Framework, NestJS, Vue, Svelte, Astro, ArkUI, and
OpenHarmony. The [compatibility matrix](docs/codegraph-parity.md) states the
tested scope and intentional limits.

## Read the documentation

- [Get command syntax and examples](docs/cli-reference.md)
- [Configure files and indexing](docs/configuration.md)
- [Install, operate, and troubleshoot Structurely](docs/operations.md)
- [Run or deploy the private dashboard](docs/dashboard.md)
- [Connect through MCP](docs/compatibility.md)
- [Understand the architecture](docs/architecture.md)
- [Review release verification](docs/releases.md)
- [Reproduce acceptance tests](docs/acceptance.md)
- [Contribute](CONTRIBUTING.md)
- [Report a security issue](SECURITY.md)

## Develop Structurely

```bash
cargo fmt --check
cargo test --all-targets --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --locked
```

Structurely is available under the [MIT License](LICENSE). Structurely is not
affiliated with CodeGraph.
