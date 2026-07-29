# OpenHarmony callback-argument acceptance — 2026-07-29

Structurely commit `85bd25bd30958eac560027ecfbbb02823b0ae723`
indexed exactly 6,995 ETS files from
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- Fresh index: 122.791 seconds
- Symbols: 47,955
- Relationships: 88,297
- Database: 191,561,728 bytes
- Assertions: 10/10
- Isolated gate peak RSS: 519,056 KiB

The added assertion proves
`RequestDownload.downloadFile → Download.downloadFileCallback` through the
third formal/actual argument position. The callback is registered at line 401,
invoked from nested SDK event closures, and carries
`dynamic/callback-argument` provenance with confidence `0.96`. The nine
earlier correlated ArkTS, ArkUI, package, emitter, and Builder assertions
remain green.

Structurely binary SHA-256:
`f0f7cddcab3e22d115af6669ad56454f7fd18cdfed98287e593bf5dda0d3ae78`.
Raw result SHA-256:
`c07f780fbd0e81b7d135e82cc129e461d3162422d9fb35890a5638927209b94f`.
