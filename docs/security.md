# Security model

This page states what each of the six servers is and — just as importantly — what it is
**not**. The servers run with the OS permissions of the MCP server process;
none of them is a sandbox. Connect them only to clients you trust, and
never expose them over an untrusted network.

## Filesystem server: path and symlink controls

All filesystem operations are restricted to the directories passed on the
command line (`AccessControl` in `src/support/access.rs`).

- **Roots are canonicalized at startup.** Each allowed directory is stored
  both as given (lexically normalized) and as canonicalized. This fixes
  symlinked-path mismatches such as macOS `/tmp` → `/private/tmp`: a path
  requested as `/tmp/x` still validates when the allowed root was given as
  `/private/tmp`.
- **Requested paths are lexically normalized** (`.` collapsed, `..`
  resolved where possible, redundant separators stripped) before any
  check, blocking `..` escapes like `/allowed/../../etc/passwd`.
- **`~` is expanded** to the user's home directory for both roots and
  requested paths.
- **Relative paths resolve against the first allowed directory.**
- **Symlink escapes are rejected.** For existing paths, the canonicalized
  target must still be inside an allowed root. For paths that do not exist
  yet (new files/directories), the deepest existing ancestor is
  canonicalized and must resolve inside an allowed root — so whole trees
  can be created at once without allowing a symlinked ancestor to escape.
- **Writes are atomic.** `write_file`/`edit_file` write a unique temp file
  next to the target and rename it into place, so a symlink that appears
  between validation and the write is never followed.
- Search and tree walks re-validate every visited entry against the
  allowed roots and silently skip anything that resolves outside.

This is access control, not a sandbox: within an allowed directory the
server can do anything the process user can do.

## Fetch server: network exposure

The fetch server makes real HTTP(S) requests on behalf of the model. Key
facts:

- **It can reach local and internal IP addresses.** There is no SSRF
  protection: `http://127.0.0.1/...`, link-local, or RFC 1918 addresses are
  fetchable. Only enable this server when you trust the clients connected
  to it.
- **Only `http` and `https` schemes are accepted**; anything else is
  rejected before any network I/O.
- **Every request has a 30-second timeout.**
- **robots.txt policy** (for the `fetch` tool, mirroring the reference
  server):
  - robots.txt unreachable → the fetch is blocked with an error;
  - robots.txt returns 401/403 → blocked (assumed not allowed);
  - any other 4xx (for example 404) → allowed;
  - otherwise the robots.txt is parsed and matched against the autonomous
    user agent; a disallow rule blocks the fetch.
  - `--ignore-robots-txt` disables these checks for the tool.
  - The `fetch` **prompt** never checks robots.txt: it is user-initiated
    and uses the "User-Specified" user agent instead.
- Default user agents (customizable with `--user-agent`):
  - autonomous (tool): `ModelContextProtocol/1.0 (Autonomous; +https://github.com/modelcontextprotocol/servers)`
  - manual (prompt): `ModelContextProtocol/1.0 (User-Specified; +https://github.com/modelcontextprotocol/servers)`
- Response bodies are decoded as text (charset-aware); HTML is simplified
  to markdown with `style`, `script`, `noscript`, and `template` content
  suppressed rather than leaked into model output.

## Memory server: process permissions and plaintext data

- The knowledge graph is persisted as a **plaintext JSONL file**, written
  with the OS permissions of the server process. There is no encryption.
- The file location is resolved in this order: `--memory-file` flag, then
  the `MEMORY_FILE_PATH` environment variable, then `memory.jsonl` in the
  current working directory.
- A corrupt file fails loads with an explicit error (never silently
  truncated), and concurrent mutations are serialized with a mutex.
- The file format matches the reference memory server
  (`{"type":"entity",...}` / `{"type":"relation",...}` lines), so a file
  written by this server is readable by the reference server and vice
  versa.
- Data at rest is protected only by filesystem permissions; treat the
  memory file like any other sensitive local data.

## Shell server: not a sandbox, no command filter

`execute_command` runs one local program directly (no shell) with the **OS
permissions of the MCP server process**: user identity, environment,
network access, filesystem access — everything the user can do.

- The `cwd` restriction only constrains the **working directory** of each
  command. A program running in an allowed directory can still read, write,
  and execute anything the user can. **The cwd restriction is not a
  sandbox.**
- There is **no command allow/deny filter**. This is deliberate:
  blacklisting strings like `rm` or `sudo` would be ineffective and is
  intentionally avoided.
- There is no implicit shell (`sh -c` / `bash -c` / `cmd /C` are never
  spawned), so shell metacharacters in arguments are inert — but a model
  can trivially run a shell itself (`program: "bash", args: ["-lc", ...]`),
  which is documented behavior.
- Guards that do exist: `cwd` must resolve inside an allowed directory
  (same validation as the filesystem server, including symlink checks),
  `timeout_ms` terminates runaway processes (with `kill_on_drop` reaping
  the child even if the request is cancelled), and stdout/stderr capture is
  bounded to 1 MiB per stream.
- Only enable this server for clients you trust completely, and never
  expose it over a network you do not control.

## CLI-level protections

- Exactly one of six servers can be selected, and server-specific options may only
  be combined with their server (see [CLI](/cli)); misconfigurations fail
  loudly instead of being silently ignored.
- Startup validation rejects unusable configurations (for example no
  accessible allowed directory) before serving.

## Logging and secrets

- All logs go to stderr (never stdout, which is reserved for JSON-RPC).
- The E2E acceptance suite sanitizes its output: the API key, authorization
  headers, and raw response bodies are never printed (see
  [OpenRouter E2E](/openrouter-e2e)).

## Skills server: instructions, not execution

Skills are repository content. The server constrains discovery and manifests
to the selected workspace and rejects escaping symlink targets, but that is
workspace access control rather than an OS sandbox. `activate_skill` returns
instructions and resource paths only: it never automatically executes a skill
body, script, command, or resource. Review and trust the workspace before
enabling this server.

## Agents server: external authority

Agents send prompts to their configured external model provider and can start
configured child MCP commands or call child HTTP servers. Definitions and
skills are therefore trusted workspace inputs, not a security boundary.
Provider credentials come only from the environment variable named by
`env_key`; literal key/token fields are rejected. Do not commit or log those
environment values. Agent permission, sandbox, and isolation fields do not
create OS-level isolation. Enable agents only for a workspace, client, and
child-server configuration you trust.
