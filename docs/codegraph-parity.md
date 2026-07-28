# CodeGraph 1.5.0 parity matrix

This matrix audits Structurely against CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`. “Compatible” requires executable
behavioral evidence; matching a command or tool name is not enough.

| Capability | Status | Current evidence | Required parity evidence |
|---|---|---|---|
| Stable graph epochs and rollback | Superior | deterministic snapshots and injected rollback tests | preserve while expanding every resolver |
| Index/query performance | Superior | pinned 441-file benchmark | rerun after each major semantic layer |
| Core CLI/MCP names and schemas | Compatible (core) | eight handlers, pinned contract fixture, persistent-stdio binary test, and 14-scenario live differential run against pinned CodeGraph | pagination and cross-platform suites |
| Explore context usefulness | Compatible (core) | global/per-symbol budgets, exact/corroborated ranking, line-numbered excerpts, omission/staleness disclosure, and a pinned differential flow scoring 1.0000 versus CodeGraph 0.9583 | broaden usefulness queries and add blinded large-repo evaluation |
| Project config and custom extensions | Compatible (core) | `structurely.json`/`codegraph.json`; extension, exclude, include, includeIgnored, precedence and malformed-config tests | differential configuration fixture |
| TS/JS path aliases | Compatible (core) | JSONC baseUrl/paths, wildcard specificity, target fallback, index/extension canonicalization and escape tests | representative real-repo evidence |
| Workspace/package resolution | Compatible (core) | npm/yarn/pnpm workspaces, scoped packages, entrypoints/subpaths, Cargo crates, and Go workspaces | Bun workspace fixture and representative monorepo gates |
| Nested repositories and worktrees | Compatible (core) | opted-in embedded repositories and submodule gitdirs are indexed; linked worktree gitdirs are skipped as duplicate views; worktree roots require and support independent local indexes | real-git cross-platform subprocess coverage |
| Framework resolvers | Partial | Express, FastAPI, React Router JSX/object routes, Django `path`/`re_path`/legacy `url`, DRF viewsets, and NestJS HTTP controllers; adversarial fixtures; five pinned real-repository acceptance runs | GraphQL, NestJS microservices/websockets, prioritized remaining adapters, and a second real repository per adapter |
| Callbacks and dynamic dispatch | Partial | named callbacks; initializer/object/array/assignment references; strict import/local resolution; literal events, fanout cap, incremental cleanup, provenance and shadowing tests | broader member/type dispatch and differential fixtures |
| Language/dialect breadth | Behind | 19 language dialects, including core Dart class/function/method/call extraction | prioritize Vue/Svelte component dialects, Objective-C, and kernel-parity fixtures |
| Daemon and shared live index | Compatible (core) | spawned-process start/status/catch-up/stop test; exclusive project lock; failure release/restart; MCP daemon/foreground fallback metadata | cross-platform CI evidence and sustained fault-injection soak |
| Installers and agent integrations | Compatible (core) | idempotent project-scoped Codex, Claude Code and Cursor install/status/uninstall tests preserve unrelated TOML/JSON | executable client discovery smoke tests on release artifacts |

The live agent-seam differential gate is `scripts/differential_mcp.py`; the
representative repository gate is `scripts/acceptance_repositories.py`.
Checked-in results remain evidence for their exact pinned revisions, not a
claim that every framework pattern is supported.

## Delivery order

1. Deep project inventory and project-resolution context.
2. TypeScript aliases, workspace packages, and canonical import targets.
3. Budgeted and freshness-honest Explore/Node output.
4. Function references, callbacks, and dynamic dispatch. (named callbacks and
   literal events delivered; broader value-flow remains)
5. Express/React Router and Django/FastAPI resolver adapters. (core adapters
   delivered; broader framework-specific dispatch remains)
6. Remaining framework and language adapters prioritized by real repositories.
7. Shared daemon, project discovery, installers, and operational hardening.
   (core daemon and three project-scoped integrations delivered)

The matrix remains intentionally conservative. A capability moves to
“compatible” only when a fixture exercises the behavior and a pinned
differential or independently specified acceptance check passes.
