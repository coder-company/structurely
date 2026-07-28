# OpenHarmony Harmony emitter acceptance — 2026-07-28

Structurely commit `8c3972ed31fbf599436a85d0d4226c465df8e320`
indexed the exact 6,995-file ETS corpus from
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- Fresh index: 125.526 seconds
- Symbols: 47,939
- Relationships: 82,227
- Database: 183,586,816 bytes
- Assertions: 7/7

The new exact-record assertion proves that `PageThreadModel.build` dispatches
to `PageThreadModel.build.callback` through `framework/ohos-emitter`, with
confidence `0.97` and emission evidence at line `121`. All six previous ArkTS,
ArkUI event, package-render, route, and style-helper assertions remain green.

- Structurely binary SHA-256:
  `5ac602fabfe64889456191ce244bdeb214c18fe0bbd40efd4174ad2fd7bdfd90`
- Manifest SHA-256:
  `6b23cf6b18fc45715c3af4fcaaefa8c76294728debf565c3a97f563e47196ced`
- Raw result SHA-256:
  `e3ce010b2de88da8c90f994233beef977599d8b1d975e48bd4da3f8465dbc498`
