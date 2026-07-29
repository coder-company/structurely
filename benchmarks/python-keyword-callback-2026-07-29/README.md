# Python keyword callback acceptance — 2026-07-29

Clean Structurely commit `9637a27` advances the graph model to v54 and maps
Python keyword callback arguments to exact formal indexes only after unique
callee resolution.

The parser persists exact eligible formal names for undecorated Python
functions and methods. Ordinary, defaulted, and keyword-only formals are
supported; positional-only and variadic formals are not keyword-addressable.
The resolver rejects unknown or duplicate keyword names, `**mapping`,
ambiguous/external callees, decorated definitions, malformed syntax, and calls
above the uniform 64-argument work cap. Positional callbacks—including valid
positional calls into `/` formals—retain their existing behavior.

Independent review caught and closed three regressions before merge:

- source-text slashes inside defaults could be mistaken for the `/` separator;
- the initial argument cap did not suppress positional callback work;
- positional-only formals were accidentally excluded from positional flows.

The final gate passes 185 unit tests, daemon and MCP integrations, strict
Clippy, and independent review with no remaining blockers.

On pinned LightRAG, a clean 515-file index materializes 16 exact keyword
callback-argument relationships at confidence 0.96, bringing all accepted
inline callback-argument relationships to 46. On pinned Django, a clean
2,972-file index materializes 14 keyword relationships: the three audited
production flows in CookieStorage and the release-archive script, plus eleven
test flows. Django has 90 accepted inline callback-argument relationships in
total.

Combined, the two real repositories prove 30 keyword callback-argument edges
and 30 stable inline callback identities. This materially exceeds the four
strict seed sites because full unique-callee resolution proves additional
`safe_vdb_operation_with_exception`, `truncate_list_by_token_size`, cookie
bisect, and schema-test registrations.
