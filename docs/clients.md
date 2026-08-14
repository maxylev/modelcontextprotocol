# Adding the servers to your MCP client

All four servers ship in the single `modelcontextprotocol` binary and talk
over stdio, so every client config is the same shape: point the client at the
binary with a subcommand (or flag) selecting one server. This page shows the
exact file, format, and command for the popular clients.

Install the binary first if you have not already
([Getting started](/getting-started)):

```bash
cargo install modelcontextprotocol
```

Pick the directories you want to expose before wiring anything up. The
examples below use `~/Developer` for `filesystem` and `~/Developer/my-project`
for `shell`; adjust to taste. The `fetch` example enables the Mozilla user
agent and `--ignore-robots-txt` (see the [CLI reference](/cli) for what those
do); plain `modelcontextprotocol fetch` also works.

## The shared JSON shape

Most clients accept a JSON block that looks like this — the `mcpServers`
key, a server name, and a `command`/`args` pair:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "modelcontextprotocol",
      "args": ["filesystem", "~/Developer"]
    },
    "fetch": {
      "command": "modelcontextprotocol",
      "args": ["fetch", "--ignore-robots-txt", "--user-agent", "Mozilla/5.0"]
    },
    "memory": {
      "command": "modelcontextprotocol",
      "args": ["memory"]
    },
    "shell": {
      "command": "modelcontextprotocol",
      "args": ["shell", "~/Developer/my-project"]
    }
  }
}
```

Clients differ only in _where_ that block lives and whether they add wrapper
keys. The sections below give the exact per-client files. In every case,
restart the client after editing.

## opencode (v1 and v2)

opencode reads the `mcp` key from `opencode.json`/`opencode.jsonc`. Global
config lives at `~/.config/opencode/opencode.json` (macOS/Linux) or
`%USERPROFILE%\.config\opencode\opencode.json` (Windows); project config is
`opencode.json` in the project root. Both are merged, and JSONC comments are
allowed.

The format applies to both opencode v1 and v2. The only v2 additions over
v1 are the optional `cwd` and `timeout` fields — everything else, including
file locations and merge order, is identical.

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "filesystem": {
      "type": "local",
      "command": ["modelcontextprotocol", "filesystem", "~/Developer"],
      "enabled": true,
    },
    "fetch": {
      "type": "local",
      "command": [
        "modelcontextprotocol",
        "fetch",
        "--ignore-robots-txt",
        "--user-agent",
        "Mozilla/5.0",
      ],
      "enabled": true,
    },
    "memory": {
      "type": "local",
      "command": ["modelcontextprotocol", "memory"],
      "enabled": true,
    },
    "shell": {
      "type": "local",
      "command": ["modelcontextprotocol", "shell", "~/Developer/my-project"],
      "enabled": true,
    },
  },
}
```

Set `"enabled": false` to keep a server configured but not running.

Alternatively, add servers interactively:

```bash
opencode mcp add          # interactive wizard
opencode mcp list         # what is configured
opencode mcp debug memory # inspect a server's session
```

## Claude Desktop

Edit `claude_desktop_config.json`:

- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows: `%APPDATA%\Claude\claude_desktop_config.json`

via Claude menu → Settings… → Developer → **Edit Config**, or by hand:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "modelcontextprotocol",
      "args": ["filesystem", "~/Developer"]
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
      "args": ["shell", "~/Developer/my-project"]
    }
  }
}
```

Restart Claude Desktop completely after editing. Claude Desktop runs on macOS
and Windows only.

## Claude Code (CLI)

Add servers with the CLI (stdio is the default transport):

```bash
claude mcp add filesystem -- modelcontextprotocol filesystem ~/Developer
claude mcp add fetch -- modelcontextprotocol fetch
claude mcp add memory -- modelcontextprotocol memory
claude mcp add shell -- modelcontextprotocol shell ~/Developer/my-project
```

Or write the project-scoped `.mcp.json` at your project root and commit it to
share with teammates:

```json
{
  "mcpServers": {
    "filesystem": {
      "type": "stdio",
      "command": "modelcontextprotocol",
      "args": ["filesystem", "~/Developer"]
    },
    "fetch": {
      "type": "stdio",
      "command": "modelcontextprotocol",
      "args": ["fetch"]
    },
    "memory": {
      "type": "stdio",
      "command": "modelcontextprotocol",
      "args": ["memory"]
    },
    "shell": {
      "type": "stdio",
      "command": "modelcontextprotocol",
      "args": ["shell", "~/Developer/my-project"]
    }
  }
}
```

Scopes: `local` and `user` go into `~/.claude.json` (per project vs. global),
`project` goes into `.mcp.json`. Manage with `claude mcp list`, `claude mcp
get <name>`, `claude mcp remove <name>`, or in-session `/mcp`.

## OpenAI Codex CLI

Codex reads `[mcp_servers.*]` tables from `~/.codex/config.toml` (user) or
`.codex/config.toml` (project). Stdio servers use `command` plus `args`:

```toml
[mcp_servers.filesystem]
command = "modelcontextprotocol"
args = ["filesystem", "~/Developer"]

[mcp_servers.fetch]
command = "modelcontextprotocol"
args = ["fetch"]

[mcp_servers.memory]
command = "modelcontextprotocol"
args = ["memory"]

[mcp_servers.shell]
command = "modelcontextprotocol"
args = ["shell", "~/Developer/my-project"]
```

Or add them with the CLI:

```bash
codex mcp add filesystem -- modelcontextprotocol filesystem ~/Developer
codex mcp add memory -- modelcontextprotocol memory
codex mcp list
```

Enable/disable per session with `/mcp` in the TUI.

## Pi agent

Pi's MCP support ships as the `pi-mcp-adapter` extension. Install it once,
then restart:

```bash
pi install npm:pi-mcp-adapter
```

Configure servers in a standard MCP JSON file. Use the project-scoped
`.mcp.json` (preferred) or the user-global `~/.pi/agent/mcp.json`:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "modelcontextprotocol",
      "args": ["filesystem", "~/Developer"]
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
      "args": ["shell", "~/Developer/my-project"]
    }
  }
}
```

Pi also reads `~/.config/mcp/mcp.json`, `~/.agents/mcp.json`, and
`~/.agents/mcp/mcp.json`, in that order, with `.mcp.json` (and finally
`.pi/mcp.json`) taking precedence. Manage in-session with `/mcp`, or scaffold
from another tool's config with `/mcp setup` (or `pi-mcp-adapter init`).

## Gemini CLI

Gemini reads the `mcpServers` key from `~/.gemini/settings.json` (user) or
`.gemini/settings.json` (project; the CLI default scope is project):

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "modelcontextprotocol",
      "args": ["filesystem", "~/Developer"]
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
      "args": ["shell", "~/Developer/my-project"]
    }
  }
}
```

Or add servers with the CLI:

```bash
gemini mcp add filesystem -- modelcontextprotocol filesystem ~/Developer
gemini mcp add memory -- modelcontextprotocol memory
gemini mcp list
```

Manage with `gemini mcp remove|enable|disable <name>`, or in-session `/mcp`.

## Cursor

Cursor reads `mcpServers` from `.cursor/mcp.json` (project) or
`~/.cursor/mcp.json` (user). You can also use the Customize page
(sidebar) → MCP → "Add new MCP server" and paste the shared JSON block
above.

```json
{
  "mcpServers": {
    "filesystem": {
      "type": "stdio",
      "command": "modelcontextprotocol",
      "args": ["filesystem", "~/Developer"]
    },
    "fetch": {
      "type": "stdio",
      "command": "modelcontextprotocol",
      "args": ["fetch"]
    },
    "memory": {
      "type": "stdio",
      "command": "modelcontextprotocol",
      "args": ["memory"]
    },
    "shell": {
      "type": "stdio",
      "command": "modelcontextprotocol",
      "args": ["shell", "~/Developer/my-project"]
    }
  }
}
```

Cursor interpolates `${env:VAR}`, `${userHome}`, and `${workspaceFolder}` in
`command`, `args`, and `env`.

## Other clients

The same `mcpServers` JSON (or an equivalent) applies elsewhere:

| Client         | Where the config lives                                  | Key               | Notes                                    |
| -------------- | ------------------------------------------------------- | ----------------- | ---------------------------------------- |
| Windsurf       | `~/.codeium/mcp_config.json`                            | `mcpServers`      | Or Settings → Tools → Add Server         |
| Zed            | `~/.config/zed/settings.json` (or `.zed/settings.json`) | `context_servers` | Or Settings → AI → MCP Servers           |
| VS Code        | `.vscode/mcp.json` (or user `mcp.json`)                 | `servers`         | VS Code uses `servers`, not `mcpServers` |
| Continue       | `config.yaml` in your Continue config directory         | `mcpServers`      | YAML list of `{name, command, args}`     |
| Cline          | `~/.cline/mcp.json`                                     | `mcpServers`      | Or the IDE MCP Configure tab             |
| Roo Code       | `mcp_settings.json` (global) or `.roo/mcp.json`         | `mcpServers`      | Project file wins on name conflict       |
| Claude Desktop | see above                                               | `mcpServers`      | macOS/Windows                            |

For these, add the shared JSON block from the top of this page, adapting the
wrapper key per the table (for example, Zed expects `context_servers`, VS Code
expects `servers`).

## Verify

After wiring a client, confirm the servers appear and their tools load. A
quick offline check of the binary itself:

```bash
modelcontextprotocol --version
modelcontextprotocol --help
```

or drive a single server directly with any stdio MCP client — see
[Verification](/verification) for worked examples with the MCP Inspector.

## Security reminder

The `shell` and `fetch` servers are powerful: `shell` runs local programs with
the full permissions of the MCP server process, and `fetch` can reach local
and internal network addresses. Only connect them to clients you trust. See
the [Security model](/security) before enabling them anywhere.
