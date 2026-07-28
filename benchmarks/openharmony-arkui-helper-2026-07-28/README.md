# OpenHarmony ArkUI helper acceptance — 2026-07-28

Structurely commit `2bcfc6a` indexed the exact 6,995-file ETS corpus from
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- Fresh index: 126.608 seconds
- Symbols: 47,909
- Relationships: 82,197
- Database: 183,365,632 bytes
- Assertions: 6/6

Two new exact-record assertions prove both supported style-helper shapes:

- `VerifyCodeComponentWithoutCursor.buildVerifyCodeComponent` calls global
  `@Extend(Text)` helper `verifyCodeUnitStyle` with
  `framework/arkui-helper`, confidence `0.97`, at line `149`.
- `NotificationPublish.view` calls component-owned `@Styles` method
  `NotificationPublish.viewStyle` with the same provenance and confidence at
  line `71`.

All four earlier ArkTS extraction, event, package-render, and route assertions
remain green. Structurely release binary SHA-256:
`dd21c2b62f49571f14391d8ca93c9ccbbdb80be2aa6c8e0a3c672ee3326f336a`.
