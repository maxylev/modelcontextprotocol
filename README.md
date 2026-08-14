# modelcontextprotocol

[![CI](https://github.com/maxylev/modelcontextprotocol/actions/workflows/ci.yml/badge.svg)](https://github.com/maxylev/modelcontextprotocol/actions/workflows/ci.yml)
[![Docs](https://github.com/maxylev/modelcontextprotocol/actions/workflows/docs.yml/badge.svg)](https://github.com/maxylev/modelcontextprotocol/actions/workflows/docs.yml)

Four Model Context Protocol (MCP) servers in a single Rust binary,
implementing the [MCP `2026-07-28` specification](https://modelcontextprotocol.io/specification/2026-07-28)
over stdio:

| Server       | Identity         | What it does                                                                   |
| ------------ | ---------------- | ------------------------------------------------------------------------------ |
| `filesystem` | `mcp-filesystem` | Secure read/write access restricted to allowed directories                     |
| `fetch`      | `mcp-fetch`      | Fetch URLs, convert HTML to markdown, robots.txt enforcement                   |
| `memory`     | `mcp-memory`     | Persistent knowledge-graph memory (entities, relations, observations) as JSONL |
| `shell`      | `mcp-shell`      | Execute local programs directly with a restricted working directory            |

Built on the official Rust SDK (`rmcp` 3.1). The `2026-07-28` protocol is
stateless: `server/discover` without an `initialize` handshake, cache hints
on `tools/list`, and per-tool annotations.

**Full documentation: <https://maxylev.github.io/modelcontextprotocol/>**
(usage, CLI reference, protocol details, security model, tool reference for
all 25 tools, coverage matrix, and CI/CD notes).

## Installation

```bash
cargo install modelcontextprotocol
```

Or install directly from source:

```bash
cargo install --git https://github.com/maxylev/modelcontextprotocol
```

The release binary is about 5 MB. Requirements: Rust 1.97+ (MSRV), Linux /
macOS / Windows.

## Quick start

Both a subcommand form and an equivalent flag form are supported so the
binary fits any MCP client. Subcommand form (recommended):

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "modelcontextprotocol",
      "args": ["filesystem", "${HOME}/Developer"]
    },
    "fetch": {
      "command": "modelcontextprotocol",
      "args": ["fetch"]
    },
    "memory": {
      "command": "modelcontextprotocol",
      "args": ["memory"]
    },
    "shell": {
      "command": "modelcontextprotocol",
      "args": ["shell", "${HOME}/Developer/my-project"]
    }
  }
}
```

Flag form (`--filesystem <DIR>`, `--fetch`, `--memory`, `--shell <DIR>`)
is equivalent. See the [CLI reference](https://maxylev.github.io/modelcontextprotocol/cli.html)
for all options, conflict rules, and environment variables.

### Validate

```bash
modelcontextprotocol --version   # version
modelcontextprotocol             # prints usage to stderr, exits 1
```

Quick smoke test with a generic MCP client: start
`modelcontextprotocol memory --memory-file /tmp/smoke.jsonl`, list tools
(9 memory tools), call `create_entities`, call `read_graph`. More in the
[verification guide](https://maxylev.github.io/modelcontextprotocol/verification.html).

## Security — read before you trust

- **Shell server:** executes arbitrary local programs with the **OS
  permissions of the MCP server process**. The working-directory
  restriction is **not a sandbox**, and there is **no command filter**.
  Only connect it to clients you trust.
- **Fetch server:** can reach **local and internal network addresses**
  (no SSRF protection); only enable it for trusted clients. Requests are
  bounded by a 30-second timeout and robots.txt policy.
- Filesystem access is restricted to configured directories with symlink
  protection — access control, not a sandbox.
- See the [security model](https://maxylev.github.io/modelcontextprotocol/security.html)
  for details.

## Development

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --locked   # offline, secret-free
cargo build --release
```

A gated, real-network acceptance suite (`tests/openrouter_e2e.rs`, ignored
by default) drives every tool through real OpenRouter roundtrips; it
requires `OPENROUTER_API_KEY` and spends tokens. Run it with:

```bash
OPENROUTER_API_KEY=<key in the environment> \
env -u OPENROUTER_MODEL cargo test --test openrouter_e2e \
  -- --ignored --nocapture --test-threads=1
```

Docs site: `cd docs && npm install`, then `npm run docs:dev` (dev) or
`npm run docs:build` (build; output in `docs/.vitepress/dist`). Docs gates
are `npm run format:check` and `npm run docs:build`.

## Repository layout

- `src/` — the binary: `cli.rs` (CLI), `fs/`, `fetch/`, `memory/`,
  `shell/` (servers), `support/` (shared access control and helpers).
- `tests/` — integration suites per server plus the gated OpenRouter E2E
  harness (`openrouter/`).
- `docs/` — VitePress documentation site (deployed to GitHub Pages).
- `.github/workflows/` — CI (lint + test matrix), docs publishing, and
  crates.io publishing workflows.

## License

MIT
