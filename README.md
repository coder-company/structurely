# Structurely

Structurely gives coding agents a local semantic map of your codebase. It
indexes symbols, calls, routes, callbacks, UI flows, and framework relationships
into a transactional SQLite graph. Your source code stays on your machine.

Structurely is written in Rust and exposes CodeGraph-compatible CLI and MCP
tools. On the pinned 441-file acceptance corpus, it matches all 25 shared
compatibility checks while indexing 2.37× faster, querying 10.54× faster, and
using 84.60% less peak memory than CodeGraph 1.5.0. See the
[reproducible results](benchmarks/release-hardening-2026-07-29/README.md).

## Install Structurely

You need Rust 1.88 or newer.

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

Tagged GitHub releases also provide standalone archives, SHA-256 checksums, and
build-provenance attestations.

## Index your first project

```bash
structurely init /path/to/project
structurely explore "authentication flow" --path /path/to/project
```

`init` creates `/path/to/project/.structurely/graph.db`. Add `.structurely/` to
the project's ignore file if your global Git configuration does not already
ignore it.

Use the background indexer to keep the graph current:

```bash
structurely daemon start --path /path/to/project
structurely daemon status --path /path/to/project
```

## Connect your coding agent

Structurely configures project-local MCP settings without changing unrelated
entries:

```bash
structurely integrations install codex --path /path/to/project
structurely integrations install claude --path /path/to/project
structurely integrations install cursor --path /path/to/project
```

Restart the agent after installation. To configure another MCP client manually,
run this command as a standard-input/standard-output server:

```bash
structurely serve --mcp --path /absolute/path/to/project
```

Structurely implements `codegraph_explore`, `codegraph_search`,
`codegraph_callers`, `codegraph_callees`, `codegraph_impact`,
`codegraph_status`, `codegraph_files`, and `codegraph_node`. It advertises only
`codegraph_explore` by default for CodeGraph compatibility. Set
`CODEGRAPH_MCP_TOOLS` to a comma-separated list to advertise more tools:

```bash
CODEGRAPH_MCP_TOOLS=explore,node,search,callers \
  structurely serve --mcp --path /path/to/project
```

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
