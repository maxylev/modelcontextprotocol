# Command line (CLI)

The binary is invoked as `modelcontextprotocol`. Exactly **one** server must
be selected, in one of two equivalent styles:

- **Subcommand form** — `modelcontextprotocol <server> [options] [DIR ...]`
- **Flag form** — `modelcontextprotocol --<server> [options] [DIR ...]`

The flag form exists so the binary can be wired into MCP client
configurations that cannot pass subcommands. Both forms are normalized into
the same command internally.

## Commands

| Command                      | Required arguments     | Options                                                         | Server           |
| ---------------------------- | ---------------------- | --------------------------------------------------------------- | ---------------- |
| `filesystem <DIR> [DIR ...]` | at least one directory | —                                                               | `mcp-filesystem` |
| `fetch`                      | —                      | `--ignore-robots-txt`, `--user-agent <UA>`, `--proxy-url <URL>` | `mcp-fetch`      |
| `memory`                     | —                      | `--memory-file <PATH>`                                          | `mcp-memory`     |
| `shell <DIR> [DIR ...]`      | at least one directory | —                                                               | `mcp-shell`      |

Equivalent flag forms:

| Flag form                      | Equivalent to                |
| ------------------------------ | ---------------------------- |
| `--filesystem <DIR> [DIR ...]` | `filesystem <DIR> [DIR ...]` |
| `--fetch`                      | `fetch`                      |
| `--memory`                     | `memory`                     |
| `--shell <DIR> [DIR ...]`      | `shell <DIR> [DIR ...]`      |

### Fetch options

| Option                      | Description                                                                                         |
| --------------------------- | --------------------------------------------------------------------------------------------------- |
| `--ignore-robots-txt`       | Skip robots.txt checks for the `fetch` tool (the `fetch` prompt never checks robots.txt regardless) |
| `--user-agent <USER_AGENT>` | Custom User-Agent header used for all requests, replacing both default agents                       |
| `--proxy-url <URL>`         | Route all requests through this HTTP(S) proxy                                                       |

### Memory options

| Option                 | Description                                                                              |
| ---------------------- | ---------------------------------------------------------------------------------------- |
| `--memory-file <PATH>` | Location of the memory JSONL file; overrides the `MEMORY_FILE_PATH` environment variable |

## Selection and conflict rules

`Cli::into_command` enforces strict selection rules; any violation prints
the usage summary to **stderr** and exits with code **1** instead of
silently ignoring options:

- Exactly one server selector (subcommand or top-level flag) must be
  present.
- Server-specific options may only be combined with the server they belong
  to. Examples of rejected invocations:
  - `--ignore-robots-txt` or `--proxy-url` with `filesystem`, `memory`, or
    `shell`
  - `--memory-file` with `fetch`, `filesystem`, or `shell`
  - Two selectors at once (for example `fetch --memory`)
  - `filesystem` with no directory (directories are `required` for
    filesystem and shell)

Server startup also validates its arguments:

- `filesystem` / `shell`: each directory is expanded (`~` → home), made
  absolute, and canonicalized. Directories that do not exist or are not
  directories are skipped with a warning; if no usable directory remains,
  the server exits with an error. See [Security model](/security).
- `memory`: the resolved memory file path is made absolute relative to the
  current working directory when needed.

## Environment variables

| Variable             | Used by        | Description                                                                                                                                                             |
| -------------------- | -------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `RUST_LOG`           | all servers    | `tracing` filter. Defaults to `modelcontextprotocol=warn` when unset. Logs go to **stderr** with ANSI colors disabled; stdout carries only JSON-RPC messages            |
| `MEMORY_FILE_PATH`   | memory         | Memory file location, overridden by `--memory-file`. Defaults to `memory.jsonl` in the current working directory                                                        |
| `OPENROUTER_API_KEY` | E2E suite only | API key for the ignored real-network acceptance suite (`tests/openrouter_e2e.rs`). Not read by any server                                                               |
| `OPENROUTER_MODEL`   | E2E suite only | Model override accepted by the acceptance suite for diagnostics; the acceptance default is the exact alias `~deepseek/deepseek-v4-flash-latest`. Not read by any server |

## Generic flags

- `--help` / `-h` — clap-generated help.
- `--version` / `-V` — prints the crate version (for example
  `modelcontextprotocol 0.1.0`).

## Exit behavior

- Selection/validation errors: usage or error message on stderr, exit code 1.
- Once running, a server serves MCP over stdio until the transport closes
  (the client disconnects or the parent process exits), then exits cleanly.
