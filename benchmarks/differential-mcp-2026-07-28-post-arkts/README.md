# MCP differential after ArkTS — 2026-07-28

The live stdio gate ran a clean Structurely commit `5da97e2` against
identity-verified CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`.

- Compatibility predicates: 19/19
- Added ArkTS predicates: ArkUI event handler and reactive state-to-build flow
- Structurely context usefulness: 1.0000
- CodeGraph context usefulness: 0.9583
- Structurely binary SHA-256:
  `89f044df4f59c544a351679e5076981875ae687b286cd7b01efb516f526fa058`

The runner verifies the comparator checkout's Git commit and package version
before either MCP session starts. Structurely-specific ArkUI predicates also
require framework provenance, so ordinary same-name call resolution cannot
satisfy them.
