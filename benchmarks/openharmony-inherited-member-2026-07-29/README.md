# OpenHarmony inherited-member acceptance — 2026-07-29

Clean Structurely commit
`d28838f4ce6d49bb558536d50898e3f6b72ae1f0` indexed exactly 6,995 ETS
files from `openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- CLI duration: 120.209 seconds
- Measured wall time: 2:00.29
- Source-emitted symbols: 47,955
- Resolver-reported relationships: 83,270
- Persisted symbols/relationships: 48,251 / 124,727
- Database: 202,125,312 bytes
- Correlated assertions: 17/17
- Isolated gate peak RSS: 232,428 KiB

The seventeenth assertion proves a production inherited receiver flow:
`AlbumPage.btnAction` calls `BasicDataSource.notifyDataReload` at line 64.
The imported `MediaDataSource` receiver has no local override, so bounded
nearest-ancestor lookup follows verified `extends` relationships to the exact
`BasicDataSource` method. The relationship uses
`tree-sitter/name-resolution`, confidence `0.97`, and the
`nearest inherited receiver type` explanation.

The full inherited inventory contains 23 relationships at 23 evidence sites,
from 19 sources to 11 targets. Every relationship has name-resolution
provenance and confidence 0.97.

The semantic snapshot against the accepted call-result database changes
125,655 persisted relationships to 124,727: 1,667 removed and 739 added, net
-928. Every removed relationship is ordinary name resolution; no callback
relationship is removed. The callback audit records zero removals and ten
additions. In particular, VoiceCallDemo `BufferModel` lines 192, 195, and 201
each retain the exact nullable `SocketImpl` method edge, inline-callback
containment, and callback-argument invocation.

- Structurely binary SHA-256:
  `3dde38c72a32491f6422722ab1968df86b88562d03835271f4190834e57b0f5c`
- Database SHA-256:
  `a19d6960841a8a809dd9457dc7b82971e57639cac206dd2da7834fd39c27ede4`
- Raw acceptance SHA-256:
  `c1410a8de07d47de597028cb978548f7ef6e3330635bd50f632d707828e376dd`
- Raw init SHA-256:
  `c7e4db717a1d56969f4f049f98fa1a029478ecc443bb770898b839cc4a33b544`
- Inherited inventory SHA-256:
  `bea0031585b7855c35a036c4e93754a0a8027662cab2ae5cedc741e5f5eaa901`
- Snapshot audit SHA-256:
  `bd0c0c139b6c0cdc358741b0f9dc29e47ce4ebb23bd9416a3ec7cbf5bf86b6f5`
- Callback audit SHA-256:
  `5d917ebb3553431d36b4fc48b2a0b812a80813f0cb5ce037ffec13af6cc88ec2`
- Timing SHA-256:
  `a8405bcdd3a16dfdf709d162662d4d28bc2ff2a7ddd1953dd903ab65964eca0f`
