# Structurely domain language

- **Project** — a filesystem tree whose source is represented by one graph.
- **Graph epoch** — the committed generation visible to every reader. Readers
  never observe a partially updated epoch.
- **Symbol** — a declaration with a stable semantic identity independent of its
  current line number.
- **Semantic key** — the language, symbol kind, container, and qualified name
  used to derive a stable public symbol ID.
- **Fact** — parser or resolver evidence about a symbol or relationship.
- **Observation** — a file-local, unresolved Fact such as a call, import,
  registration, or event dispatch that still needs project context.
- **Pending relationship** — an Observation whose target is selected during
  graph resolution rather than extraction.
- **Registration** — an Observation that wires a named callable to a route,
  callback API, or literal dynamic-dispatch channel.
- **Relationship** — a directed, typed connection between two symbols.
- **Evidence** — provenance, confidence, source location, and explanation for a
  relationship.
- **Indexer** — the module that scans source files, extracts facts, resolves
  relationships, and atomically publishes a graph epoch.
- **Resolver adapter** — language or framework-specific relationship logic
  operating on extracted facts.
- **Compatibility surface** — the CodeGraph-compatible CLI commands and MCP
  tools presented to coding agents. It does not include CodeGraph's database
  format.
