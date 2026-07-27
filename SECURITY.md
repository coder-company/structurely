# Security policy

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability. Use GitHub's private
security advisory flow for `coder-company/structurely` and include:

- affected Structurely version or commit;
- operating system and installation method;
- minimal reproduction and observed impact;
- whether untrusted repository content or MCP input is required.

Maintainers should acknowledge a complete report within seven days. Timelines
for a fix and disclosure depend on severity and reproducibility. Please avoid
accessing data you do not own and give maintainers a reasonable opportunity to
ship a fix before public disclosure.

## Scope and trust model

Structurely parses repository contents as untrusted input and does not execute
indexed source code. It does write a SQLite index inside the selected project
and exposes source excerpts to the local MCP client that launched it. Anyone
who can invoke that MCP server should therefore be treated as having read
access to the indexed project.

The `projectPath` MCP argument can select another local project. Configure the
server only for trusted local clients and use operating-system permissions to
limit which files its process can read. Structurely is not a network service
and does not provide authentication or tenant isolation.

Supported releases and the current `main` branch receive security fixes.
Release archives provide checksums and GitHub build-provenance attestations as
described in `docs/releases.md`.
