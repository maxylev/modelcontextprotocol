# OpenRouter E2E acceptance

The gated, real-network acceptance suite in `tests/openrouter_e2e.rs` is
the strongest verification in this repository. It exercises the whole stack
— real MCP servers, the real protocol lifecycle, and a real frontier model
over the real OpenRouter API.

## What the suite does

1. Spawns each of the four real servers in a disposable environment and
   connects via the modern `server/discover` lifecycle, asserting the
   negotiated protocol version (`2026-07-28`), server identity
   (`mcp-filesystem`, `mcp-fetch`, `mcp-memory`, `mcp-shell`),
   capabilities, and instructions.
2. Derives every OpenRouter function schema from the runtime MCP `Tool`
   definitions through a single deterministic normalizer/validator
   (`tests/openrouter/schema.rs`) — no duplicated handwritten schemas.
3. Runs one forced `assistant -> tool -> assistant` roundtrip per case:
   the model must call exactly the intended tool with exactly the intended,
   schema-valid arguments; the MCP tool executes; the result returns with
   the matching `tool_call_id`; tools are resent; the final answer must be
   bounded and non-empty. An independent programmatic oracle checks the MCP
   result and fixture side effects (disk content, JSONL state, content
   blocks).
4. Consumes the `fetch` prompt and the `memory://knowledge-graph` resource
   through one bounded real request each, with the content fed into the
   model.
5. Enforces coverage: `assert_coverage` requires exact set equality between
   the case catalog and the live tool inventory, plus per-parameter
   coverage (provided for every parameter; additionally omitted for
   optional parameters). Adding a tool or parameter without updating the
   catalog fails the suite.

Fetch correctness never depends on the public internet: every fetch case
uses a deterministic local `tiny_http` fixture (including robots.txt
modes). Shell cases use an isolated temp cwd and this test binary as a
deterministic helper; no destructive commands are ever executed.

## Model and bounds

- Endpoint: `POST https://openrouter.ai/api/v1/chat/completions`, Bearer
  auth from `OPENROUTER_API_KEY` (environment only).
- Required default model alias: `~deepseek/deepseek-v4-flash-latest`
  (OpenRouter `~` = latest of the family). `OPENROUTER_MODEL` is accepted
  as an override for diagnostics, but the acceptance run must use the
  default alias; the response `model` field reports the concrete resolved
  model.
- Forced tool shape `{"type":"function","function":{...}}`;
  `parallel_tool_calls: false`, `stream: false`, `temperature: 0`.
- Per-request: timeout 45 s, response body ≤ 1 MiB, `max_tokens` 256 per
  tool call and 200 per final answer/consumption request.
- Retry policy: at most 2 attempts per request, retrying only transport
  errors, 429s, 5xx, and `Retry-After` responses (backoff capped at 5 s).
  In addition, a whole roundtrip is retried exactly once for
  model-generation flakes only: a model-echo deviation (dropped/rewritten
  argument — the argument guard runs again and still blocks before any MCP
  execution) or an empty final answer cut off by the `length` finish
  reason. Oracle failures, schema failures, and server failures are never
  retried.
- Suite budget: 14 minutes 30 seconds; a hard abort still reports all
  failures collected so far.
- Tool results fed back to the model are capped at 4000 characters per call.

## How to run it

```bash
OPENROUTER_API_KEY=<key in the environment> \
env -u OPENROUTER_MODEL cargo test --test openrouter_e2e \
  -- --ignored --nocapture --test-threads=1
```

- The suite is **ignored by default** — ordinary `cargo test` runs stay
  offline, secret-free, and cost-free.
- Invocation without `OPENROUTER_API_KEY` fails clearly; the gating cannot
  silently skip.
- The key is read from the environment at runtime and is never logged,
  printed, or written anywhere. The summary prints only case metadata,
  request IDs, resolved model names, token usage, and elapsed times.

## Most recent verified acceptance run

The following numbers characterize the latest historical run of the suite
(2026-08-14). They are a record of that run, not a guarantee for future
runs: model names, token usage, and timing vary with OpenRouter's routing
and the resolved model version. The **repeatable guarantees** are the
harness invariants above (coverage assertions, argument guard, oracles,
bounds, retry policy).

| Metric                               | Value                                             |
| ------------------------------------ | ------------------------------------------------- |
| Requested model alias                | `~deepseek/deepseek-v4-flash-latest`              |
| Actual resolved model                | `deepseek/deepseek-v4-flash-0731`                 |
| Cases executed                       | 66 (29 filesystem, 16 memory, 11 fetch, 10 shell) |
| MCP tool calls                       | 66                                                |
| Full roundtrips                      | 66                                                |
| Real OpenRouter HTTP attempts        | 135 (including retries)                           |
| Transport retries                    | 1                                                 |
| Prompt/resource consumption requests | 2 (fetch prompt, memory resource)                 |
| Tokens (in / out)                    | 54,878 / 11,647                                   |
| Total elapsed                        | 404 s                                             |

All 66 cases passed with the programmatic oracles; the fetch prompt and the
memory resource were consumed successfully in real requests.

## Metrics captured

Per case: case ID, server, tool, status (ok/deviation/error), request IDs,
actual (resolved) model, token usage, elapsed, MCP status, retries.
Aggregate: real HTTP attempt count including retries, MCP tool calls, full
roundtrips, prompt/resource consumption requests, tokens in/out, retries.
Never printed: API key, authorization headers, raw response bodies.

The full per-tool case matrix is maintained in the [Coverage matrix](/coverage).
