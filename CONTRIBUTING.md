# Contributing

Structurely accepts focused issues and pull requests. For behavior changes,
describe the user-visible contract and add a fixture that fails without the
change.

## Development checks

Use the pinned toolchain and locked dependency graph:

```bash
cargo fmt --check
cargo test --all-targets --locked
cargo clippy --all-targets -- -D warnings
cargo build --release --locked
```

Run the semantic quality gate:

```bash
cargo run --locked -- init fixtures/semantic
cargo run --locked -- quality \
  --path fixtures/semantic \
  --manifest fixtures/semantic/quality.json
```

Parser changes need positive and adversarial cases. Relationship changes must
assert provenance, confidence, and deterministic resolution. Storage changes
must preserve atomic epochs and include forward migration coverage. MCP changes
must preserve JSON-RPC session health after invalid requests.

Do not commit generated `.structurely` indexes. Benchmark claims must follow
`docs/benchmarks.md` and include raw machine-readable reports. Security issues
must follow `SECURITY.md`, not the public issue tracker.
