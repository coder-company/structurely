# MCP interface

Structurely owns its agent-facing interface. Its behavior and schemas are
benchmarked against CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`, but its tools use Structurely
names.

The complete CLI surface is documented in the
[CLI reference](cli-reference.md). Compatibility-sensitive graph commands
include `search`, `explore`, `callers`, `callees`, `impact`, `status`, and
`serve --mcp`; Structurely-native workflows add `research`, `trace`,
`workspace`, `session`, `recap`, and `memory`.

MCP tools:

- `structurely_search`
- `structurely_explore`
- `structurely_research`
- `structurely_callers`
- `structurely_callees`
- `structurely_impact`
- `structurely_trace`
- `structurely_status`
- `structurely_files`
- `structurely_node`
- `structurely_workspace`
- `structurely_session`
- `structurely_memory`

`tools/list` advertises only `structurely_explore` by default. Set
`STRUCTURELY_MCP_TOOLS` to a comma-separated list such as
`explore,node,search,callers,research,trace` to advertise additional tools. All
13 handlers remain callable even when they are not advertised by default.

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
