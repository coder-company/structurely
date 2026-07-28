# CodeGraph 1.5.0 parity matrix

This matrix audits Structurely against CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`. “Compatible” requires executable
behavioral evidence; matching a command or tool name is not enough.

| Capability | Status | Current evidence | Required parity evidence |
|---|---|---|---|
| Stable graph epochs and rollback | Superior | deterministic snapshots and injected rollback tests | preserve while expanding every resolver |
| Index/query performance | Superior | pinned 441-file benchmark | rerun after each major semantic layer |
| Core CLI/MCP names and schemas | Partial | eight handlers and pinned contract fixture | differential output, error, ambiguity, and pagination suites |
| Explore context usefulness | Behind | basic search plus caller/callee grouping | global output budget, ranking, flow spine, completeness and freshness signals |
| Project config and custom extensions | Partial | `structurely.json`/`codegraph.json`, extension and exclude tests | include, includeIgnored, precedence and malformed-entry suites |
| TS/JS path aliases | Missing | raw module suffix matching only | JSONC baseUrl/paths, wildcard priority, multiple targets and escape protection |
| Workspace/package resolution | Missing | no manifest context | npm/pnpm/bun, Cargo and Go workspace fixtures |
| Nested repositories and worktrees | Missing | generic ignore walker | embedded repo, submodule, worktree and mismatch suites |
| Framework resolvers | Missing | route/component kinds have no producers | adversarial adapters plus two representative repositories per adapter |
| Callbacks and dynamic dispatch | Missing | direct AST calls and receiver hints only | function-reference facts, registration synthesis, fanout caps and provenance |
| Language/dialect breadth | Behind | 18 language dialects | prioritized missing grammars and kernel-parity fixtures |
| Daemon and shared live index | Missing | foreground watcher and sync-per-tool MCP | shared process, locks, watchdogs, fallback, catch-up and staleness suites |
| Installers and agent integrations | Behind | native scripts and manual client config | idempotent install/uninstall for major coding-agent clients |

## Delivery order

1. Deep project inventory and project-resolution context.
2. TypeScript aliases, workspace packages, and canonical import targets.
3. Budgeted and freshness-honest Explore/Node output.
4. Function references, callbacks, and dynamic dispatch.
5. Express/React Router and Django/FastAPI resolver adapters.
6. Remaining framework and language adapters prioritized by real repositories.
7. Shared daemon, project discovery, installers, and operational hardening.

The matrix remains intentionally conservative. A capability moves to
“compatible” only when a fixture exercises the behavior and a pinned
differential or independently specified acceptance check passes.
