# Command line (CLI)

Invoke the one binary as `modelcontextprotocol`. Select exactly one server in
either equivalent form; there is no configuration-file option.

```text
modelcontextprotocol filesystem <DIR> [DIR ...]
modelcontextprotocol fetch [--ignore-robots-txt] [--user-agent <UA>] [--proxy-url <URL>]
modelcontextprotocol memory [--memory-file <PATH>]
modelcontextprotocol shell <DIR> [DIR ...]
modelcontextprotocol skills <DIR>
modelcontextprotocol agents <DIR>

modelcontextprotocol --filesystem <DIR> [DIR ...]
modelcontextprotocol --fetch [--ignore-robots-txt] [--user-agent <UA>] [--proxy-url <URL>]
modelcontextprotocol --memory [--memory-file <PATH>]
modelcontextprotocol --shell <DIR> [DIR ...]
modelcontextprotocol --skills <DIR>
modelcontextprotocol --agents <DIR>
```

`skills` and `agents` each take exactly one positional workspace directory.
`filesystem` and `shell` each take one or more allowed directories.

| Server       | Identity         | Server-specific options                                         |
| ------------ | ---------------- | --------------------------------------------------------------- |
| `filesystem` | `mcp-filesystem` | none                                                            |
| `fetch`      | `mcp-fetch`      | `--ignore-robots-txt`, `--user-agent <UA>`, `--proxy-url <URL>` |
| `memory`     | `mcp-memory`     | `--memory-file <PATH>`                                          |
| `shell`      | `mcp-shell`      | none                                                            |
| `skills`     | `mcp-skills`     | none                                                            |
| `agents`     | `mcp-agents`     | none                                                            |

## Selection and validation

Selection is strict: exactly one subcommand or top-level selector must be
present. Any mixed selectors, missing required directory, or option belonging
to another server prints usage to stderr and exits 1. For example,
`fetch --memory`, `skills /workspace --user-agent UA`, and
`agents /workspace --memory-file memory.jsonl` are rejected. The two workspace
servers have no options beyond their one positional workspace.

Filesystem and shell roots are expanded, made absolute, and canonicalized;
unusable roots are warned about and startup fails when none remain. Skills and
agents canonicalize their one workspace and require it to be a directory.

## Environment variables

| Variable                                 | Used by                              | Description                                                                                                       |
| ---------------------------------------- | ------------------------------------ | ----------------------------------------------------------------------------------------------------------------- |
| `RUST_LOG`                               | all servers                          | `tracing` filter; defaults to `modelcontextprotocol=warn`. Logs use stderr only.                                  |
| `MEMORY_FILE_PATH`                       | memory                               | JSONL path, overridden by `--memory-file`; default `memory.jsonl` in the current directory.                       |
| `OPENAI_API_KEY`                         | agents default OpenAI definitions    | Default `env_key` for `model_provider: openai`.                                                                   |
| `ANTHROPIC_API_KEY`                      | agents default Anthropic definitions | Default `env_key` for `model_provider: anthropic`.                                                                |
| a definition's `env_key`                 | agents                               | Credential for that definition; custom definitions can use different names, such as `OPENROUTER_API_KEY`.         |
| `OPENROUTER_API_KEY`, `OPENROUTER_MODEL` | legacy OpenRouter E2E only           | Not read by the six MCP servers unless an agent definition explicitly uses `OPENROUTER_API_KEY` as its `env_key`. |

`--help`/`-h` prints clap help; `--version`/`-V` prints the crate version.
After startup, the selected server serves stdio JSON-RPC until the transport
closes.
