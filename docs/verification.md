# Verification

How to verify that the binary is correct, safe to use, and behaves as
documented. Everything here is reproducible locally — the gated real-network
suite is optional.

## Offline gates (what CI runs)

These are the authoritative correctness gates; they are fast and need no
network, secrets, or tokens:

```bash
cargo fmt --all -- --check                              # formatting
cargo clippy --all-targets --all-features -- -D warnings # lint, warnings are errors
cargo test --all-targets --all-features --locked        # unit + integration
cargo build --release                                   # release build sanity
```

The integration suites (`tests/*_server.rs`) drive the compiled binary over
stdio with the rmcp client and assert, per server:

- the full tool inventory and every parameter (plus `tools/list` cache
  hints `ttlMs: 0` / `cacheScope: public`);
- access-control edge cases: `..` traversal, symlink escapes, relative
  paths, non-existent targets, multiple roots, `~` expansion;
- robots.txt behaviors (missing/404, allow, disallow, 401/403, server
  errors), truncation pagination, user agents, and the fetch prompt;
- the memory JSONL lifecycle, persistence across restarts, the
  `memory://knowledge-graph` resource, and both subscription flows;
- shell argv/cwd/timeout/exit-code/truncation behavior and cwd validation.

## Quick manual smoke tests

```bash
# CLI sanity: usage on stderr, exit code 1 (no server selected)
modelcontextprotocol

# Version and help
modelcontextprotocol --version
modelcontextprotocol --help

# Invalid combinations are rejected loudly
modelcontextprotocol filesystem /tmp --ignore-robots-txt   # exits 1
modelcontextprotocol fetch --memory-file /tmp/x.jsonl      # exits 1
```

To observe a live session, connect any MCP client that can run a stdio
server. A minimal check with a generic client: start
`modelcontextprotocol memory --memory-file /tmp/smoke.jsonl`, list tools
(expect 9 memory tools), call `create_entities` with one entity, call
`read_graph`, and confirm `memory://knowledge-graph` appears in
`resources/list`.

## Interactive inspection with the MCP Inspector

The [MCP Inspector](https://github.com/modelcontextprotocol/inspector) can
drive the binary interactively. These commands are examples of how to wire
it up; the Inspector itself is not part of this repository's test suite and
no claim is made that it has been run against this binary:

```bash
npx @modelcontextprotocol/inspector --cli ./target/release/modelcontextprotocol \
  filesystem /path/to/dir --method tools/list

npx @modelcontextprotocol/inspector --cli ./target/release/modelcontextprotocol \
  fetch --method tools/call --tool-name fetch --tool-arg url=https://example.com

# Server flags go after the `--` separator (inspector CLI rule)
npx @modelcontextprotocol/inspector --cli ./target/release/modelcontextprotocol \
  memory --memory-file /tmp/memory.jsonl -- --method tools/call \
  --tool-name create_entities --tool-arg 'entities=[{"name":"alice","entityType":"person","observations":[]}]'
```

## Real-network acceptance (optional, gated)

The OpenRouter E2E suite (`tests/openrouter_e2e.rs`) is the strongest
verification this repository offers: it runs every tool through a real
model-mediated roundtrip and consumes the fetch prompt and memory resource
through bounded real requests. It is `#[ignore]`d by default, requires
`OPENROUTER_API_KEY`, and spends tokens — see
[OpenRouter E2E](/openrouter-e2e) for the run command, bounds, and the most
recent verified run.

## What verification does not cover

- Compatibility with clients using the legacy `initialize` handshake is
  inherited from rmcp and is not locally acceptance-tested (see
  [Protocol](/protocol)).
- Behavior of `move_file` when the destination already exists is
  platform-dependent (Unix `rename` may replace; Windows can fail) and is
  not asserted (see [Filesystem server](/servers/filesystem)).
- No sandboxing guarantees: verification confirms the documented access
  controls, not isolation of the process user (see [Security model](/security)).
