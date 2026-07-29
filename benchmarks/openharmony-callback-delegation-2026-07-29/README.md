# OpenHarmony callback-delegation inventory — 2026-07-29

Structurely commit `23def0279930028a9cbbe1498b9cd4c1885a099e`
indexed exactly 6,995 ETS files from
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- Fresh index: 135.767 seconds
- Source-emitted symbols: 47,955
- Resolver-reported relationships: 88,979
- Database: 194,359,296 bytes
- Existing correlated assertions: 14/14
- Isolated gate peak RSS: 519,204 KiB
- Callback-parameter invocations in the inspected graph: 133
- Callback-parameter delegation observations: 3,412
- Complete `dynamic/callback-delegation` relationships: 0

This is deliberately a negative inventory, not a production-acceptance claim
for the new relationship. The corpus contains many formal-to-formal forwarding
observations, but none joins a named concrete registration to a uniquely
resolved terminal consumer. The missing registrations are predominantly inline
closures, so inline callback identities remain the next measured semantic gap.
Synthetic, adversarial, incremental, rollback, migration, ambiguity, cycle,
branching, and depth-cap tests prove the bounded resolver itself.

Moving exact callsite targets from durable SQLite storage to a transaction-local
table recovered 9,007,104 bytes from the initial implementation run without
changing reported graph cardinality.

Structurely binary SHA-256:
`f9720aef8d3ed6818ca811beaec4c4724ee71ddd42591ae339ed1a1ecb50b1a4`.
Raw gate SHA-256:
`fe74eb8ecc8ee3394aee33ae22f55fbbef3223d1008615c31c414538633b0e6f`.
Delegation-count SHA-256:
`3c92be647bfd2e2932626480c191fee307a33c7a4f1ea4fb2dd5f1b723581f13`.
