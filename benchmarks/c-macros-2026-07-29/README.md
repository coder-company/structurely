# C macro callback-table acceptance — 2026-07-29

This gate compares Structurely graph model v67 with identity-verified
CodeGraph 1.5.0 commit `572d22bfbe82602080e457bec655f72e3314f9ef`.
The fixture covers source-ordered object and function macros, nested
substitution, designated and whole-table initializers, pointer and struct
arrays, repeated includes, mutually exclusive and unknown conditions,
constant conditions, explicit `#undef`, and incremental header changes.

Structurely resolved all 14 intended callback targets across 11 dispatch
callers and emitted zero targets for the three deliberately invalid or
impossible callers. Pinned CodeGraph resolved one of the 14 intended targets:
the function-like designated `SLOT(slot_target)` case. It emitted no target
for the other ten positive callers. Both engines emitted no target for the
three negative callers.

After changing both the source object alias and an included-header alias,
Structurely changed exactly the two affected files and replaced both targets.
A following no-op sync reported zero changed files and preserved the exact
target set.

The adversarial portion additionally proves that:

- same-offset conditional groups in different headers cannot collide;
- a header included twice reevaluates its condition in each include context;
- six independent unknown branches with identical outcomes converge instead
  of exhausting the 32-state cap;
- changing a condition macro inside its selected branch does not retroactively
  change the branch decision;
- `0`, hexadecimal/octal/suffixed zero, comments, `&&`, and `||` fold safely;
- expanded postfix calls are not misreported as direct callback addresses;
- 10,000 unconditional definitions replay through the linear fast path.

Reproduce:

```bash
cargo build --release
python3 scripts/acceptance_c_macros.py \
  --structurely target/release/structurely \
  --codegraph-repository /path/to/pinned/codegraph \
  --output benchmarks/c-macros-2026-07-29/results.json
```

The measured wall times are fixture-scale diagnostics, not a general
performance comparison. Use the pinned 441-file benchmark for performance
claims.
