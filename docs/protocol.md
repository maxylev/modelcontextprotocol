# Protocol

Every server in the binary implements the Model Context Protocol
specification **2026-07-28** (the crate pins `SPEC_VERSION` in
`src/support/mod.rs`). The protocol layer is provided by the official Rust
SDK, [`rmcp`](https://crates.io/crates/rmcp) 3.1, over a stdio transport.

## Stateless 2026-07-28 flow

The 2026-07-28 protocol is stateless: the legacy `initialize` handshake is
not required, and clients can instead use the optional `server/discover`
RPC to learn the server's identity and capabilities in one round trip.
The rmcp client used by the test suites connects with
`ClientLifecycleMode::Discover` and negotiates `V_2026_07_28`; the
acceptance suite verifies the negotiated version on every connection.

**Compatibility with older `initialize`-based clients:** version
negotiation for the legacy flow is inherited from rmcp and is not
acceptance-tested in this repository. Treat it as inherited library
behavior, not a locally verified guarantee.

## Stdio cleanliness

- **stdout** carries only JSON-RPC messages. Anything written to stdout
  would corrupt the protocol, so all logging goes to **stderr** (tracing
  fmt layer, ANSI disabled).
- The server stays quiet until the client speaks first and serves
  requests in sequence over the stdio transport.

## List cache hints

`tools/list` responses carry the 2026-07-28 cache hints:
`ttlMs: 0` and `cacheScope: "public"`. This is asserted in all four server
integration suites (`tests/*_server.rs`).

Cache hints on `prompts/list` and `resources/list` are **not** documented
here: they are not asserted by any test, so no claim is made about them.

## Server identities and capabilities

| Server     | Implementation name | Version               | Capabilities                             |
| ---------- | ------------------- | --------------------- | ---------------------------------------- |
| filesystem | `mcp-filesystem`    | crate version (0.1.0) | tools                                    |
| fetch      | `mcp-fetch`         | crate version (0.1.0) | tools, prompts                           |
| memory     | `mcp-memory`        | crate version (0.1.0) | tools, resources, resource subscriptions |
| shell      | `mcp-shell`         | crate version (0.1.0) | tools                                    |

Each server also publishes `instructions` text through discovery, describing
how clients should use it (for example, filesystem instructs clients to use
`list_allowed_directories` first).

## Tools and prompts

- **25 tools total:** 14 filesystem + 1 fetch + 9 memory + 1 shell. Every
  tool, parameter, default, and bound is documented on the per-server pages:
  [Filesystem](/servers/filesystem), [Fetch](/servers/fetch),
  [Memory](/servers/memory), [Shell](/servers/shell).
- **1 prompt:** `fetch` on the fetch server (takes `url`).
- Tool schemas are generated at compile time with `schemars` and exposed
  through rmcp; there are no handwritten schema duplicates.

### Tool annotations

Every tool carries `ToolAnnotations` so clients can decide how to present
and cache calls:

- `readOnlyHint` — true for all read-only tools (for example
  `read_text_file`, `list_directory`, `read_graph`, the `fetch` tool).
- `idempotentHint` / `destructiveHint` — set per tool (for example
  `write_file` is destructive and idempotent; `execute_command` is
  destructive and not idempotent).
- `openWorldHint` — `false` for filesystem/memory/shell tools (they only
  touch the local machine within configured bounds), `true` for the fetch
  tool and `execute_command` (they interact with the outside world).

## Resources and subscriptions (memory server)

- Resource `knowledge-graph` at `memory://knowledge-graph`
  (`application/json`), containing the full graph in the same shape as the
  `read_graph` tool.
- Modern (2026-07-28) subscription flow: the server accepts
  `resource_subscriptions` for that URI
  (`accepted_subscription_filter`) and holds `subscriptions/listen` open,
  forwarding a `notifications/resources/updated` notification on every
  graph mutation.
- Legacy compatibility: mutation tools also broadcast to legacy
  `resources/subscribe` listeners through the same notification channel.
- Subscription behavior is covered by the offline suites; the resource
  itself is additionally consumed through one bounded real OpenRouter
  request in the acceptance suite (see [OpenRouter E2E](/openrouter-e2e)).

## Result shape

Tool results are plain text content blocks, plus structured content where
it matters:

- memory tools return `structuredContent` (the graph or the mutation
  result) alongside text.
- `execute_command` returns the full structured `CommandOutput` in
  `structuredContent` with a concise text summary; see
  [Shell server](/servers/shell).
- Errors are returned as tool-level errors (`isError: true`) with a
  model-readable text message — the servers never crash the transport on
  bad input.
