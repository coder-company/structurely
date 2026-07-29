# MCP interface

Structurely owns its agent-facing interface. Its behavior and schemas are
benchmarked against CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`, but its tools use Structurely
names.

CLI commands:

- `structurely init [path]`
- `structurely sync [path]`
- `structurely status [path]`
- `structurely search <query> [--path <path>]`
- `structurely explore <query> [--path <path>]`
- `structurely serve --mcp [--path <path>]`

MCP tools:

- `structurely_search`
- `structurely_explore`
- `structurely_callers`
- `structurely_callees`
- `structurely_impact`
- `structurely_status`
- `structurely_files`
- `structurely_node`

`tools/list` advertises only `structurely_explore` by default. Set
`STRUCTURELY_MCP_TOOLS` to a comma-separated list such as
`explore,node,search,callers` to advertise additional tools. All eight handlers
remain callable and available through the CLI.

Existing arguments and required response fields remain compatible. Structurely
may add `confidence`, `provenance`, and `explanation`. Contract fixtures will
normalize additive fields when comparing against a pinned CodeGraph release.
The server speaks newline-delimited JSON-RPC 2.0 over stdio, advertises bounded
integer arguments in its MCP schemas, returns standard protocol errors for
malformed requests, and reports tool execution failures through MCP `isError`
content without terminating the session.

Initialization negotiates MCP revisions `2024-11-05`, `2025-03-26`, and
`2025-06-18`. Unknown or omitted revisions fall back to `2024-11-05`.

Structurely owns its versioned SQLite graph model and does not import another
indexer's database.
