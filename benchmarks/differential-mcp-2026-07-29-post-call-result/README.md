# MCP differential after call-result resolution — 2026-07-29

At clean Structurely commit
`8bce9ac6cb2042953bc477e9c57efdbe05bccad3`:

- Structurely and pinned CodeGraph pass 22/22 shared compatibility predicates.
- Both score 1.0000 for context usefulness, required-fact recall,
  relevant-file recall, and file precision.
- Structurely uses 2,459 response characters; CodeGraph uses 2,532.
- Structurely passes the separately scored Harmony emitter extension 1/1.
- The normalized pinned CodeGraph capture is byte-identical to the preceding
  accepted baseline.
- Structurely's fixture database grows from 221,184 to 233,472 bytes because
  the graph schema now stores explicit nominal return summaries.

The harness `baselineMatches` field remains false because it compares a whole
prior report to the current normalized capture; direct normalized-capture
comparison passed.

- Structurely binary SHA-256:
  `b2b0ade8ebbfa907fa25c7b3ae6622c2e6c79606c9f10986f4c0ccdd1cddb91b`
- Raw result SHA-256:
  `a2b483ba9a29cb16e0229363dffa1c251a8785f34ab982136fc06377ec46f349`
