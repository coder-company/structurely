# MCP differential after hardened ArkUI routing — 2026-07-28

The live persistent-stdio gate ran clean Structurely commit `c7af33f` against
identity-verified CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`.

- Compatibility predicates: 21/21 for both engines
- ArkUI route predicate: exact page name and file for both engines, plus
  `framework/arkui-route` provenance for Structurely
- Context usefulness: 1.0000 for both engines
- Required-fact recall, relevant-file recall, and file precision: 1.0000 for
  both engines
- Structurely response size: 2,164 characters
- CodeGraph response size: 2,560 characters
- Structurely binary SHA-256:
  `845f7b1ba157354318f9dae42fa8c2d75c0be0c3bb8cce4ce5cf504b6efe0f86`

The Structurely-only adversarial suite additionally requires an exact
`@ohos.router` or `@kit.ArkUI` import binding, rejects lexical shadows and
ambiguous `@Entry` targets, normalizes bounded literal paths, and verifies
incremental marker and edge cleanup.
