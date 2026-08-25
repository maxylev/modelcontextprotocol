# Shell server

Executes local programs directly (no implicit shell) with the OS
permissions of the MCP server process, and with the working directory
restricted to configured allowed directories.

- **Identity:** `mcp-shell` (crate version)
- **Capabilities:** tools
- **Invocation:** `modelcontextprotocol shell [DIR ...]` or
  `modelcontextprotocol --shell [DIR ...]` — defaults to the process's current
  directory when no directory is supplied

> **Security warning:** this server grants arbitrary local command
> execution with the permissions of the MCP server process. The `cwd`
> restriction is **not a sandbox**, and there is **no command allow/deny
> filter** (deliberately: blacklisting strings like `rm` or `sudo` would be
> ineffective). Only connect it to clients you trust. See
> [Security model](/security).

## Tool: execute_command

Execute one local program directly with an explicit argv and wait for it
to finish or time out.

| Parameter    | Required | Default                 | Bounds / notes                                                                                                                                                                                      |
| ------------ | -------- | ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `program`    | yes      | —                       | executable name or path, resolved through PATH like a direct exec; never a shell command string. Empty/whitespace-only is an error                                                                  |
| `args`       | no       | `[]`                    | each argument preserved exactly: no shell parsing, quoting, or glob expansion                                                                                                                       |
| `cwd`        | no       | first allowed directory | must resolve inside an allowed directory (same path validation as the filesystem server, including symlink checks); relative paths resolve against the first allowed directory; must be a directory |
| `timeout_ms` | no       | `120000`                | 1..600000; out of range is an error. On expiry the process is terminated and `timed_out` is reported                                                                                                |

There is no implicit shell: the server never runs `sh -c`/`bash -c`/`cmd
/C` wrappers. If the model deliberately needs shell syntax, it runs an
installed shell itself, e.g. `program: "bash"` with `args: ["-lc", "cargo
test && git status"]` — which is documented, intended behavior.

### Result (structuredContent)

| Field              | Type            | Notes                                                                                           |
| ------------------ | --------------- | ----------------------------------------------------------------------------------------------- |
| `exit_code`        | integer \| null | numeric exit code, or `null` when no normal exit code is available (e.g. terminated on timeout) |
| `stdout`           | string          | captured standard output, lossy UTF-8, bounded to 1 MiB                                         |
| `stderr`           | string          | captured standard error, lossy UTF-8, bounded to 1 MiB                                          |
| `timed_out`        | boolean         | true when the process was terminated for exceeding `timeout_ms`                                 |
| `stdout_truncated` | boolean         | true when stdout exceeded the 1 MiB capture limit                                               |
| `stderr_truncated` | boolean         | true when stderr exceeded the 1 MiB capture limit                                               |

Behavior notes:

- A **non-zero exit code is a normal completed execution**, not a tool
  failure: `cargo test` exiting 101 returns a normal result with
  `exit_code: 101` and captured output. `isError: true` is reserved for
  execution-operation failures: empty `program`, invalid or out-of-range
  `timeout_ms`, disallowed/non-directory `cwd`, an unspawnable executable,
  or internal I/O failure.
- Timeout is a normal structured outcome (`timed_out: true`,
  `exit_code: null`), not an error.
- stdout and stderr are captured **separately and concurrently**. Past the
  1 MiB limit the pipe keeps being drained (the child never blocks) but
  additional bytes are discarded, and the matching truncation flag is set.
- The tool returns both a concise text summary (exit status, previews
  capped at 4000 characters, truncation notes) and the full payload in
  `structuredContent`.
- The child is killed and reaped on timeout, and is reaped even if the
  request future is cancelled (`kill_on_drop`).

## Example

```json
{
  "program": "cargo",
  "args": ["test", "--workspace"],
  "cwd": "/repo",
  "timeout_ms": 120000
}
```

## Annotations

`execute_command` carries `readOnlyHint: false`, `destructiveHint: true`,
`idempotentHint: false`, `openWorldHint: true` (it interacts with the
outside world and is not safe to retry blindly).
