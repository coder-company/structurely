# MCP compatibility and context usefulness — 2026-07-28

This persistent-stdio differential run compares Structurely with CodeGraph
1.5.0 commit `572d22bfbe82602080e457bec655f72e3314f9ef`.

- All 16 protocol and behavioral scenarios pass for both engines, including
  React `setState → render` and `render → JSX child` runtime-flow checks.
- Both return every required fact, all three relevant files, both flow spines,
  current line-numbered source, and stay within the 48,000-character budget.
- Structurely returns only the three relevant files.
- CodeGraph also returns unrelated `api.py`, giving file precision 0.75.
- Aggregate usefulness is Structurely 1.0000 versus CodeGraph 0.9583.

The score is fixture-specific and intentionally decomposed in `results.json`;
it is not a claim about every possible query. The executable gate fails if
Structurely drops below the pinned engine on this flow:

```bash
python3 scripts/differential_mcp.py \
  --structurely target/debug/structurely \
  --codegraph /path/to/codegraph/dist/bin/codegraph.js \
  --output /tmp/differential-results.json
```
