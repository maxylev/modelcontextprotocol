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

- **Using the prebuilt binaries:** none — just a supported OS. No Rust
  toolchain needed.
- **Building from source:** a recent Rust toolchain (the crate declares MSRV
  1.97 and edition 2024).
- A 64-bit desktop OS; the prebuilt binaries also cover Windows ARM64,
  32-bit ARM Linux, Android, and iOS (details below).
- Node.js 24 is only needed to build and preview this documentation site,
  not to use the servers.

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

Requires `curl`, `tar`, and `sha256sum` (Linux/Android) or `shasum`
(macOS). The script lives in the repository root (`install.sh`).

### Prebuilt binaries (no Rust required)

Every `v*` tag publishes prebuilt binaries to
[GitHub Releases](https://github.com/maxylev/modelcontextprotocol/releases).
Download your platform's archive, extract it, and put the
`modelcontextprotocol` executable in your `bin` folder (or anywhere on your
`PATH`).

Direct links to the **latest** version of each package (replace `rust-msrv`
with `rust-stable` for the build made with the latest stable Rust):

- macOS — Apple Silicon
  ([modelcontextprotocol-aarch64-apple-darwin-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-apple-darwin-rust-msrv.tar.gz))
- macOS — Intel
  ([modelcontextprotocol-x86_64-apple-darwin-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-x86_64-apple-darwin-rust-msrv.tar.gz))
- Linux — x86_64, glibc
  ([modelcontextprotocol-x86_64-unknown-linux-gnu-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-x86_64-unknown-linux-gnu-rust-msrv.tar.gz))
- Linux — aarch64, glibc
  ([modelcontextprotocol-aarch64-unknown-linux-gnu-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-unknown-linux-gnu-rust-msrv.tar.gz))
- Linux — x86_64, static musl
  ([modelcontextprotocol-x86_64-unknown-linux-musl-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-x86_64-unknown-linux-musl-rust-msrv.tar.gz))
- Linux — aarch64, static musl
  ([modelcontextprotocol-aarch64-unknown-linux-musl-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-unknown-linux-musl-rust-msrv.tar.gz))
- Linux — armv7, static musl
  ([modelcontextprotocol-armv7-unknown-linux-musleabihf-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-armv7-unknown-linux-musleabihf-rust-msrv.tar.gz))
- Windows — x86_64
  ([modelcontextprotocol-x86_64-pc-windows-msvc-rust-msrv.zip](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-x86_64-pc-windows-msvc-rust-msrv.zip))
- Windows — ARM64
  ([modelcontextprotocol-aarch64-pc-windows-msvc-rust-msrv.zip](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-pc-windows-msvc-rust-msrv.zip))
- Android — aarch64
  ([modelcontextprotocol-aarch64-linux-android-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-linux-android-rust-msrv.tar.gz))
- Android — x86_64
  ([modelcontextprotocol-x86_64-linux-android-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-x86_64-linux-android-rust-msrv.tar.gz))
- Android — armv7
  ([modelcontextprotocol-armv7-linux-androideabi-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-armv7-linux-androideabi-rust-msrv.tar.gz))
- iOS — aarch64, unsigned (see [note](#ios-note) below)
  ([modelcontextprotocol-aarch64-apple-ios-rust-msrv.tar.gz](https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-apple-ios-rust-msrv.tar.gz))

The `-musl` Linux builds are fully static and run on any distribution; the
Windows builds link the CRT statically (no VC++ runtime needed). Not sure
about your architecture? Run `uname -m` (macOS/Linux) or
`echo %PROCESSOR_ARCHITECTURE%` (Windows). Every archive ships with a
`.sha256` checksum file named after the archive base — e.g.
`modelcontextprotocol-aarch64-apple-darwin-rust-msrv.sha256`.

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

Then add `%USERPROFILE%\bin` to your `PATH` (System Properties →
Environment Variables, or `setx PATH "%PATH%;%USERPROFILE%\bin"` and
reopen the terminal).

#### Install on Android (Termux)

```bash
curl -L -O https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-linux-android-rust-msrv.tar.gz
tar -xzf modelcontextprotocol-aarch64-linux-android-rust-msrv.tar.gz
mv modelcontextprotocol "$PREFIX/bin/"
```

#### Verify a download

```bash
# macOS / Linux
curl -L -O https://github.com/maxylev/modelcontextprotocol/releases/latest/download/modelcontextprotocol-aarch64-apple-darwin-rust-msrv.sha256
shasum -a 256 -c modelcontextprotocol-aarch64-apple-darwin-rust-msrv.sha256

# Windows PowerShell
Get-FileHash modelcontextprotocol.zip -Algorithm SHA256
```

#### iOS note

The iOS archive is an unsigned Mach-O binary. iOS has no user-installable
`bin` folder, so it cannot be run on a device as-is; it is provided for
signing and embedding into an app.

### From crates.io

```bash
cargo install modelcontextprotocol
```

### From the source repository

```bash
cargo install --git https://github.com/maxylev/modelcontextprotocol
```

All three install the `modelcontextprotocol` binary into your Cargo bin
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

- [Adding to MCP clients](/clients) — exact configs for opencode, Claude,
  Codex, Pi, Gemini, Cursor, and more.
- [Servers reference](/servers/filesystem) — every tool, parameter, and bound.
- [Protocol](/protocol) — what the binary implements and what is verified.
- [OpenRouter E2E](/openrouter-e2e) — the gated real-network acceptance suite.
- [Coverage matrix](/coverage) — the case catalog behind the tests.
