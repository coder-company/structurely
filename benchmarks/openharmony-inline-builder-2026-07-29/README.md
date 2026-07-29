# OpenHarmony inline BuilderParam acceptance — 2026-07-29

Structurely commit `6379b44ea5083acd95215f7c8c3aeecf0965ed5a`
indexed exactly 6,995 ETS files from
`openharmony/applications_app_samples` commit
`a826ab0e75fe51d028c1c5af58188e908736b53b`.

- Fresh index: 117.309 seconds
- Source-emitted symbols: 47,955
- Resolver-reported relationships: 88,979
- Database: 193,331,200 bytes
- Assertions: 14/14
- Isolated gate peak RSS: 518,948 KiB

The added assertion proves the exact production flow:

`ListExchange.build → ListExchangeViewComponent.build.<BuilderParam adapter
ListExchange.deductionView>`

The dispatch resolves an inline arrow adapter assigned in
`code/UI/ListBeExchange/ListExchange/src/main/ets/view/ListExchangeView.ets`
at line 117. It carries
`framework/arkui-builder-param-dispatch` provenance and confidence `0.97`.
The thirteen earlier correlated ArkTS, ArkUI, package, emitter, Builder,
callback, and project-aware BuilderParam assertions remain green.

Synthetic inline adapters and trailing children are materialized only after
the resolver proves one component and one declared BuilderParam. Consequently,
the init report counts parser-emitted source symbols, while validated synthetic
symbols are inserted transactionally during resolution; this report field is
not total persisted graph cardinality.

Structurely binary SHA-256:
`6a3a3acad94fab28cfa13e4b47e2548454f3093c44098f317d62482369c55b32`.
Raw result SHA-256:
`814aa0996c82dd0e04c5cd94b734da7ef397743af9be1e1b767ba63e1791b8e5`.
