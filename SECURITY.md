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
limit which files its process can read.

The optional dashboard bridge is a network service restricted to an operating
system loopback address. It requires one-time pairing and a random bearer token,
enforces an explicit origin allowlist, and bounds request bodies. It is not a
multi-tenant service and must never be exposed through a reverse proxy, tunnel,
container port publication, or remote bind. A Vercel or Cloudflare deployment
contains only static assets; project data flows directly from the local bridge
to the paired browser tab. See `docs/dashboard.md` for the complete boundary.

Supported releases and the current `main` branch receive security fixes.
Release archives provide checksums and GitHub build-provenance attestations as
described in `docs/releases.md`.
