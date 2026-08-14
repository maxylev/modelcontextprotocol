# Getting started

`modelcontextprotocol` is a single Rust binary that provides four Model
Context Protocol (MCP) servers over stdio:

| Server       | Identity         | What it does                                                        |
| ------------ | ---------------- | ------------------------------------------------------------------- |
| `filesystem` | `mcp-filesystem` | Secure read/write access restricted to allowed directories          |
| `fetch`      | `mcp-fetch`      | Fetch URLs, convert HTML to markdown, robots.txt enforcement        |
| `memory`     | `mcp-memory`     | Persistent knowledge-graph memory as JSONL                          |
| `shell`      | `mcp-shell`      | Execute local programs directly with a restricted working directory |

All four implement the MCP `2026-07-28` specification (see
[Protocol](/protocol)).

## Requirements

- A recent Rust toolchain (the crate declares MSRV 1.97 and edition 2024).
- A 64-bit desktop OS; the crate builds on Linux, macOS, and Windows (CI
  runs the test suite on all three).
- Node.js 24 is only needed to build and preview this documentation site,
  not to use the servers.

## Installation

From crates.io:

```bash
cargo install modelcontextprotocol
```

Or install directly from the source repository:

```bash
cargo install --git https://github.com/maxylev/modelcontextprotocol
```

Both install the `modelcontextprotocol` binary into your Cargo bin
directory. The release profile is tuned for a small binary (about 5 MB).

Verify the install:

```bash
modelcontextprotocol --version
modelcontextprotocol --help
```

## Configure your MCP client

Each server is selected on the command line. Both a subcommand form and an
equivalent flag form are supported, so the binary fits any client's
configuration style. The subcommand form is recommended.

Subcommand form:

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

Flag form:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "modelcontextprotocol",
      "args": ["--filesystem", "${HOME}/Developer"]
    },
    "fetch": {
      "command": "modelcontextprotocol",
      "args": ["--fetch"]
    },
    "memory": {
      "command": "modelcontextprotocol",
      "args": ["--memory"]
    },
    "shell": {
      "command": "modelcontextprotocol",
      "args": ["--shell", "${HOME}/Developer/my-project"]
    }
  }
}
```

See [Command line (CLI)](/cli) for the complete reference, including
options, conflict rules, and environment variables.

## Validate a running server

The fastest offline check is to run the binary without arguments: it prints
the usage summary to stderr and exits with code 1.

```bash
modelcontextprotocol
# modelcontextprotocol: exactly one server must be selected, ...
```

To inspect the live server (tools, prompts, resources), connect any MCP
client. A practical check is to launch the binary and use a generic stdio
client, or exercise it interactively with the MCP Inspector — see
[Verification](/verification) for worked examples.

## Read before you trust

- The [shell server](/servers/shell) executes arbitrary local programs with
  the OS permissions of the MCP server process. Its working-directory
  restriction is **not a sandbox** and there is **no command filter**.
- The [fetch server](/servers/fetch) can reach **local and internal network
  addresses**; there is no SSRF protection.
- Only connect these servers to clients you trust, and never expose them
  over an untrusted network. See [Security model](/security).

## Next steps

- [Servers reference](/servers/filesystem) — every tool, parameter, and bound.
- [Protocol](/protocol) — what the binary implements and what is verified.
- [OpenRouter E2E](/openrouter-e2e) — the gated real-network acceptance suite.
- [Coverage matrix](/coverage) — the case catalog behind the tests.
