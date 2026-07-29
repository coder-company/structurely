# OpenHarmony ArkUI Builder acceptance — 2026-07-29

Structurely commit `2fb00eadd928dad0d0e65f2a19e4409c83f7d270`
indexed exactly 6,995 ETS files from
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- Fresh index: 127.893 seconds
- Symbols: 47,955
- Relationships: 82,429
- Database: 183,951,360 bytes
- Assertions: 9/9
- Isolated gate peak RSS: 519,268 KiB

The added assertion proves
`ActionBarButton.build → ActionBarButton.PopupBuilder` for the real
children-bearing `Row { … }.onClick(…).bindPopup(…)` chain, with
`framework/arkui-builder-registration`, confidence `0.97`, and registration
line `99`. The eight earlier correlated ArkTS, ArkUI, package, and emitter
assertions remain green.

Structurely binary SHA-256:
`3d0194aeb994222fb1e5363b2536a82dc03c7d87b12787f7e970afddf3bdcd08`.
Raw result SHA-256:
`353c27e29256599a53be64766d3577f59336ab2486645ebdf30266980a8c2bb9`.
