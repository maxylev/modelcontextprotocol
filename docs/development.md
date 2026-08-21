# Development

## Prerequisites

- Rust toolchain matching `rust-toolchain.toml` (1.97.x).
- Node.js 24 only for the docs site tooling.
- No other system dependencies: all network, TLS, and parsing needs are
  vendored crates.

## Standard commands

```bash
cargo fmt                    # format
cargo clippy --all-targets --all-features -- -D warnings   # lint
cargo test --all-targets --all-features --locked           # unit + integration
cargo build --release        # size-tuned release binary (~6.5 MB on macOS)
cargo doc --no-deps          # rustdoc for the docs site
```

The offline suite (`cargo test`) is fast, secret-free, and cost-free: it
spawns the real binary over stdio and never touches the network (fetch
tests use deterministic local `tiny_http` fixtures on `127.0.0.1`).

## Test layout

| Path                                           | What it covers                                                             |
| ---------------------------------------------- | -------------------------------------------------------------------------- |
| `tests/fs_server.rs`                           | Filesystem server: every tool, path/symlink/access edges, list hints       |
| `tests/fetch_server.rs`                        | Fetch server: robots.txt modes, truncation, user agents, prompt, bounds    |
| `tests/memory_server.rs`                       | Memory server: graph lifecycle, JSONL persistence, resource, subscriptions |
| `tests/shell_server.rs`                        | Shell server: argv/cwd/timeout/truncation/exit codes, access validation    |
| `tests/skills_server.rs`                       | Skills server: discovery, activation, manifests, containment               |
| `tests/agents_server.rs`                       | Agents server: definitions, lifecycle, provider/child-MCP safety           |
| `tests/common/mod.rs`                          | Shared MCP client helpers                                                  |
| `tests/openrouter_e2e.rs`, `tests/openrouter/` | Gated real-network acceptance suite (ignored by default)                   |

## The E2E case catalog

The semantic catalog lives in `tests/openrouter/cases.rs`. It is the single
source of truth for what runs online, and `assert_coverage` enforces exact
set equality between the catalog and the live tool inventory plus
per-parameter coverage. **If you add or rename a tool or parameter, you
must update the catalog** — the acceptance suite fails otherwise. The
human-readable mirror is the [Coverage matrix](/coverage).

## Running the gated acceptance suite

The suite is `#[ignore]`d and requires `OPENROUTER_API_KEY` in the
environment (never on the command line), and `OPENROUTER_MODEL` unset so
the exact required default alias is used:

```bash
OPENROUTER_API_KEY=<key in the environment> \
env -u OPENROUTER_MODEL cargo test --test openrouter_e2e \
  -- --ignored --nocapture --test-threads=1
```

It spends real tokens and takes minutes; bounds, retry policy, and the most
recent verified run are documented on the [OpenRouter E2E](/openrouter-e2e)
page.

For the opt-in live agents test, load uncommitted secrets from `.env.test`
rather than placing values in a command or definition:

```bash
set -a
. ./.env.test
set +a
cargo test --test agents_openrouter_e2e -- --ignored --test-threads=1
```

Never commit or log `.env.test` or its values. The ordinary offline command
above remains the required secret-free coverage for all six servers.

## Docs site tooling

The docs live in `docs/` and are built with VitePress 2
(`vitepress` 2.0.0-alpha.19, exact pin) and formatted with Prettier 3.9.6
(exact pin). The package is private and ESM.

```bash
cd docs
npm install          # installs exact versions from package-lock.json
npm run docs:dev     # local dev server
npm run docs:build   # production build -> docs/.vitepress/dist
npm run docs:preview # serve the built site locally
npm run format       # prettier --write .
npm run format:check # prettier --check .
```

**Docs gates:** `format:check` and `docs:build` are the lint/format gates
for documentation changes (no markdownlint dependency). The build also
fails on dead internal links, so every page and anchor referenced in this
site is verified at build time. See [CI & publishing](/ci-publishing) for
how the site is deployed.

### Docs conventions

- Keep pages source-accurate: the source files in `src/` and `tests/` are
  the ground truth; this site is a mirror, not a spec.
- Tool reference tables state every parameter, default, and bound.
- Security-relevant behavior belongs on [Security model](/security) and is
  linked, not duplicated.
