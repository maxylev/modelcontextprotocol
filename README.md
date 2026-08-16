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

### One-line installer (macOS / Linux / Android)

```bash
curl -fsSL https://github.com/maxylev/modelcontextprotocol/releases/latest/download/install.sh | bash
```

The script detects your OS and CPU, downloads the matching prebuilt binary
(MSRV build), verifies its SHA-256 checksum, and installs it to
`~/.local/bin` (`$PREFIX/bin` under Termux). Override the directory with
`INSTALL_DIR`:

```bash
curl -fsSL https://github.com/maxylev/modelcontextprotocol/releases/latest/download/install.sh | INSTALL_DIR=/usr/local/bin bash
```

Requires `curl`, `tar`, and `sha256sum` (Linux/Android) or `shasum` (macOS).
The script itself is in the repository root (`install.sh`).

### Prebuilt binaries (no Rust required)

Every `v*` tag publishes prebuilt binaries to
[GitHub Releases](https://github.com/maxylev/modelcontextprotocol/releases).
No Rust or Cargo needed: download your platform's archive below, extract it,
and put the `modelcontextprotocol` executable in your `bin` folder (or
anywhere on your `PATH`).

Direct links to the **latest** version of each package (replace
`rust-msrv` with `rust-stable` for the build made with the latest stable
Rust):

| Platform | Latest download |
| --- | --- |
| macOS — Apple Silicon | [modelcontextprotocol-aarch64-apple-darwin-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-apple-darwin-rust-msrv.tar.gz) |
| macOS — Intel | [modelcontextprotocol-x86_64-apple-darwin-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-x86_64-apple-darwin-rust-msrv.tar.gz) |
| Linux — x86_64 (glibc) | [modelcontextprotocol-x86_64-unknown-linux-gnu-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-x86_64-unknown-linux-gnu-rust-msrv.tar.gz) |
| Linux — aarch64 (glibc) | [modelcontextprotocol-aarch64-unknown-linux-gnu-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-unknown-linux-gnu-rust-msrv.tar.gz) |
| Linux — x86_64 (static musl) | [modelcontextprotocol-x86_64-unknown-linux-musl-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-x86_64-unknown-linux-musl-rust-msrv.tar.gz) |
| Linux — aarch64 (static musl) | [modelcontextprotocol-aarch64-unknown-linux-musl-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-unknown-linux-musl-rust-msrv.tar.gz) |
| Linux — armv7 (static musl) | [modelcontextprotocol-armv7-unknown-linux-musleabihf-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-armv7-unknown-linux-musleabihf-rust-msrv.tar.gz) |
| Windows — x86_64 | [modelcontextprotocol-x86_64-pc-windows-msvc-rust-msrv.zip](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-x86_64-pc-windows-msvc-rust-msrv.zip) |
| Windows — ARM64 | [modelcontextprotocol-aarch64-pc-windows-msvc-rust-msrv.zip](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-pc-windows-msvc-rust-msrv.zip) |
| Android — aarch64 | [modelcontextprotocol-aarch64-linux-android-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-linux-android-rust-msrv.tar.gz) |
| Android — x86_64 | [modelcontextprotocol-x86_64-linux-android-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-x86_64-linux-android-rust-msrv.tar.gz) |
| Android — armv7 | [modelcontextprotocol-armv7-linux-androideabi-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-armv7-linux-androideabi-rust-msrv.tar.gz) |
| iOS — aarch64 (unsigned; see note below) | [modelcontextprotocol-aarch64-apple-ios-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-apple-ios-rust-msrv.tar.gz) |

The `-musl` Linux builds are fully static and run on any distribution; the
Windows builds link the CRT statically (no VC++ runtime needed). Not sure
about your architecture? Run `uname -m` (macOS/Linux) or `echo %PROCESSOR_ARCHITECTURE%` (Windows). Every release ships a `.sha256`
checksum file named after the archive base — e.g.
`modelcontextprotocol-aarch64-apple-darwin-rust-msrv.sha256` — covering the
archive and the `install.sh` script.

#### Install on macOS

```bash
curl -L -O https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-apple-darwin-rust-msrv.tar.gz
tar -xzf modelcontextprotocol-aarch64-apple-darwin-rust-msrv.tar.gz
sudo install -m 755 modelcontextprotocol /usr/local/bin/
```

(On Intel Macs, use the `x86_64-apple-darwin` archive above.)

#### Install on Linux

```bash
curl -L -O https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-x86_64-unknown-linux-musl-rust-msrv.tar.gz
tar -xzf modelcontextprotocol-x86_64-unknown-linux-musl-rust-msrv.tar.gz
sudo install -m 755 modelcontextprotocol /usr/local/bin/
```

(The static musl build works on every x86_64 distribution. On `aarch64`
devices — Raspberry Pi 4/5, Apple Silicon VMs — use the
`aarch64-unknown-linux-musl` archive, or `armv7-unknown-linux-musleabihf`
on 32-bit ARM.)

#### Install on Windows (PowerShell)

```powershell
Invoke-WebRequest -Uri "https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-x86_64-pc-windows-msvc-rust-msrv.zip" -OutFile modelcontextprotocol.zip
Expand-Archive modelcontextprotocol.zip -DestinationPath modelcontextprotocol
New-Item -ItemType Directory -Force -Path "$HOME\bin" | Out-Null
Move-Item modelcontextprotocol\modelcontextprotocol.exe "$HOME\bin\"
```

Then add `%USERPROFILE%\bin` to your `PATH` (System Properties → Environment
Variables, or `setx PATH "%PATH%;%USERPROFILE%\bin"` and reopen the
terminal).

#### Install on Android (Termux)

```bash
curl -L -O https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-linux-android-rust-msrv.tar.gz
tar -xzf modelcontextprotocol-aarch64-linux-android-rust-msrv.tar.gz
mv modelcontextprotocol "$PREFIX/bin/"
```

#### Verify a download

```bash
# macOS / Linux — download the archive, its checksum, and the installer,
# then verify all three against the release checksum file
curl -L -O https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-apple-darwin-rust-msrv.tar.gz
curl -L -O https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-apple-darwin-rust-msrv.sha256
curl -L -O https://github.com/maxylev/modelcontextprotocol/releases/latest/download/install.sh
shasum -a 256 -c modelcontextprotocol-aarch64-apple-darwin-rust-msrv.sha256

# Windows PowerShell
Get-FileHash modelcontextprotocol.zip -Algorithm SHA256
```

> **iOS note:** the iOS archive is an unsigned Mach-O binary. iOS has no
> user-installable `bin` folder, so it cannot be run on a device as-is; it
> is provided for signing and embedding into an app.

### From source

```bash
cargo install modelcontextprotocol
```

Or install directly from the repository:

```bash
cargo install --git https://github.com/maxylev/modelcontextprotocol
```

The release binary is about 5 MB. Building from source requires Rust 1.97+
(MSRV); the prebuilt binaries above run on Linux / macOS / Windows / Android.

## Quick start

Both a subcommand form and an equivalent flag form are supported so the
binary fits any MCP client. Subcommand form (recommended):

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "modelcontextprotocol",
      "args": ["filesystem", "~/Developer/my-project"]
    },
    "shell": {
      "command": "modelcontextprotocol",
      "args": ["shell", "~/Developer/my-project"]
    },
    "fetch": {
      "command": "modelcontextprotocol",
      "args": ["fetch", "--ignore-robots-txt", "--user-agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36"]
    },
    "memory": {
      "command": "modelcontextprotocol",
      "args": ["memory"]
    }
  }
}
```

Flag form (`--filesystem <DIR>`, `--fetch`, `--memory`, `--shell <DIR>`)
is equivalent. See the [CLI reference](https://maxylev.github.io/modelcontextprotocol/cli.html)
for all options, conflict rules, and environment variables.

### Adding to your MCP client

A tutorial with the exact config file, format, and commands for each client
is in the docs: [Adding the servers to your MCP client](https://maxylev.github.io/modelcontextprotocol/clients.html).

Covered clients:

| Client        | How to configure                                             |
| ------------- | ------------------------------------------------------------ |
| opencode v1/v2 | `mcp` key in `opencode.json` / `opencode.jsonc`, or `opencode mcp add` |
| Claude Desktop | `mcpServers` in `claude_desktop_config.json`                 |
| Claude Code   | `claude mcp add <name> -- modelcontextprotocol ...`, or `.mcp.json` |
| Codex CLI     | `[mcp_servers.*]` in `~/.codex/config.toml`, or `codex mcp add` |
| Pi agent      | `pi install npm:pi-mcp-adapter`, then `mcpServers` in `.mcp.json` / `~/.pi/agent/mcp.json` |
| Gemini CLI    | `mcpServers` in `~/.gemini/settings.json`, or `gemini mcp add` |
| Cursor        | `mcpServers` in `.cursor/mcp.json` / `~/.cursor/mcp.json`     |
| Windsurf, Zed, VS Code, Continue, Cline, Roo | see the tutorial table |

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
