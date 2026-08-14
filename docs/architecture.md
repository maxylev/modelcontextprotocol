# Architecture

A single Rust binary, four independent MCP servers. The crate is a
workspace-free package (`Cargo.toml`, edition 2024, MSRV 1.97) whose only
binary target is `modelcontextprotocol`.

## Source layout

```
src/
├── main.rs            # entry point: tracing init, CLI parse, dispatch
├── cli.rs             # clap definitions, flag/subcommand normalization, usage
├── fs/                # filesystem server
│   ├── mod.rs         #   server + 14 tools
│   ├── edit.rs        #   line-based edits + git-style diff rendering
│   ├── format.rs      #   size formatting, head/tail line helpers
│   └── search.rs      #   glob search + directory tree
├── fetch/             # fetch server
│   ├── mod.rs         #   server, fetch tool + fetch prompt
│   └── http.rs        #   reqwest client, robots.txt logic, HTML→markdown
├── memory/            # memory server
│   ├── mod.rs         #   server, 9 tools, resource + subscriptions
│   └── graph.rs       #   knowledge graph, JSONL persistence
├── shell/             # shell server
│   ├── mod.rs         #   server, execute_command tool
│   └── drain.rs       #   bounded stream capture
└── support/           # shared, server-neutral code
    ├── mod.rs         #   SPEC_VERSION, text/error result helpers
    └── access.rs      #   AccessControl: roots, normalization, symlink checks
```

**Dependency direction:** `main`/CLI → concrete servers (`fs`, `fetch`,
`memory`, `shell`) → `support` → external crates. Concrete servers never
depend on one another; everything they share lives in `support`.

## Key design decisions

- **One binary, four identities.** Each server advertises a distinct
  implementation name (`mcp-filesystem`, `mcp-fetch`, `mcp-memory`,
  `mcp-shell`) with the crate version, so a single install serves all four
  MCP server entries in a client configuration.
- **Two CLI styles.** `Cli::into_command` normalizes the subcommand and
  flag forms into one command and rejects ambiguous or conflicting
  invocations before any server starts (see [CLI](/cli)).
- **Shared path security.** `AccessControl` (used by filesystem and shell)
  canonicalizes allowed roots once and validates every requested path —
  lexical normalization, `~` expansion, relative-path resolution against
  the first root, and canonicalization with symlink-escape rejection (see
  [Security model](/security)).
- **rmcp protocol layer.** Servers are `ServerHandler` implementations over
  rmcp 3.1 (`transport-io`), with tool/prompt routers driven by the
  `#[tool]` / `#[prompt]` macros and schemars-generated JSON Schemas. The
  protocol version is `2026-07-28` (see [Protocol](/protocol)).
- **Async throughout.** tokio (multi-thread runtime) with `fs`, `process`,
  `time`, and `io-util` features; all file, process, and network I/O is
  async.
- **Small release binary.** `opt-level = "z"`, `lto = "fat"`,
  `codegen-units = 1`, `strip = true`; TLS uses the `ring` backend
  (reqwest `rustls-no-provider`, with the provider installed explicitly at
  startup) instead of aws-lc-rs. The resulting binary is about 5 MB.

## Concurrency within a server

- **Memory:** a tokio mutex serializes graph load/modify/save so concurrent
  mutations cannot interleave; a broadcast channel (capacity 16) fans
  graph-change notifications out to subscribed clients, and
  `subscriptions/listen` forwards them while honoring cancellation.
- **Shell:** stdout/stderr are drained concurrently by two tasks, each
  bounded to 1 MiB (the pipe keeps being drained past the limit so the
  child never blocks); the child is killed and reaped on timeout, and
  `kill_on_drop` reaps it if the request is cancelled.
- **Fetch:** one shared reqwest client with a fixed 30-second request
  timeout; robots.txt checks and fetches share the same client.

## Testing architecture

- `tests/*_server.rs` — offline integration suites that spawn the real
  binary over stdio with the rmcp client and cover every tool parameter,
  access-control edge case, robots.txt behavior, truncation pagination,
  user agents, prompts, resources, subscriptions, and persistence.
- `tests/common/mod.rs` — shared protocol helpers.
- `tests/openrouter_e2e.rs` + `tests/openrouter/` — the gated real-network
  acceptance suite: case catalog, harness, schema normalizer/validator, and
  sanitized metrics (see [OpenRouter E2E](/openrouter-e2e) and the
  [Coverage matrix](/coverage)).

## Docs site

The documentation you are reading is built with VitePress 2
(2.0.0-alpha.19) from `docs/`, with a small custom theme in
`docs/.vitepress/theme/` (default theme + custom CSS, no components).
`cargo doc` output is copied into the built site under `/rustdoc/` by the
publishing workflow (see [CI & publishing](/ci-publishing)).
