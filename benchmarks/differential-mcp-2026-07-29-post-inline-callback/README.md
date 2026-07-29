# MCP differential after inline callbacks — 2026-07-29

At clean Structurely code commit
`2ce5c888e82d522c452245f453f2aa15013a4162`:

- Structurely and pinned CodeGraph pass 22/22 shared compatibility predicates.
- Both score 1.0000 for context usefulness, required-fact recall,
  relevant-file recall, and file precision.
- Structurely uses 2,459 response characters; CodeGraph uses 2,532.
- Structurely passes the separately scored Harmony emitter extension 1/1.
- Normalized Structurely output is byte-identical to the pre-optimization
  inline-callback run; normalized CodeGraph output is byte-identical to the
  pinned baseline.

Structurely binary SHA-256:
`a15b3fc4222fa16bb1aa9f6113e1b61f9525474e4713fb8b50780ae6a9262f4a`.
Raw result SHA-256:
`50a4d15bb7064ec323fd410b28645d26c2acf7344699d78e7967e1182508bf6c`.
