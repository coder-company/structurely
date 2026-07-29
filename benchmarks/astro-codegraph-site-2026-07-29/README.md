# Astro acceptance on the CodeGraph site — 2026-07-29

Structurely commit `e33fda3` adds graph model v57 and production Astro
extraction. The acceptance corpus is the four real `.astro` files and their
TypeScript utility from identity-verified CodeGraph 1.5.0 commit
`572d22bfbe82602080e457bec655f72e3314f9ef`.

The clean release gate indexes all five files in 35.933 ms wall time and
materializes 13 compact source symbols and 17 relationships:

- four full-file Astro components;
- the `/` page route and its exact route-to-component edge;
- the page `index` component rendering `GraphDiagram` at line 70;
- exact `getStarsLabel` import/call flows from `index.astro` line 13 and
  `SocialIcons.astro` line 9;
- the unchanged `getStarsLabel → fetchStars → format` TypeScript utility flow.

The gate explicitly rejects CodeGraph noise observed on the same corpus:
external `Default` does not become a user symbol or self-edge, markup in
frontmatter/scripts/styles/comments is not double-scanned, and no Astro
template self-edge is present.

Extraction accepts a frontmatter fence only on the first nonblank line,
preserves BOM/CRLF/multibyte offsets, lexically scans multiple script regions,
and fails closed on unclosed regions. Template calls require one exact relative
project import. Routes support static, Unicode, dotted, dynamic, terminal or
nonterminal rest segments, while malformed brackets, repeated rest parameters,
underscore-private paths, and project-root import escapes fail closed.

Independent review found and closed route-rest, dotted/Unicode page,
project-root traversal, external self-resolution, and embedded-tag boundary
defects before merge. The final repository gate passes 213 library tests,
daemon and persistent MCP process tests, strict all-target/all-feature Clippy,
formatting, and diff checks. The pinned MCP differential remains 25/25 with
both engines scoring 1.0000 context usefulness.

The clean release binary SHA-256 is
`0aa4931672d58945a7e20e602e019cdb2dd1fb82a90889bad255468c9d8136af`.
The raw Astro acceptance report SHA-256 is
`6c4b8a8a979c3a6c50744c74d72435532ff60a9710e79ab4e8fb7b70a648fa90`;
the raw MCP report SHA-256 is
`a802921a18624ab7fae4a857a8e2634823a663553d12f0b8b9d4f2b4606a381d`.
