# CodeGraph 1.5.0 parity matrix

This matrix audits Structurely against CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`. “Compatible” requires executable
behavioral evidence; matching a command or tool name is not enough.

| Capability | Status | Current evidence | Required parity evidence |
|---|---|---|---|
| Stable graph epochs and rollback | Superior | deterministic snapshots and injected rollback tests | preserve while expanding every resolver |
| Index/query performance | Superior | current pinned 441-file v56 benchmark: 2.611× index, 9.491× query p50, 7.994× query p95, 87.50% less peak memory, 12.29% smaller database | rerun after each major semantic layer |
| Core CLI/MCP names and schemas | Compatible (core) | eight handlers, pinned contract fixture, persistent-stdio binary test, exact empty resource/template/prompt discovery probes, and current 25/25 live differential run against identity-verified pinned CodeGraph | pagination and cross-platform suites |
| Explore context usefulness | Compatible (core) | global/per-symbol budgets, exact/corroborated ranking, line-numbered excerpts, omission/staleness disclosure, and a pinned differential flow scoring 1.0000 for both engines | broaden usefulness queries and add blinded large-repo evaluation |
| Project config and custom extensions | Compatible (core) | `structurely.json`/`codegraph.json`; extension, exclude, include, includeIgnored, precedence and malformed-config tests | differential configuration fixture |
| TS/JS path aliases | Compatible (core) | JSONC baseUrl/paths, wildcard specificity, target fallback, index/extension canonicalization and escape tests | representative real-repo evidence |
| Workspace/package resolution | Compatible (core) | npm/yarn/pnpm workspaces, scoped packages, entrypoints/subpaths, Cargo crates, Go workspaces, and bounded Harmony ohpm `file:` dependencies with ambiguity/escape rejection, live differential, and pinned OpenHarmony evidence | Bun workspace fixture and broader representative monorepo gates |
| Nested repositories and worktrees | Compatible (core) | opted-in embedded repositories and submodule gitdirs are indexed; linked worktree gitdirs are skipped as duplicate views; worktree roots require and support independent local indexes | real-git cross-platform subprocess coverage |
| Framework resolvers | Partial | Express; all 53 deployed FastAPI endpoints and 49 exact dependency sites across pinned LightRAG and Graphiti, including ten endpoint-to-settings paths, with exact lexical/import/package-alias, immutable-path, `Annotated`, factory, nested-prefix, class-owned, evidence-site and incremental resolution; React Router JSX/object routes; Django `path`/`re_path`/legacy `url`; DRF viewsets; NestJS HTTP controllers plus import-proven websocket, message/event pattern, unary/streaming gRPC, and Query/Mutation/Subscription/ResolveField/ResolveReference GraphQL endpoints; and Vue/Svelte template children and events; adversarial fixtures and pinned real-repository gates | FastAPI `api_route(methods=...)`, dependency generator/yield semantics and function-local imports; non-Nest GraphQL ecosystems; broader Vue/Svelte directives; prioritized remaining adapters; and a second real repository per remaining adapter |
| Callbacks and dynamic dispatch | Partial | named callbacks; exact byte-correlated positional and Python keyword-name-to-formal propagation, bounded formal-to-formal delegation, and bounded same-class callable-field storage/invocation; C/C++ direct and typedef function-pointer fields, assignments, positional/designated tables, include-visible chained layout dispatch, bounded field-to-field fixed-point propagation, typedef-proven file-local bare arrays with indexed/cast entries, exact argument-to-formal stored-field flow, capped type-level may-call fanout, exact evidence sites, incremental cleanup, and pinned OpenHarmony player/libsamplerate evidence; accepted JS-family/ArkTS inline closure and Python lambda identities with nested ownership and rejected-call fallback; immediate TS/TSX/ArkTS call-result receivers through explicit simple nominal return annotations; scope-aware simple/nullable and outer-simple-generic receiver annotations, direct-new const arrow factories, exact Set/Array `for…of` elements, and bounded verified nearest-ancestor member lookup; initializer/object/array/assignment references; strict import/local/Harmony-project resolution; literal events; React `setState → render → JSX child`; bounded interface-to-implementation dispatch; app-scoped Harmony emitter channels with named/inline callbacks, immutable imported descriptors/member constants, exact top-level same-file constructor-built descriptors, bounded barrel forwarding, and real OpenHarmony, LightRAG, and Django evidence; fanout/depth/work caps, incremental cleanup, rollback, provenance, adversarial tests, and pinned differential checks | C/C++ pointer returns/local aliases, macro/conditional table construction and compilation-database include paths; qualified/Promise return flow; deeper call-result chains with measured project targets; cross-file inferred factories and descriptor getter/field flow; broader heap/alias callback flow; inline callable forms in remaining languages; broader collection/member/type dispatch; Flutter rebuilds; and remaining cross-language event channels |
| Language/dialect breadth | Behind | 23 language dialects; `.mts`/`.cts` TypeScript detection; Vue/Svelte component flows; offset-exact Astro frontmatter/scripts, imported template components and expressions, and `src/pages` routes; native ArkTS grammar; bounded ArkUI component/state/event/router/style-helper/emitter, component-owned popup `@Builder`, project-aware exact local/imported, inline-arrow/function, and trailing-child `@BuilderParam` registration plus consumer dispatch, and ohpm package flows; adversarial, incremental, live differential, and pinned real-repo gates | Objective-C, Liquid, Delphi, Luau, Astro alias/workspace component imports and renamed default-helper exports, cross-file-member/default/deferred ArkUI builder flows, and remaining CodeGraph dialects |
| Daemon and shared live index | Compatible (core) | spawned-process start/status/catch-up/stop test; exclusive project lock; failure release/restart; MCP daemon/foreground fallback metadata; durable same-directory atomic state replacement; state-publication failures stop the watcher and propagate | cross-platform CI evidence and sustained fault-injection soak |
| Installers and agent integrations | Compatible (core) | idempotent project-scoped Codex, Claude Code and Cursor install/status/uninstall tests preserve unrelated TOML/JSON; durable cross-platform atomic config replacement | executable client discovery smoke tests on release artifacts |

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
