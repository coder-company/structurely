# Configure a project

Structurely works without a configuration file. Add `structurely.json` to the
project root when you need custom extensions or scan rules.

## Map a custom extension

```json
{
  "extensions": {
    ".view": "typescript"
  }
}
```

Extension names may contain ASCII letters and numbers. Language names are
case-insensitive. Supported values match the languages listed in the README;
`c#`, `c++`, `csharp`, and `cpp` aliases are accepted.

## Control indexed paths

Patterns use gitignore syntax and are relative to the project root:

```json
{
  "exclude": [
    "vendor/**",
    "generated/**"
  ],
  "include": [
    "generated/checked-in-api.ts"
  ],
  "includeIgnored": [
    "third_party/local-module/**"
  ]
}
```

- `exclude` removes matching paths.
- `include` adds matching paths that normal discovery omitted.
- `includeIgnored` opts an ignored embedded repository or submodule into the
  project graph.

Use `includeIgnored` only for source you trust and want represented in the same
graph. Structurely rejects lexical and symlink escapes outside the project.

## Understand precedence and errors

Structurely reads `structurely.json` first. It reads `codegraph.json` only when
`structurely.json` does not exist.

Configuration fails closed. Structurely stops indexing when JSON is malformed,
a field has the wrong type, a language is unknown, a glob is empty or invalid,
the file exceeds the read limit, or the file is a symbolic link. Fix the
reported error and run `structurely sync`.

## Configure runtime limits

Structurely bounds all public queries. Result limits range from 1 to 100,
impact depth ranges from 1 to 20, and benchmark iterations range from 1 to
1,000. Invalid values return an error instead of being silently reduced.

The indexer uses available CPU parallelism with a default maximum of eight
workers. Set `STRUCTURELY_PARSE_WORKERS` to a positive integer to change the
limit. Structurely caps the value at 16 and at the number of changed files.

Set `CODEGRAPH_MCP_TOOLS` to control which compatible MCP tools appear in
`tools/list`:

```bash
CODEGRAPH_MCP_TOOLS=explore,node,search,callers
```
