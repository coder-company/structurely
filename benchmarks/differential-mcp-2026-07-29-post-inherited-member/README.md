# MCP differential after inherited-member resolution — 2026-07-29

At clean Structurely commit
`d28838f4ce6d49bb558536d50898e3f6b72ae1f0`:

- Structurely and pinned CodeGraph pass 22/22 shared compatibility predicates.
- Both score 1.0000 for context usefulness, required-fact recall,
  relevant-file recall, and file precision.
- Structurely uses 2,459 response characters; CodeGraph uses 2,532.
- Structurely passes the separately scored Harmony emitter extension 1/1.
- Both flow-spine checks, line-numbered source, and output-budget checks pass.
- The normalized pinned CodeGraph capture exactly matches the prior whole-report
  baseline (`baselineMatches: true`), and the gate exits zero.

This run also verifies the corrected differential baseline reader: a prior
full report is compared through its normalized CodeGraph capture instead of
being compared wholesale to one capture.

- Structurely binary SHA-256:
  `3dde38c72a32491f6422722ab1968df86b88562d03835271f4190834e57b0f5c`
- Raw result SHA-256:
  `a9ccf1e0569ca39f84955fa48540de1619489759bf492e0e868174fbe2bd2968`
