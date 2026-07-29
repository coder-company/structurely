# MCP probe and atomic-publication gate — 2026-07-29

This gate verifies Structurely source commit `b940801` against identity-checked
CodeGraph 1.5.0 commit `572d22bfbe82602080e457bec655f72e3314f9ef`.

The persistent stdio differential now includes the discovery requests commonly
sent by Codex and other MCP clients:

- `resources/list` returns `{"resources":[]}`;
- `resources/templates/list` returns `{"resourceTemplates":[]}`;
- `prompts/list` returns `{"prompts":[]}`.

Both engines pass all 25 shared scenarios. Structurely continues to pass its
separate Harmony emitter extension, and both engines retain a 1.0000 context
usefulness score. The tested release binary SHA-256 is
`f6f2f648c234ebe6202436afdcba76a19d5f861ac37740bec687eefb19d78581`.
The raw differential report SHA-256 is
`50f5eddcb6ccb104ae88e2ec70cc865d5f759d308245b66d9b108b9903084d2e`.

The operational gate additionally passes:

- 64 consecutive replacements of an existing file with no temporary leaks;
- failed publication preserving the previous destination and cleaning its
  temporary file;
- daemon state-publication failure requesting shutdown and retaining its error;
- idempotent integration configuration replacement;
- 200 library tests plus the daemon and persistent MCP process tests;
- strict Clippy, formatting, and diff checks.

Unix publication uses same-filesystem rename and parent-directory
synchronization. Windows publication uses `MoveFileExW` with
`MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`.
