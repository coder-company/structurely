# Structurely

Structurely is a Rust-first, local semantic code intelligence engine for coding
agents. It indexes source into a transactional SQLite graph and exposes
CodeGraph-compatible CLI and MCP tools with additional relationship confidence,
provenance, and explanations.

> Structurely is under active development. The current engine supports
> TypeScript, TSX, JavaScript, JSX, Python, and Rust. See
> [production acceptance gates](docs/acceptance.md) for the remaining scope.

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
version. Rust-free standalone release bundles are planned before the first
stable release.

## Quick start

```bash
structurely init /path/to/project
structurely status /path/to/project
structurely search UserController --path /path/to/project
structurely explore authentication --path /path/to/project
```

Indexes are stored in `/path/to/project/.structurely/graph.db`. Source code and
graph data stay local.

## MCP

Configure an MCP client to execute:

```bash
structurely serve --mcp --path /path/to/project
```

The current compatibility surface exposes:

- `codegraph_search`
- `codegraph_explore`
- `codegraph_callers`
- `codegraph_callees`

Every relationship result includes its confidence, provenance, source location,
and explanation.

## Design

- [Architecture and invariants](docs/architecture.md)
- [CodeGraph compatibility contract](docs/compatibility.md)
- [Production acceptance gates](docs/acceptance.md)
- [Domain language](CONTEXT.md)

## Development

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Structurely is not affiliated with CodeGraph. CodeGraph compatibility describes
the agent-facing protocol supported by Structurely.
