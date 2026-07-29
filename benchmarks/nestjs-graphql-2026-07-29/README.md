# NestJS GraphQL acceptance — 2026-07-29

Clean Structurely commit `d9188d1` advances the graph model to v56 and adds
import-proven NestJS GraphQL resolver endpoints.

The adapter accepts exact named, aliased, or uniquely verified namespace
imports from `@nestjs/graphql`. A handler must belong to an import-proven
`@Resolver` class and use `Query`, `Mutation`, `Subscription`, `ResolveField`,
or `ResolveReference`. Root operation names come from exact literal
`options.name`, a leading literal string, or the handler name. Fields and
references require a proven parent from an exact string or simple synchronous
resolver arrow.

Each endpoint is a stable Route symbol with
`framework/nestjs-graphql` provenance, a file-containment relationship, and an
exact owning-class handler call. Dynamic names, ambiguous imports, invalid
GraphQL names, parentless fields/references, malformed arrows, and lexical
decorator shadows fail closed.

Independent review caught and closed five precision issues before merge:
idiomatic `@Resolver(of => User)` arrows, file-global rather than lexical
shadowing, valid namespace imports, JavaScript `$` names invalid in GraphQL,
and function-hoisted `var` shadows hidden in nested blocks.

On pinned Nest commit `fafe503`, a clean 1,727-file index materializes all 42
production resolver handlers across 13 files:

- 18 queries
- 11 mutations
- 6 subscriptions
- 5 resolved fields
- 2 federation references

The 42 route symbols produce 84 exact GraphQL relationships. The clean run
completes in 4.350 seconds wall time with 67,280 KiB peak RSS and persists
8,340 symbols and 31,307 relationships. The full gate passes 196 unit tests,
daemon and MCP integrations, strict Clippy, and independent review with no
remaining blockers.
