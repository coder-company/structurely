# CodeGraph compatibility contract

Structurely targets behavioral compatibility at the agent-facing seam, not
database-file compatibility. The current contract is audited against CodeGraph
1.5.0 source commit `572d22bfbe82602080e457bec655f72e3314f9ef`.

Initial compatibility commands:

- `structurely init [path]`
- `structurely sync [path]`
- `structurely status [path]`
- `structurely search <query> [--path <path>]`
- `structurely explore <query> [--path <path>]`
- `structurely serve --mcp [--path <path>]`

Initial MCP tools:

- `codegraph_search`
- `codegraph_explore`
- `codegraph_callers`
- `codegraph_callees`
- `codegraph_impact`
- `codegraph_status`
- `codegraph_files`
- `codegraph_node`

Like the pinned upstream version, `tools/list` advertises only
`codegraph_explore` by default. Set `CODEGRAPH_MCP_TOOLS` to a comma-separated
list such as `explore,node,search,callers` to advertise additional tools. All
eight handlers remain callable and available through the CLI.

Existing arguments and required response fields remain compatible. Structurely
may add `confidence`, `provenance`, and `explanation`. Contract fixtures will
normalize additive fields when comparing against a pinned CodeGraph release.
The server speaks newline-delimited JSON-RPC 2.0 over stdio, advertises bounded
integer arguments in its MCP schemas, returns standard protocol errors for
malformed requests, and reports tool execution failures through MCP `isError`
content without terminating the session.

The SQLite schema is explicitly not compatible. A future importer may read an
existing CodeGraph index, but Structurely owns its versioned graph model.
