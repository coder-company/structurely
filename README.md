# Structurely

Structurely is a Rust-first, local semantic code intelligence engine for coding
agents. It indexes source into a transactional SQLite graph and exposes
CodeGraph-compatible CLI and MCP tools with additional relationship confidence,
provenance, and explanations.

Structurely 0.1 supports TypeScript, TSX, JavaScript, JSX, Python, Rust, Go,
Java, C#, C, C++, Ruby, PHP, Swift, Lua, Kotlin, Scala, and R. Its production
contract and reproducible evidence are listed in the
[acceptance gates](docs/acceptance.md).

## Build

```bash
cargo build --release
```

The repository pins its Rust toolchain through `rust-toolchain.toml`.

## Install

macOS and Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/coder-company/structurely/main/scripts/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/coder-company/structurely/main/install.ps1 | iex
```

The installers use `cargo install --locked` from the public Structurely
repository. Set `STRUCTURELY_VERSION` to a release tag to install a pinned
version. Native standalone archives, SHA-256 checksums, and build attestations
are attached to tagged GitHub Releases.

## Quick start

```bash
structurely init /path/to/project
structurely status /path/to/project
structurely search UserController --path /path/to/project
structurely explore authentication --path /path/to/project
structurely watch /path/to/project
```

Indexes are stored in `/path/to/project/.structurely/graph.db`. Source code and
graph data stay local.

`structurely watch` keeps the graph synchronized through native recursive
filesystem notifications. Changes are debounced into one transactional graph
epoch; press Ctrl-C for a graceful final flush and shutdown.

## MCP

Configure an MCP client to execute:

```bash
structurely serve --mcp --path /path/to/project
```

The current compatibility surface implements:

- `codegraph_search`
- `codegraph_explore`
- `codegraph_callers`
- `codegraph_callees`
- `codegraph_impact`
- `codegraph_status`
- `codegraph_files`
- `codegraph_node`

Only `codegraph_explore` is advertised to MCP clients by default, matching
CodeGraph 1.5.0. Set `CODEGRAPH_MCP_TOOLS=explore,node,search,callers` (or
another comma-separated selection) to advertise narrower tools.

Every relationship result includes its confidence, provenance, source location,
and explanation. Tool inputs are validated and collection sizes are bounded so
malformed or unexpectedly broad agent requests fail predictably.

## Design

- [Architecture and invariants](docs/architecture.md)
- [CodeGraph compatibility contract](docs/compatibility.md)
- [Production acceptance gates](docs/acceptance.md)
- [Reproducible benchmark protocol](docs/benchmarks.md)
- [Release and artifact verification](docs/releases.md)
- [Changelog](CHANGELOG.md)
- [Installation, privacy, and troubleshooting](docs/operations.md)
- [Domain language](CONTEXT.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)

## Development

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Structurely is not affiliated with CodeGraph. CodeGraph compatibility describes
the agent-facing protocol supported by Structurely.

## License

Structurely is available under the [MIT License](LICENSE).
