# CodeGraph compatibility

Structurely targets the coding-agent behavior of CodeGraph 1.5.0 at commit
`572d22bfbe82602080e457bec655f72e3314f9ef`. It does not read or write
CodeGraph's database format.

## What matches

The pinned differential suite passes all 25 shared predicates:

- MCP initialization, tool discovery, resources, templates, and prompts;
- search with exact and ambiguous names;
- callers, callees, impact, node windows, and explore flows;
- status and indexed-file resources;
- React rendering, JSX children, interface dispatch, ArkUI state and events,
  OpenHarmony packages and routes;
- missing arguments, invalid limits, and missing symbols.

Both implementations score 1.0 for required-fact recall, relevant-file recall,
file precision, flow continuity, line-numbered source, and output budget. See
the [accepted launch comparison](../benchmarks/release-hardening-2026-07-29/README.md).

## What Structurely adds

Every inferred relationship can include confidence, provenance, source
location, and a plain-language explanation. Structurely also provides atomic
graph epochs, deterministic snapshots, explicit freshness metadata, bounded
public queries, fail-closed configuration, and corruption recovery that
quarantines unsafe storage.

The test suite includes deeper framework and language behavior for React,
Express, React Router, FastAPI, Django, Django REST Framework, NestJS, Vue,
Svelte, Astro, ArkUI, OpenHarmony, and C/C++ callback patterns. These additions
do not change the compatible MCP fields.

## Understand intentional limits

Structurely does not claim database compatibility or complete static analysis
of every runtime behavior. It fails closed when evidence is ambiguous or work
would exceed a safety budget.

Known limits include:

- Objective-C, Liquid, Delphi, and Luau source;
- arbitrary reflection, runtime code generation, and string-built dispatch;
- unbounded heap-alias and cross-process callback flow;
- external or generated headers that are not part of the indexed project;
- every framework plugin or language dialect supported by future CodeGraph
  releases.

Treat relationships with lower confidence as candidates for inspection, not
proof of runtime behavior. File a focused fixture when a missing relationship
matters to your project.
