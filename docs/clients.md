# Adding the servers to your MCP client

All six servers use the one `modelcontextprotocol` binary over stdio. Use a
separate entry per server. The examples use explicit workspace paths, but the
path can be omitted for `filesystem`, `shell`, `skills`, and `agents` when the
client launches MCP server processes in the intended workspace directory.

## Shared JSON shape

This conceptual `.mcp.json` shows all six. `${WORKSPACE}` and
`${OPENROUTER_API_KEY}` are environment-variable references for clients that
support such interpolation; otherwise replace the workspace with a literal
path and configure the agent credential using that client's supported
environment mechanism. No token value belongs in this file.

```json
{
  "mcpServers": {
    "filesystem": { "command": "modelcontextprotocol", "args": ["filesystem", "${WORKSPACE}"] },
    "fetch": { "command": "modelcontextprotocol", "args": ["fetch"] },
    "memory": { "command": "modelcontextprotocol", "args": ["memory"] },
    "shell": { "command": "modelcontextprotocol", "args": ["shell", "${WORKSPACE}"] },
    "skills": { "command": "modelcontextprotocol", "args": ["skills", "${WORKSPACE}"] },
    "agents": {
      "command": "modelcontextprotocol",
      "args": ["agents", "${WORKSPACE}"],
      "env": { "OPENROUTER_API_KEY": "${OPENROUTER_API_KEY}" }
    }
  }
}
```

`skills` reads repository instructions without executing them. `agents` can
contact providers and start configured child tools or commands; enable it only
in workspaces you trust.

## opencode (v1 and v2)

Put this under `mcp` in global or project `opencode.json`/`opencode.jsonc`.
opencode local servers use a command array:

```jsonc
{
  "mcp": {
    "filesystem": {
      "type": "local",
      "command": ["modelcontextprotocol", "filesystem", "~/Developer/my-project"],
      "enabled": true,
    },
    "fetch": { "type": "local", "command": ["modelcontextprotocol", "fetch"], "enabled": true },
    "memory": { "type": "local", "command": ["modelcontextprotocol", "memory"], "enabled": true },
    "shell": {
      "type": "local",
      "command": ["modelcontextprotocol", "shell", "~/Developer/my-project"],
      "enabled": true,
    },
    "skills": {
      "type": "local",
      "command": ["modelcontextprotocol", "skills", "~/Developer/my-project"],
      "enabled": true,
    },
    "agents": {
      "type": "local",
      "command": ["modelcontextprotocol", "agents", "~/Developer/my-project"],
      "enabled": true,
    },
  },
}
```

## Claude Desktop

Edit `claude_desktop_config.json` from Claude's Developer settings (macOS:
`~/Library/Application Support/Claude/`; Windows: `%APPDATA%\Claude\`):

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "modelcontextprotocol",
      "args": ["filesystem", "~/Developer/my-project"]
    },
    "fetch": { "command": "modelcontextprotocol", "args": ["fetch"] },
    "memory": { "command": "modelcontextprotocol", "args": ["memory"] },
    "shell": { "command": "modelcontextprotocol", "args": ["shell", "~/Developer/my-project"] },
    "skills": { "command": "modelcontextprotocol", "args": ["skills", "~/Developer/my-project"] },
    "agents": { "command": "modelcontextprotocol", "args": ["agents", "~/Developer/my-project"] }
  }
}
```

Restart Claude Desktop after editing.

## Claude Code

Add each stdio server, or use a project `.mcp.json` with the shared
`mcpServers` shape:

```bash
claude mcp add filesystem -- modelcontextprotocol filesystem ~/Developer/my-project
claude mcp add fetch -- modelcontextprotocol fetch
claude mcp add memory -- modelcontextprotocol memory
claude mcp add shell -- modelcontextprotocol shell ~/Developer/my-project
claude mcp add skills -- modelcontextprotocol skills ~/Developer/my-project
claude mcp add agents -- modelcontextprotocol agents ~/Developer/my-project
```

For a committed `.mcp.json`, give each shared entry `"type": "stdio"`.

## OpenAI Codex CLI

Add these tables to `~/.codex/config.toml` or `.codex/config.toml`:

```toml
[mcp_servers.filesystem]
command = "modelcontextprotocol"
args = ["filesystem", "~/Developer/my-project"]
[mcp_servers.fetch]
command = "modelcontextprotocol"
args = ["fetch"]
[mcp_servers.memory]
command = "modelcontextprotocol"
args = ["memory"]
[mcp_servers.shell]
command = "modelcontextprotocol"
args = ["shell", "~/Developer/my-project"]
[mcp_servers.skills]
command = "modelcontextprotocol"
args = ["skills", "~/Developer/my-project"]
[mcp_servers.agents]
command = "modelcontextprotocol"
args = ["agents", "~/Developer/my-project"]
```

## Pi agent

After `pi install npm:pi-mcp-adapter`, put the shared `mcpServers` JSON shape
in project `.mcp.json` or `~/.pi/agent/mcp.json`. Pi's shape is the same:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "modelcontextprotocol",
      "args": ["filesystem", "~/Developer/my-project"]
    },
    "fetch": { "command": "modelcontextprotocol", "args": ["fetch"] },
    "memory": { "command": "modelcontextprotocol", "args": ["memory"] },
    "shell": { "command": "modelcontextprotocol", "args": ["shell", "~/Developer/my-project"] },
    "skills": { "command": "modelcontextprotocol", "args": ["skills", "~/Developer/my-project"] },
    "agents": { "command": "modelcontextprotocol", "args": ["agents", "~/Developer/my-project"] }
  }
}
```

## Gemini CLI

Use the same `mcpServers` entries in `~/.gemini/settings.json` or
`.gemini/settings.json`:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "modelcontextprotocol",
      "args": ["filesystem", "~/Developer/my-project"]
    },
    "fetch": { "command": "modelcontextprotocol", "args": ["fetch"] },
    "memory": { "command": "modelcontextprotocol", "args": ["memory"] },
    "shell": { "command": "modelcontextprotocol", "args": ["shell", "~/Developer/my-project"] },
    "skills": { "command": "modelcontextprotocol", "args": ["skills", "~/Developer/my-project"] },
    "agents": { "command": "modelcontextprotocol", "args": ["agents", "~/Developer/my-project"] }
  }
}
```

## Cursor

Use `.cursor/mcp.json` or `~/.cursor/mcp.json`. Cursor supports `${env:VAR}`
in `command`, `args`, and `env`, so this shape can pass an agent credential:

```json
{
  "mcpServers": {
    "filesystem": {
      "type": "stdio",
      "command": "modelcontextprotocol",
      "args": ["filesystem", "${workspaceFolder}"]
    },
    "fetch": { "type": "stdio", "command": "modelcontextprotocol", "args": ["fetch"] },
    "memory": { "type": "stdio", "command": "modelcontextprotocol", "args": ["memory"] },
    "shell": {
      "type": "stdio",
      "command": "modelcontextprotocol",
      "args": ["shell", "${workspaceFolder}"]
    },
    "skills": {
      "type": "stdio",
      "command": "modelcontextprotocol",
      "args": ["skills", "${workspaceFolder}"]
    },
    "agents": {
      "type": "stdio",
      "command": "modelcontextprotocol",
      "args": ["agents", "${workspaceFolder}"],
      "env": { "OPENROUTER_API_KEY": "${env:OPENROUTER_API_KEY}" }
    }
  }
}
```

## Other clients

Windsurf, Zed, VS Code, Continue, Cline, and Roo Code use the same six binary
invocations in their documented MCP configuration shape. Add `skills` as
`modelcontextprotocol skills <workspace>` and `agents` as
`modelcontextprotocol agents <workspace>` alongside the existing filesystem,
fetch, memory, and shell entries;
adapt only the wrapper key (`context_servers` for Zed, `servers` for VS Code,
and the client's equivalent elsewhere). Use environment references only when
that client documents that syntax, and never put a real token in configuration.

Restart the client, then confirm all intended server entries and tools appear.
