# MCP differential after Harmony package resolution — 2026-07-28

The live stdio gate ran clean Structurely commit `9e09aca` against
identity-verified CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`.

- Compatibility predicates: 20/20
- New predicate: bare ArkTS import through an `oh-package.json5` `file:`
  dependency and declared `main`
- Structurely context usefulness: 1.0000
- CodeGraph context usefulness: 0.9583
- Structurely binary SHA-256:
  `7218a95a2edd0f0c8c135455af69ee98cd23b6a82eefabcf6740534040ef075e`

The fixture includes a same-name decoy outside the target package. Both engines
must return the symbol from `harmony/profile-data/Index.ets`.
