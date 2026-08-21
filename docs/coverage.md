---
title: Coverage matrix
description: The OpenRouter e2e case catalog — every tool, parameter, and protocol flow exercised by the acceptance suite.
---

# Coverage matrix

Maintained in sync with the semantic case catalog in
`tests/openrouter/cases.rs` and the runtime tool inventory. For the
human-readable tool reference see the [server pages](/servers/filesystem),
and for the harness, bounds, and the most recent verified run see
[OpenRouter E2E](/openrouter-e2e).

**Anti-drift guarantee:** at runtime, `openrouter::cases::assert_coverage`
asserts **exact set equality** between the catalog and the live tool
inventory (per server) and requires every exposed parameter to appear in at
least one online case (provided for all parameters; additionally omitted for
optional parameters). Adding a tool or parameter without updating this
catalog fails the suite, so the matrix below cannot silently drift from the
binary. This document is the human-readable mirror; the code is the source
of truth.

Categories:

- **online** — executed in the ignored acceptance suite (`cargo test --test
openrouter_e2e -- --ignored`) against the real OpenRouter API, one forced
  assistant → tool → assistant roundtrip per case.
- **offline** — covered by ordinary `cargo test` suites (`tests/*_server.rs`,
  unit tests in `tests/openrouter/schema.rs`) with no network.

## Model and bounds

- Default model (required alias): `~deepseek/deepseek-v4-flash-latest`
  (override `OPENROUTER_MODEL` accepted for diagnostics; response `model` is
  the concrete resolved model and is printed).
- Endpoint `POST https://openrouter.ai/api/v1/chat/completions`, Bearer auth
  from `OPENROUTER_API_KEY` only.
- Forced tool shape `{"type":"function","function":{...}}`;
  `parallel_tool_calls: false`, `stream: false`, `temperature: 0`.
- Request timeout ≤ 45 s; response body ≤ 1 MiB; ≤ 2 attempts, retry only
  for 429/5xx/transport; max 1 tool per forced case; suite budget ≤ 15
  minutes. One additional retry of a whole roundtrip is allowed for
  model-generation flakes only: a model-echo deviation (dropped/rewritten
  arguments — the argument guard runs again and still blocks before any MCP
  execution) or an empty final answer cut off by the `length` finish reason.
  Oracle failures and server failures are never retried.
- `max_tokens`: 256 per tool call, 200 per final answer/consumption request.

## Schema pipeline

- `tests/openrouter/schema.rs` is the **single** MCP-schema → OpenRouter
  schema normalizer (no duplicated handwritten schemas anywhere): resolves
  `$defs`/`$ref`, collapses nullable `["T","null"]` types, preserves
  properties/required/description/default/enum/minimum/maximum/
  minItems/maxItems and declared `additionalProperties`, adds
  `additionalProperties: false` on every object, and collects unsupported
  keywords in diagnostics (unit-tested offline with local JSON fixtures).
- A deterministic local validator (same module) checks required, JSON types,
  objects/properties/additionalProperties, arrays/items, enum, numeric
  min/max before any model-produced argument reaches an MCP server. Model
  arguments must also equal the case's intended arguments (modulo
  schema-declared default padding) — a deviation blocks MCP execution and
  is reported, never retried.

## Legacy OpenRouter chat harness

The matrix below is the catalog exercised by `tests/openrouter_e2e.rs`. It
covers the original filesystem, fetch, memory, and shell inventory. It does
**not** cover `activate_skill`, `spawn_agent`, `send_input`, or `wait_agent`.
Those new tools have offline integration coverage. The optional
`tests/agents_openrouter_e2e.rs` test uses `.env.test` environment loading and
the Responses adapter; it is separate from the legacy harness. Never commit
or log that file or its values.

## Tool inventory and case matrix

### Filesystem server (`mcp-filesystem`)

| Tool                           | Parameters                                                                      | Online cases                                                                                         | Offline coverage                                                                                                                  |
| ------------------------------ | ------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `read_text_file`               | `path` (req, string); `head` (opt, int ≥0); `tail` (opt, int ≥0)                | fs-001 (omits head/tail), fs-002 (head), fs-003 (tail), fs-029 (head on large file)                  | missing file, both head+tail rejected, bad types (`tests/fs_server.rs`)                                                           |
| `read_file` (deprecated alias) | same schema as `read_text_file`                                                 | fs-004 (omits head/tail), fs-005 (head), fs-006 (tail)                                               | alias identity                                                                                                                    |
| `read_media_file`              | `path` (req, string)                                                            | fs-007 (PNG → image block), fs-008 (text → embedded resource)                                        | —                                                                                                                                 |
| `read_multiple_files`          | `paths` (req, string[])                                                         | fs-009 (empty → error), fs-010 (one), fs-011 (multiple)                                              | partial failure tolerated                                                                                                         |
| `write_file`                   | `path` (req), `content` (req)                                                   | fs-012 (create, disk oracle), fs-013 (overwrite, disk oracle)                                        | missing args rejected                                                                                                             |
| `edit_file`                    | `path` (req), `edits` (req, array of `{oldText,newText}`), `dryRun` (opt, bool) | fs-014 (dryRun=true, preview only), fs-015 (dryRun=false, applied), fs-016 (dryRun omitted, 2 edits) | no-match error, whitespace tolerance                                                                                              |
| `create_directory`             | `path` (req)                                                                    | fs-017 (nested, disk oracle)                                                                         | idempotent re-create                                                                                                              |
| `list_directory`               | `path` (req)                                                                    | fs-019 (markers)                                                                                     | empty/one/multiple entries, sorting                                                                                               |
| `list_directory_with_sizes`    | `path` (req); `sortBy` (opt, string: name\|size)                                | fs-020 (sortBy omitted), fs-021 (sortBy=size)                                                        | invalid sortBy rejected, totals                                                                                                   |
| `directory_tree`               | `path` (req); `excludePatterns` (opt, string[])                                 | fs-022 (omitted), fs-023 (1 pattern), fs-024 (2 patterns)                                            | JSON shape, nested children                                                                                                       |
| `move_file`                    | `source` (req), `destination` (req)                                             | fs-018 (rename, disk oracle)                                                                         | rename/move with disk side effects (`tests/fs_server.rs`); destination-exists behavior is platform-dependent and **not** asserted |
| `search_files`                 | `path` (req), `pattern` (req); `excludePatterns` (opt, string[])                | fs-025 (omitted, `*.txt`), fs-026 (1 pattern, `**/*.rs`)                                             | no matches, exclusions                                                                                                            |
| `get_file_info`                | `path` (req)                                                                    | fs-027 (metadata)                                                                                    | dir/file variants                                                                                                                 |
| `list_allowed_directories`     | (none)                                                                          | fs-028 (zero-param)                                                                                  | multi-root output                                                                                                                 |

### Fetch server (`mcp-fetch`)

| Tool    | Parameters                                                                                                                      | Online cases                                                                                                                                                                                                                                                                                                                               | Offline coverage                                                                                                    |
| ------- | ------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------- |
| `fetch` | `url` (req, string); `max_length` (opt, int 1..999999, default 5000); `start_index` (opt, int ≥0, default 0); `raw` (opt, bool) | ft-001 (defaults, html→markdown), ft-002 (raw=true), ft-003 (raw=false), ft-004 (non-HTML), ft-005 (max_length truncation + hint, exact slice), ft-006 (start_index resume, exact slice), ft-007 (user-agent observation), ft-008 (robots disallow → error), ft-009 (robots allow), ft-010 (robots missing/404), ft-011 (HTTP 404 → error) | robots 401/403/500, invalid URL/scheme, max_length/start_index bounds, prompt error paths (`tests/fetch_server.rs`) |

All fetch fixtures are deterministic `tiny_http` servers on `127.0.0.1` —
no public internet dependency. The `fetch` prompt is additionally consumed
through one bounded real request (see below).

### Memory server (`mcp-memory`)

| Tool                  | Parameters                                                      | Online cases                                                     | Offline coverage     |
| --------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------- | -------------------- |
| `create_entities`     | `entities` (req, array of `{name, entityType, observations[]}`) | mem-000 (empty), mem-001 (one), mem-002 (multiple; nested lists) | duplicate-name skip  |
| `create_relations`    | `relations` (req, array of `{from, to, relationType}`)          | mem-003 (empty), mem-004 (one), mem-005 (multiple)               | duplicate skip       |
| `add_observations`    | `observations` (req, array of `{entityName, contents[]}`)       | mem-006 (multiple contents)                                      | unknown entity error |
| `delete_entities`     | `entityNames` (req, string[])                                   | mem-013 (multiple; relations pruned)                             | unknown names        |
| `delete_observations` | `deletions` (req, array of `{entityName, observations[]}`)      | mem-011 (persisted deletion)                                     | unknown entity error |
| `delete_relations`    | `relations` (req, array of `{from,to,relationType}`)            | mem-012 (persisted deletion)                                     | unknown relations    |
| `read_graph`          | (none)                                                          | mem-007, mem-014, mem-015 (post-restart)                         | empty graph          |
| `search_nodes`        | `query` (req, string)                                           | mem-008 (match), mem-009 (no match)                              | —                    |
| `open_nodes`          | `names` (req, string[])                                         | mem-010 (multiple names)                                         | unknown names        |

State oracles: each mutation verifies the JSONL file on disk (persistence)
and graph structure; mem-015 respawns the server on the same file and
verifies the state survives (restart persistence). The
`memory://knowledge-graph` resource is read through MCP and consumed through
one bounded real request (see below). Subscription behavior
(`subscriptions/listen`, `resources/subscribe`) remains offline-only.

### Shell server (`mcp-shell`)

| Tool              | Parameters                                                                                                                                                     | Online cases                                                                                                                                                                                                                                                                                                                                           | Offline coverage                                                                                   |
| ----------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------- |
| `execute_command` | `program` (req, string); `args` (opt, string[], default []); `cwd` (opt, string, default first allowed dir); `timeout_ms` (opt, int 1..600000, default 120000) | sh-001 (argv/stdout/exit 0; cwd omitted), sh-002 (args omitted → usage error), sh-003 (argv → nonzero+stderr), sh-004 (argv preserved, helper echo), sh-005 (stderr capture), sh-006 (exit 7), sh-007 (cwd provided), sh-008 (cwd omitted → default), sh-009 (timeout_ms provided → timed out), sh-010 (stdout truncated at 1 MiB; timeout_ms omitted) | empty program, cwd escapes/symlinks/non-dir, spawn failure, drain bounds (`tests/shell_server.rs`) |

Shell cases run in an isolated temp cwd; the helper is this test binary
re-invoked with `--exact e2e_helper_command <mode>` (argv/cwd/exit/stdout/
stderr/sleep/big modes). No destructive commands are executed.

## Protocol flows (online)

| Flow                                           | Assertions                                                                                                                                      | Real request                                 |
| ---------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------- |
| `server/discover` + identity                   | per server: protocol `2026-07-28`, capabilities, name/version, instructions                                                                     | — (MCP only)                                 |
| `tools/list`, `prompts/list`, `resources/list` | exact expected sets per server                                                                                                                  | — (MCP only)                                 |
| Forced tool roundtrip                          | every case: 1 forced tool call → MCP execution → result returned with matching `tool_call_id` → tools resent → bounded non-empty final response | 2 per case                                   |
| `prompts/get` (fetch)                          | prompt content = markdown of fixture page                                                                                                       | 1 consumption request incl. prompt content   |
| `resources/read` (memory)                      | resource JSON contains seeded fixture entity                                                                                                    | 1 consumption request incl. resource content |

## Offline-only boundaries (never online)

- Malformed/boundary rejection: bad argument types, missing required args,
  head+tail conflicts, invalid enum-like values, out-of-range numeric
  bounds — covered offline by `tests/*_server.rs` and the validator unit
  tests in `tests/openrouter/schema.rs`.
- Robots.txt 401/403/500 behaviors, proxy handling, HTML edge cases.
- `subscriptions/listen` + `resources/subscribe` notifications.
- Startup validation and CLI selection/conflict errors.

## Metrics captured (sanitized)

Per case: case ID, server, tool, status (ok/deviation/error), request IDs,
actual (resolved) model, token usage, elapsed, MCP status, retries.
Aggregate: real HTTP attempt count (including retries), MCP tool calls, full roundtrips,
prompt/resource consumption requests, tokens in/out, retries. Never printed:
API key, authorization headers, raw response bodies.

The aggregate numbers from the most recent verified acceptance run are
recorded on the [OpenRouter E2E](/openrouter-e2e) page.
