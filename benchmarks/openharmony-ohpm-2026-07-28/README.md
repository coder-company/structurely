# OpenHarmony ohpm acceptance — 2026-07-28

Structurely commit `31838d0` indexed the exact 6,995-file ETS corpus from
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- Fresh index: 129.85 seconds
- Symbols: 47,909
- Relationships: 81,952
- Database: 181,858,304 bytes
- Assertions: 3/3

The new package assertion follows the real `@ohos/window-component` dependency
from the entry module's `oh-package.json5` to the dependency's declared
`src/main/ets/components/MainPage/MainPage.ets` entry. `VideoPlayer.build`
resolves `WindowComponent` with `framework/arkui-render` provenance,
confidence `0.97`, and source line `25`; all values must occur in one returned
relationship.
