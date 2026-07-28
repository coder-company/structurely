# MCP differential after Harmony emitter resolution — 2026-07-28

The clean persistent-stdio run compares Structurely commit
`8c3972ed31fbf599436a85d0d4226c465df8e320` with identity-verified CodeGraph
1.5.0 commit `572d22bfbe82602080e457bec655f72e3314f9ef`.

- Shared compatibility: both engines pass 22/22 predicates.
- Context usefulness: both engines score 1.0000.
- Structurely response size: 2,459 characters.
- CodeGraph response size: 2,532 characters.
- Structurely-only Harmony emitter extension: 1/1.

The extension is scored separately from shared compatibility: Structurely
resolves the exact `@ohos.events.emitter` channel flow, while pinned CodeGraph
does not. This is measured superiority, not a compatibility requirement
silently imposed on CodeGraph.

Structurely binary SHA-256:
`c79f24d0f332bc3005d448a0e47b47904cdefb9116d4414f33dcb6d19a13f2da`.
