# C compilation-database acceptance — 2026-07-29

This adversarial differential gate verifies Structurely graph model v66 at
implementation commit `63f9b02bc917d5ac4d4aafa7b11c34f6a35cc81b`.
The tested release binary SHA-256 is
`4851295e3e76502d1f8e03b94f3de45f5a19bc8fda229390a038dfab669a5ca6`.

The controlled project has two C translation units that both use
`#include <config.h>`, but their `compile_commands.json` entries select
different in-project include directories. The two headers deliberately reverse
the order of `Ops.run` and `Ops.stop`, making an incorrect project-wide include
path union observable in the resolved function-pointer targets.

Structurely resolves the exact intended edges:

- `dispatch_a → alpha` through `include-a/config.h`;
- `dispatch_b → beta` through `include-b/config.h`.
- `dispatch_shared → shared_target` through the shared `-isystem` directory.

Reversing only the two duplicate-header compilation-database entries preserves
all three targets and triggers a semantic refresh; the next sync changes zero
files, and restoring only the database preserves them again without source
edits. Fresh indexing took 44.917 ms wall time (17 ms reported engine time);
reorder, no-op, and restore took 28.862 ms, 4.688 ms, and 28.158 ms. Timings
are observational, not a cross-tool performance claim.

Pinned CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef` did not preserve the two
translation-unit contexts on this fixture: both dispatchers resolved to
`beta` and `decoy_a`. Its persisted import edges send both sources to
`include-a/config.h`. After reversing only the database entries, `sync` reports
that the index is already current and retains those mappings; a full index
sends both sources to `include-b/config.h`. Both engines correctly resolve the
unique shared header. This is a narrowly scoped correctness and live-refresh
comparison, not a claim about every CodeGraph C project.

The v66 implementation accepts `arguments` before `command`; parses bounded
`-iquote`, `-I`, `/I`, `-isystem`, and `-idirafter` forms; confines databases,
sources, include directories, and resolved headers to the canonical project
root; keeps duplicate build variants separate and fails closed when they
disagree; fingerprints canonical contexts; and invalidates on database,
header, and include-directory symlink changes. It never executes compiler
commands.

Intentionally unsupported behavior includes compiler `-D`/`-U` evaluation,
response files, external/system headers outside the project, and generated
headers that are not indexed project sources.

The checked-in raw result SHA-256 is
`6705500d4ae9d95a094f6da7297e300c568603f1aea476367e444fff0c3317fd`.
`worktreeDirty: true` is expected because the acceptance script and evidence
were captured after the clean implementation commit named above.

Reproduce:

```sh
cargo build --release
python3 scripts/acceptance_c_compdb.py \
  --structurely target/release/structurely \
  --codegraph-repository /path/to/codegraph-1.5.0 \
  --output /tmp/c-compdb-results.json
```
