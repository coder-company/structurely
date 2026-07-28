# MCP differential after ArkUI style helpers — 2026-07-28

The live persistent-stdio gate ran clean Structurely commit `2bcfc6a` against
identity-verified CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`.

- Compatibility predicates: 22/22 for both engines
- New ArkUI helper predicate: `StyleHome.build → highlighted`
- Structurely requires `framework/arkui-helper` provenance
- Context usefulness: 1.0000 for both engines
- Required-fact recall, relevant-file recall, and file precision: 1.0000 for
  both engines
- Structurely response size: 2,164 characters
- CodeGraph response size: 2,560 characters
- Structurely binary SHA-256:
  `dd21c2b62f49571f14391d8ca93c9ccbbdb80be2aa6c8e0a3c672ee3326f336a`

Structurely's adversarial fixture additionally rejects undecorated lookalikes,
wrong `@Extend` intrinsic roots, and another component's `@Styles` method; its
incremental rewrite proves stale helper edges are removed.
