# OpenHarmony project Builder flow acceptance — 2026-07-29

Structurely commit `16c3a0d69ebac8914f551a6474466aa99ce4d897`
indexed exactly 6,995 ETS files from
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- Fresh index: 124.384 seconds
- Symbols: 48,370
- Relationships: 88,790
- Database: 193,306,624 bytes
- Assertions: 13/13
- Isolated gate peak RSS: 519,184 KiB

The two added assertions prove both project-aware BuilderParam adapters:

- `CodeView.build → BrotherComponentSync.build.<BuilderParam child CodeView>`
  dispatches to a trailing-child builder assigned at line 26.
- `TitleExpansionView.build → titleMenu` at line 93, with `titleContent` also
  present, resolves verified imported decorated builders.

Both carry `framework/arkui-builder-param` provenance, or the corresponding
`framework/arkui-builder-param-dispatch` provenance, with confidence `0.97`.
The eleven earlier correlated ArkTS, ArkUI, package, emitter, Builder, and
callback assertions remain green.

Structurely binary SHA-256:
`5f9c43c5c9472526a406a164bd8870af867f05e80d0d24de004f10ed46db2270`.
Raw result SHA-256:
`7096845fc63366898b477ef6534c8f999e994b93239eb6a5f549547fce7e48d4`.
