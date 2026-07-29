# Structurely architecture

Structurely is a local-first semantic code intelligence engine. The Rust
process owns the complete update path:

```text
filesystem scan
    -> language parser
    -> versioned facts
    -> stable symbol identity
    -> relationship resolution
    -> one SQLite transaction
    -> graph epoch
```

The CLI and MCP compatibility surface call the same engine interface. SQLite
is the durable source of truth; WAL mode permits concurrent read snapshots
while one writer publishes an update.

The connection checkpoints after every published epoch, auto-checkpoints after
256 WAL pages, applies a 16 MiB journal size limit, and waits up to five seconds
for a busy writer. `structurely status` reports database and WAL bytes plus the
configured checkpoint limits so growth is observable without opening SQLite.

Individual source files larger than 1 MiB are excluded before reading or
parsing, matching the pinned CodeGraph safety limit for vendored/generated
artifacts. Sync and status reports expose the skipped-file count. If a
previously indexed file grows beyond the limit, the next atomic epoch removes
its stale graph facts.

Invalid UTF-8 bytes are replaced byte-for-byte with ASCII spaces while line
breaks are preserved. This lets indexing continue across legacy source files
without shifting parser byte offsets, line evidence, or incremental hashes.

## Invariants

1. A reader observes exactly one committed graph epoch.
2. A symbol's public ID does not depend on its line or byte position.
3. Every inferred relationship has provenance, confidence, location, and an
   explanation.
4. Re-indexing a file replaces all file-owned facts atomically.
5. Incremental and clean indexing of the same source produce the same graph.
6. Storage row IDs are never public symbol identities.
7. Parser failure for one file cannot corrupt the previous committed graph.

Call resolution ranks receiver-type evidence ahead of same-file, explicit
import, and language-wide candidates. Locally constructed receivers in
TypeScript/JavaScript, Java, Python, and Rust therefore select their class
method even when another class in the same file has the same method name; the
emitted edge records the winning scope and confidence. Constructor inference
supports `new Type()`, Python `Type()` assignments, declared Java-style local
types, and Rust `Type::new()` values.

Semantic extraction has a separate adapter seam after language parsing.
Adapters consume the syntax tree and append ordinary symbols, relationships,
and pending relationships to the same file-local Fact set. The current
adapters cover named JavaScript callback registrations, Express routes,
FastAPI decorators, NestJS HTTP controllers, and literal event
registration/dispatch. React runtime adapters bridge state updates to class
renders and capitalized JSX children to their component definitions. A bounded
post-resolution adapter connects interface methods to directly declared
concrete implementations. Vue and Svelte files use offset-preserving embedded
script views plus bounded template adapters for component rendering and event
handlers. This keeps
framework policy out of the storage Module while letting every adapter reuse
import scope, alias resolution, evidence, atomic publication, and graph
traversal.

ArkTS uses its native grammar. Bounded ArkUI adapters connect `@Component` and
`@ComponentV2` render trees, direct `this.<member>` event handlers, and
mutations of decorated reactive fields to `build`. Intrinsic ArkUI DSL calls
are pruned only when they do not collide with a local or imported project
symbol. Harmony `oh-package.json5` `file:` dependencies use bounded discovery,
reject lexical and symlink escapes, drop names mapped to multiple directories,
and honor a target module's declared `main`; registry dependencies stay
external. Literal ArkUI `pushUrl` and `replaceUrl` targets resolve across the
owning Harmony module only when the receiver has a verified `@ohos.router` or
`@kit.ArkUI` import and the normalized path identifies exactly one `@Entry`
symbol. Dynamic paths, lexical router shadows, traversal, non-entry pages, and
ambiguous page files fail closed. Same-file `@Extend(Intrinsic)` style helpers
resolve only when the ArkUI chain root matches the declared intrinsic;
component-owned `@Styles` methods resolve only from the same component.
Undecorated, wrong-intrinsic, cross-component, and ambiguous candidates fail
closed.

Harmony emitter analysis requires a verified default or alias import from
`@ohos.events.emitter`, or a verified named `emitter` import from
`@kit.BasicServicesKit`. String, integer, single-`eventId` object, and unique
same-file immutable descriptor channels connect `on`/`once` registrations to
`emit` calls inside the longest matching Harmony application root. Immutable
exported literals and single-`eventId` descriptors resolve through verified
named imports; exported `static readonly` literal members resolve without
evaluating arbitrary expressions. Named and star barrel exports propagate at
most sixteen hops, reject cycles, and require all candidates to converge to one
canonical value. Numeric and string channel identities remain distinct;
dynamic IDs, mutable exports, reassignment, lexical shadows, ambiguous
registrations or exports, and cross-application matches fail closed. Named and
inline callbacks are supported. Constructor-built descriptors,
transitive callback delegation, and remaining cross-language emitter channels
are intentionally not yet resolved.

Direct callback-argument propagation records exact formal and actual argument
positions. It emits an edge only after the ordinary call resolves to one
callee, that exact formal is invoked, and the corresponding identifier,
verified import, or `this.member` actual resolves uniquely. Invocations inside
nested closures retain the outer formal unless an inner formal shadows it.
Inline closures, default/rest/destructured parameters, computed members,
parameter forwarding, ambiguous overloads, and stored or returned callbacks
fail closed. Call-site observations are persisted as compact per-file batches
and resolved in bulk.

ArkTS calls through imported singleton values prefer a unique candidate inside
the caller's Harmony project root before language-wide fallback. This rank
requires an actual import binding and applies only across `entry`, `feature`,
and `features` layouts. Multiple same-project candidates remain ambiguous.

Decorated component-owned `@Builder` methods passed through `bindPopup`
options resolve only when the target is an exact `this.member` on the same
component. The ArkTS resolver adapter handles both native modifier chains and
the grammar's recovered sibling form for children-bearing components. Recovered
chains must begin at an ArkUI component expression and contain contiguous
leading-dot/parenthesized-argument pairs; orphan, interrupted, undecorated, and
cross-component candidates fail closed.

The project-aware BuilderParam flow module persists decorated Builder
declarations, BuilderParam declarations, assignments, and consumer invocations
as compact per-file Facts, then resolves them after verified import bindings.
Object-pair and trailing-child syntax are separate resolver adapters over this
seam. Object pairs support exact same-owner `this.member` values and verified
imported bare decorated Builders. Exact inline arrow and function-expression
values receive synthetic adapter Symbols, including the ArkTS grammar's
contiguous recovered-sibling form inside ArkUI children. Trailing children
receive stable synthetic Symbols and resolve only when the uniquely imported or
local component declares exactly one BuilderParam. Every synthetic Symbol and
its containment, registration, and consumer-dispatch relationships are
materialized together in the graph transaction only after that proof; rejected
observations leave no searchable Symbol or dangling relationship. The module
emits registration and consumer dispatch relationships with separate
provenance. Undeclared keys, undecorated targets, missing or ambiguous imports,
multiple possible trailing parameters, computed members, and ambiguous
components fail closed. Cross-file `this.member` forwarding, declaration
defaults, and deferred runtime assignments remain outside this bounded layer.

Direct calls and registrations carry their own provenance, confidence, and
explanation through one resolution path. Literal event dispatch joins only
registrations with the same file-local receiver and channel, refuses dynamic
channel expressions, and emits no inferred dispatch edge above a fanout of
six. Invocations of a lexically shadowing callable parameter are retained as
dynamic Observations but deliberately do not bind to an unrelated global
Symbol.

## Modules

- `model` owns the versioned graph vocabulary and stable identity algorithm.
- `parser` converts supported source text into file-local facts.
- `semantic` owns pure callback and framework Resolver adapters.
- `store` owns schema migration, transactions, search, and graph epochs.
- `engine` owns scan, incremental invalidation, resolution, and publication.
- `mcp` adapts JSON-RPC/MCP requests to the engine.
- `main` adapts command-line commands to the same engine.

These are deliberately deep modules: callers use a small interface while
parsing, transaction ordering, schema details, and compatibility behavior stay
local to their implementations.
