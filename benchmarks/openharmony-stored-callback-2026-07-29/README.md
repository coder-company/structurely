# OpenHarmony stored-callback acceptance — 2026-07-29

Clean Structurely commit `3a7ad31` advances the graph model to v52 and adds
bounded same-class stored callback propagation. The parser accepts a direct
callback formal assigned to an exact callable-typed `this.field` and invoked
through exact `this.field(...)`. It reuses the existing callback-argument
resolver and persistence model.

The pinned 6,995-file corpus contains 48 exact formal-to-field-to-invocation
shapes. Thirty-six satisfy the conservative single-assignment/no-escape
inventory rule; twelve contain clearing, reassignment, or ambiguity requiring
additional proof. The production analyzer supports separate lifecycle clears
but rejects assign-then-clear in the same method, unsafe initializers and
writes, aliases, returns, argument escapes, computed members, nested closure
access, `.call`/`.apply`/`.bind`, and augmented assignment. Work fails closed
at 64 fields, 64 methods, or 256 observations per class.

The full acceptance gate passes 173 unit tests, daemon and MCP integrations,
strict Clippy, and independent review. A clean OpenHarmony index completes in
108.350 seconds wall time with 233,028 KiB peak RSS and persists 48,248 symbols
and 124,718 relationships.

Two real DistributedRdb registrations now resolve at confidence 0.96:

- `RdbModel.onDataChangeDetail` reaches the inline callback supplied by
  `Index.aboutToAppear` at line 70.
- `RdbModel.onDataChange` reaches the inline callback supplied by
  `Index.getWant` at line 106.

The snapshot adds two callback-argument relationships and their two inline
callback identities while removing zero callback relationships. The 36 safe
lexical shapes are an inventory upper bound; only these two persisted,
end-to-end relationships are claimed as accepted semantic gain.
