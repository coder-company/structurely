# CodeGraph compatibility contract

Structurely targets behavioral compatibility at the agent-facing seam, not
database-file compatibility.

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

Existing arguments and required response fields remain compatible. Structurely
may add `confidence`, `provenance`, and `explanation`. Contract fixtures will
normalize additive fields when comparing against a pinned CodeGraph release.

The SQLite schema is explicitly not compatible. A future importer may read an
existing CodeGraph index, but Structurely owns its versioned graph model.

