# OpenHarmony BuilderParam acceptance — 2026-07-29

Structurely commit `ca4d8227289a6b9fd5a9c95cca03e90d3efc4ca8`
indexed exactly 6,995 ETS files from
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- Fresh index: 119.907 seconds
- Symbols: 47,955
- Relationships: 88,297
- Database: 191,582,208 bytes
- Assertions: 11/11
- Isolated gate peak RSS: 519,140 KiB

The added assertion proves the exact same-file assignment
`HeatHistogramContent.build → HeatHistogramContent.content` at line 179. The
target is a component-owned decorated builder assigned to the child
component's declared `@BuilderParam`; the edge carries
`framework/arkui-builder-param` provenance with confidence `0.97`. The ten
earlier correlated ArkTS, ArkUI, package, emitter, Builder, and callback
assertions remain green.

Structurely binary SHA-256:
`d1b54398e9cd64e453892a44d4b0c4698441fc21946da7f91d4bef270a7bcc3c`.
Raw result SHA-256:
`3540107f9aed48e8aec0af2ecc0910e0d35048478a90b9965769c58e2b9610aa`.
