# OpenHarmony imported emitter acceptance — 2026-07-28

Structurely commit `f521fa014b031359007c61b52af4197d402b627c`
indexed exactly 6,995 ETS files from
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- Fresh index: 130.316 seconds
- Symbols: 47,955
- Relationships: 82,223
- Database: 183,758,848 bytes
- Assertions: 8/8

The added real assertion proves
`Utils.sendProcessMessage → KeyManager.aboutToAppear.<emitter callback…>`
through an imported `{ eventId: 1 }` descriptor, with
`framework/ohos-emitter`, confidence `0.97`, and emission line `35`. The seven
earlier correlated ArkTS and ArkUI assertions remain green.

Structurely binary SHA-256:
`4806a464ee3350d03932d4f8aac4062cb3994878288e90a4413b99140b6814aa`.
