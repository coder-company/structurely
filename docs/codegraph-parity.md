# CodeGraph 1.5.0 parity matrix

This matrix audits Structurely against CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`. “Compatible” requires executable
behavioral evidence; matching a command or tool name is not enough.

| Capability | Status | Current evidence | Required parity evidence |
|---|---|---|---|
| Stable graph epochs and rollback | Superior | deterministic snapshots and injected rollback tests | preserve while expanding every resolver |
| Index/query performance | Superior | pinned 441-file benchmark | rerun after each major semantic layer |
| Core CLI/MCP names and schemas | Partial | eight handlers and pinned contract fixture | differential output, error, ambiguity, and pagination suites |
| Explore context usefulness | Partial | global/per-symbol budgets, exact/corroborated ranking, line-numbered excerpts, omission and stale-index disclosure | differential usefulness fixture, flow spine, large-repo evaluation |
| Project config and custom extensions | Compatible (core) | `structurely.json`/`codegraph.json`; extension, exclude, include, includeIgnored, precedence and malformed-config tests | differential configuration fixture |
| TS/JS path aliases | Compatible (core) | JSONC baseUrl/paths, wildcard specificity, target fallback, index/extension canonicalization and escape tests | representative real-repo evidence |
| Workspace/package resolution | Partial | npm/yarn array/object workspaces, scoped packages, entrypoints and subpaths | pnpm/bun, Cargo and Go workspace fixtures |
| Nested repositories and worktrees | Partial | embedded repo opt-in and built-in dependency/build safety tests | submodule, worktree and mismatch suites |
| Framework resolvers | Partial | Express, FastAPI, and React Router JSX/object route symbols; imported handlers/components; adversarial dynamic-path fixtures | prioritized remaining adapters and two real repositories per adapter |
| Callbacks and dynamic dispatch | Partial | named callbacks; initializer/object/array/assignment references; strict import/local resolution; literal events, fanout cap, incremental cleanup, provenance and shadowing tests | broader member/type dispatch and differential fixtures |
| Language/dialect breadth | Behind | 18 language dialects | prioritized missing grammars and kernel-parity fixtures |
| Daemon and shared live index | Compatible (core) | spawned-process start/status/catch-up/stop test; exclusive project lock; failure release/restart; MCP daemon/foreground fallback metadata | cross-platform CI evidence and sustained fault-injection soak |
| Installers and agent integrations | Compatible (core) | idempotent project-scoped Codex, Claude Code and Cursor install/status/uninstall tests preserve unrelated TOML/JSON | executable client discovery smoke tests on release artifacts |

## Delivery order

1. Deep project inventory and project-resolution context.
2. TypeScript aliases, workspace packages, and canonical import targets.
3. Budgeted and freshness-honest Explore/Node output.
4. Function references, callbacks, and dynamic dispatch. (named callbacks and
   literal events delivered; broader value-flow remains)
5. Express/React Router and Django/FastAPI resolver adapters. (Express,
   React Router, and FastAPI delivered; Django remains)
6. Remaining framework and language adapters prioritized by real repositories.
7. Shared daemon, project discovery, installers, and operational hardening.
   (core daemon and three project-scoped integrations delivered)

The matrix remains intentionally conservative. A capability moves to
“compatible” only when a fixture exercises the behavior and a pinned
differential or independently specified acceptance check passes.
