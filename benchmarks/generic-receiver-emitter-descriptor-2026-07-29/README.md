# Generic receiver and constructor-descriptor acceptance — 2026-07-29

Clean Structurely commit `26737b7` advances the graph model to v51 and adds
two bounded semantic slices:

- outer simple generic receiver annotations such as
  `LRUCache<K, V> | null` resolve through the exact `LRUCache` nominal type;
- top-level same-file descriptor classes whose sole constructor statement is
  `this.eventId = parameter` canonicalize direct constructions and
  mutation-free one-hop `const` or `let` bindings.

The full Rust gate passes 169 unit tests, the daemon and MCP integrations, and
strict Clippy across every target and feature. Adversarial fixtures reject
qualified/intersection/conditional generic types, malformed generics, nested
descriptor classes, extra constructor work, extra parameters, unknown
arguments, reassignment, property mutation, and constructor shadowing.

On the pinned 441-file CodeGraph source intersection, one clean acceptance run
took 3.520 seconds wall time, used 142,860 KiB peak RSS, and produced 5,213
persisted symbols and 38,522 relationships in a 38,756,352-byte database.
Compared with the accepted pre-slice snapshot, exact `LRUCache` relationships
increase from 64 to 98: 34 real call sites move from fallback confidence to
exact 0.995 receiver resolution.

On OpenHarmony commit `a826ab0`, a clean 6,995-ETS-file run took 108.750
seconds wall time and 232,740 KiB peak RSS. The two real OrangeShopping
`EventsId` implementations now yield eight canonical event facts: four
registrations and four dispatches over the exact imported `DIALOG_EVENT_ID`
and `ADD_EVENT_ID` identities. The two inline registrations materialize exact
0.97 callback relationships. The two dialog callbacks are formal parameters,
and dispatch-to-inline-callback composition remains a separate bounded-flow
gap; this gate does not overclaim those downstream edges.

For comparison, the accepted five-trial CodeGraph 1.5.0 baseline remains
7.778 seconds index p50, 317.904 ms query p50, 354.738 ms query p95,
1,018,656 KiB peak RSS, and a 44,453,888-byte database. The present one-run
acceptance is a semantic regression gate, not a replacement for that
five-trial performance protocol.
