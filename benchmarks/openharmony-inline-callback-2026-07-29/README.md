# OpenHarmony inline-callback acceptance — 2026-07-29

Structurely code commit `2ce5c888e82d522c452245f453f2aa15013a4162`
indexed exactly 6,995 ETS files from
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- Fresh index: 119.239 seconds
- Source-emitted symbols: 47,955
- Resolver-reported relationships: 89,099
- Database: 205,987,840 bytes
- Correlated assertions: 15/15
- Isolated gate peak RSS: 519,072 KiB
- `dynamic/callback-argument`: 68
- `dynamic/callback-delegation`: 4
- `dynamic/callback-inline`: 67

The new assertion proves a production cross-file, transitive flow:
`Index.test00` passes an inline closure at line 47, `ClientIpc.bindAbi`
forwards it, and `connectIpc` invokes the second formal. Querying
`connectIpc` returns
`Index.build.<callback test00 argument 1 #1>` with
`dynamic/callback-delegation` provenance and confidence 0.94.

Compared with the preceding named-delegation inventory, the graph adds 120
relationships and moves from five direct callback relationships to 139 total
callback relationships. The optimized implementation indexes this corpus
12.17% faster than that preceding gate. Its database is 5.98% larger because
19,433 callback observations, provisional identities, and rejected-call
fallback ownership remain durable for incremental correctness.

- Structurely binary SHA-256:
  `a15b3fc4222fa16bb1aa9f6113e1b61f9525474e4713fb8b50780ae6a9262f4a`
- Raw gate SHA-256:
  `350f2201651c24595ce7357ca675f3ecdce37e1fafc15b785e792a7b5a00f1f9`
- Callback inventory SHA-256:
  `ce6f0dbaa27a452a1f653cd9857d522a706feb5aea27ec458a1a0c81710cb626`
