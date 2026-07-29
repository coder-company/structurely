# NestJS transport acceptance — 2026-07-29

Clean Structurely commit `adff100` advances the graph model to v55 and adds
import-proven NestJS websocket, microservice, and gRPC endpoints.

The adapter accepts named or aliased decorators only from
`@nestjs/websockets` and `@nestjs/microservices`:
`SubscribeMessage`, `MessagePattern`, `EventPattern`, `GrpcMethod`, and
`GrpcStreamMethod`. Bounded literal primitives, shallow pattern objects, and
pattern arrays are canonicalized into stable Route symbols. Each endpoint
contains exact provenance and calls its owning class method through an exact
receiver type.

Dynamic, nested, spread, computed, ambiguous, oversized, lookalike, type-only,
namespace, shadowed, and malformed forms fail closed. Message and event
decorators support Nest’s valid one-to-three-argument overloads while using
only exact argument-zero pattern identity. gRPC defaults match runtime
behavior, including Unicode upper-first handler names and falsy empty-string
service/method fallback.

Independent review caught and closed four important issues before merge:
incorrect gRPC default casing, missing catch/destructuring shadows, route-ID
churn from unrelated same-pattern handlers, and rejected valid transport/extras
overloads. Empty-string gRPC defaults were also aligned with Nest runtime
semantics.

On pinned Nest commit `fafe503`, a clean 1,727-file index materializes all 101
inventoried production transport handlers across 32 files, plus two maintained
test handlers. The graph contains 103 route symbols: 21 websocket and 82
microservice/gRPC routes. Their file-containment and exact handler-call edges
produce 206 new framework relationships.

The clean run completes in 4.400 seconds wall time with 66,488 KiB peak RSS
and persists 8,298 symbols and 31,223 relationships. The full gate passes 191
unit tests, daemon and MCP integrations, strict Clippy, and independent review
with no remaining blockers.
