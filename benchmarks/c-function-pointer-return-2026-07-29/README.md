# C++ function-pointer factory-return acceptance — 2026-07-29

This reproducible adversarial gate verifies Structurely graph model v65 at
implementation commit `44f8892ac3bd86fd22078e8d6db29bc663713f8a`.
The tested release binary SHA-256 is
`8b39785888b0054f9870a10fa270594aab0e1b465b47943e9e65e6797815738a`.

The controlled same-file C++ fixture proves:

- a local initialized from a factory return resolves to the returned callable;
- an immediate `factory(args)(args)` dispatch resolves at its own call site;
- a unique returned target is an exact 0.995-confidence call;
- two conditional returned targets are bounded 0.97-confidence may-calls;
- explicit function-pointer return declarators and deduced `auto` returns work;
- scalar returns, lambda returns, shadowed parameters, local variables,
  uninitialized locals, and a later non-pointer kill fail closed;
- changing a factory from two targets to one removes stale `alpha` edges and
  raises the remaining `beta` edges from 0.97 to 0.995;
- restoring the source restores the original seven dynamic edges;
- every accepted relationship owns a nonzero invocation-site identity, and no
  evidence identities are duplicated.

The fresh fixture emitted seven function-pointer dispatch edges. Rebinding
reduced that inventory to five exact/remaining-valid edges, and restoration
returned it to seven. Fresh indexing took 33.994 ms wall time (14 ms reported
engine time); the rebind and restore syncs took 18.702 ms and 18.173 ms.
Timings are observational, not a cross-tool performance claim.

For an objective, narrowly scoped comparison, the same fixture was indexed by
pinned CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`. Its JSON `callees` output retained
the ordinary calls to `choose` or `exact_factory`, but returned zero `alpha` or
`beta` target edges for all four factory-dispatch callers. This establishes a
feature difference on this fixture only; it does not claim that CodeGraph lacks
all C++ callback or indirect-call analysis.

The raw result records graph model, source commit, worktree state, binary hash,
fixture hashes, every resolved edge and confidence, incremental timings, and
the complete CodeGraph callee inventories. Its SHA-256 is
`f667ab63cb75dc16def079d8e9df1953a8ae7b0385f2ca5fb5c3aa222e0fd4c0`.
`worktreeDirty: true` is expected in this captured run because this new
acceptance script and evidence were not yet committed; the implementation
itself is the clean commit identified above.

Reproduce Structurely alone:

```sh
cargo build --release
python3 scripts/acceptance_c_fnptr_return.py \
  --structurely target/release/structurely \
  --output /tmp/c-function-pointer-return.json
```

Reproduce the differential gate with a clean, built checkout of pinned
CodeGraph 1.5.0:

```sh
python3 scripts/acceptance_c_fnptr_return.py \
  --structurely target/release/structurely \
  --codegraph-repository /path/to/codegraph-1.5.0 \
  --output /tmp/c-function-pointer-return.json
```
