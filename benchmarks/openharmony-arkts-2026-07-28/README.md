# OpenHarmony ArkTS acceptance — 2026-07-28

This run checks Structurely commit `090b4a0` against the exact
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

The manifest fixes the corpus at exactly 6,995 `.ets` files. A fresh index
completed in 106.26 seconds and produced 47,909 symbols, 81,917 relationships,
and a 181,583,872-byte database.

Both semantic assertions passed:

- `OperationView.build` is extracted from the expected ArkTS source.
- Its `onClick(this.handleClick)` flow resolves to the component's callable
  field with `framework/arkui-event` provenance, confidence `0.97`, and source
  line `41`. The acceptance runner requires all four values to occur in the
  same returned relationship record.

This is evidence for the pinned revision and asserted patterns, not a claim
that every ArkUI or HarmonyOS convention is supported.
