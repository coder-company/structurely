# Changelog

All notable changes are documented here. Structurely follows semantic
versioning after the initial `0.1.0` release.

## 0.4.0 — 2026-08-01

- Add `structurely doctor`, a read-only readiness report for the project,
  index, freshness, background indexer, coding-agent integration, and optional
  dashboard, with actionable remedies and automation-safe exit codes.
- Give interactive `setup` and `doctor` runs concise human summaries while
  preserving JSON for pipes, CI, and an explicit global `--json` option.
- Redesign the Unix and PowerShell installers around a restrained four-stage
  experience with download retries, checksum verification, binary smoke tests,
  atomic replacement, upgrade detection, PATH guidance, and clear next steps.
- Keep optional dashboard deployment private and recoverable: only the static
  shell is hosted, provider tools are never silently installed, and deployment
  failure cannot undo a successful Structurely installation.
- Detach the Unix freshness daemon from its launching terminal so project setup
  remains healthy after that terminal or installer session exits.
- Add native installer regressions for failed-checksum preservation,
  non-interactive behavior, dashboard choices, and a real Windows round trip.

## 0.3.0 — 2026-07-30

- Add a responsive private console for index health, semantic search,
  repository research, impact analysis, path tracing, workspaces, sessions,
  recaps, and memory.
- Add an authenticated loopback-only bridge with one-time ten-minute pairing,
  256-bit tab tokens, exact origin allowlisting, private-network preflights,
  bounded bodies, brute-force protection, and request-rate limits.
- Keep repository code, indexes, queries, sessions, recaps, and memory off
  Structurely, Vercel, and Cloudflare infrastructure; hosted deployments
  contain only five static shell and security-header files.
- Add verified Vercel and Cloudflare Pages CLI deployment, local export,
  provider dependency/authentication checks, HTTP verification, and staging
  cleanup without silently installing provider tools.
- Add bridge status, reconnect, token rotation, stop, and removal workflows
  with owner-only local state on Unix.
- Add interactive installer and project-setup choices for Vercel, Cloudflare,
  local-only, or skip while keeping CI and redirected installs non-blocking.
- Add native bridge lifecycle, provider-contract, installer, accessibility,
  privacy, browser API, packaging, and latency regression gates.
- Measure authenticated loopback p95 latency of 17.871 ms for index health and
  3.945 ms for semantic search on the checked-in benchmark machine.

## 0.2.0 — 2026-07-30

- Reach 25/25 behavioral compatibility with pinned CodeGraph 1.5.0 while
  preserving source-backed, bounded context output.
- Expand semantic extraction across 23 language dialects and add tested
  framework flows for React, Express, React Router, FastAPI, Django, NestJS,
  Vue, Svelte, Astro, ArkUI, and OpenHarmony.
- Add atomic graph epochs, deterministic snapshots, incremental convergence,
  resource budgets, fail-closed project configuration, and forward-safe schema
  migration.
- Harden storage with symbolic-link rejection, read-only query connections,
  corruption quarantine, writer coordination, and recoverable daemon
  reconciliation.
- Add idempotent project-local integrations for Codex, Claude Code, and Cursor.
- Add local research, impact and path tracing, durable memories, sessions,
  recaps, and team workspaces across both CLI and MCP, with no cloud sync.
- Add fail-safe durable-state backup and restore, content-index corruption
  recovery, dependency-policy gates, and a native installer round-trip test.
- Beat the pinned Perseus retrieval baseline at rank one (4/5 versus 3/5) and
  top-ten recall (5/5 versus 4/5), with a clean-runner regression gate.
- Avoid global graph rematerialization for source-only edits whose
  resolution-bearing facts are unchanged; pinned-repository improvements range
  from 2.60× to 21.91×.
- Consolidate launch evidence and rewrite task-focused installation, command,
  configuration, operations, compatibility, and release documentation.
- Measure 2.375× faster indexing, 10.537× faster median queries, and 84.60%
  lower peak memory than the pinned CodeGraph baseline.

## 0.1.1 — 2026-07-27

- Correct the Unix release archive path and package through a locally testable,
  shared script.
- Preserve the successfully tested `v0.1.0` tag while shipping the corrected
  release pipeline as a patch release.

## 0.1.0 — 2026-07-27

- Rust-first incremental indexing with atomic SQLite WAL graph epochs.
- Stable, versioned symbol identities and deterministic clean/incremental
  snapshots.
- Confidence-, provenance-, explanation-, and location-bearing relationships.
- Tree-sitter extraction for TypeScript/TSX, JavaScript/JSX, Python, Rust, Go,
  Java, C#, C, C++, Ruby, PHP, Swift, Lua, Kotlin, Scala, and R.
- Receiver-, lexical-, import-, module-, and language-aware call resolution.
- Natural-language symbol retrieval with identifier segmentation and
  deterministic exact-name ranking.
- CodeGraph 1.5-compatible CLI/MCP tools, default tool selection, Markdown
  explore output, cross-project queries, and MCP revision negotiation.
- Native recursive watching with debounce, readiness signaling, periodic
  reconciliation, and transactional visibility.
- Bounded parallel parsing, 1 MiB generated-source guard, bounded WAL growth,
  phase timing, storage health, benchmark reports, and semantic quality gates.
- Linux, macOS, and Windows CI plus checksummed, provenance-attested release
  archives.
