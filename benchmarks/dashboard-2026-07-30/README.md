# Dashboard bridge benchmark

This benchmark measures the authenticated HTTP adapter added by the private
dashboard. It runs against the current Structurely repository through an IPv4
loopback socket. The browser-equivalent client pairs once, warms both routes,
then alternates index-health and semantic-search requests for 30 iterations.

Run it again with:

```bash
cargo build --locked
python3 scripts/benchmark_dashboard.py \
  --binary target/debug/structurely \
  --project . \
  --output benchmarks/dashboard-2026-07-30/results.json
```

## Results

| Operation | p50 | p95 | Enforced p95 ceiling |
|---|---:|---:|---:|
| Index health | 13.943 ms | 17.871 ms | 50 ms |
| Semantic search | 3.072 ms | 3.945 ms | 100 ms |

The report records `cloud_requests: 0`. It measures the complete authenticated
loopback request, JSON serialization, engine read, and client JSON parsing.
Results are evidence for this machine and repository, not a universal latency
claim. The ceilings are deliberately wider than the observed values so the CI
gate catches material regressions without treating scheduler noise as failure.
