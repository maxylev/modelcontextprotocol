# Architecture

One Rust binary, six independent MCP servers. The workspace-free package
(`Cargo.toml`, edition 2024, MSRV 1.97) has one binary target,
`modelcontextprotocol`.

## Source layout

```text
src/
├── main.rs            # tracing, CLI parse, six-way dispatch
├── cli.rs             # clap definitions and strict selector normalization
├── fs/                # filesystem server (14 tools)
├── fetch/             # fetch server (tool and prompt)
├── memory/            # memory server (9 tools, resource, subscriptions)
├── shell/             # shell server (execute_command)
├── skills/            # skills registry, SKILL.md parser, resource manifest
├── agents/            # definitions, discovery, provider/runtime, child MCP
└── support/           # protocol constants, results, shared access control
```

`main` and `cli` dispatch to concrete servers. Filesystem and shell share
`support::access`. Skills owns the workspace `SkillRegistry`; agents creates
the same registry for its workspace and uses it to preload an agent's named
skills. Concrete server modules otherwise do not depend on one another.

## Design decisions

- **One binary, six identities.** `mcp-filesystem`, `mcp-fetch`,
  `mcp-memory`, `mcp-shell`, `mcp-skills`, and `mcp-agents` all advertise the
  crate version. A client config launches the same executable with one
  selector.
- **Strict CLI narrowing.** `Cli::into_command` accepts exactly one of the
  six subcommands or equivalent flags, rejects cross-server options, and has
  no config-file option. Skills and agents each accept one workspace.
- **Protocol narrowing.** Every server supports only MCP `2026-07-28` via
  the shared `SUPPORTED_PROTOCOL_VERSIONS`. Child MCP clients used by agents
  require discovery at that same protocol version.
- **Snapshot registries.** Skills and agents discover canonical workspace
  paths once at startup, retain deterministic precedence/name collision
  winners, and do not watch for changes. Skills activation reparses the
  selected file before returning it.
- **Async agents.** Agent sessions are process-local and retain a frozen
  definition, frozen context, provider-native conversation, and latest public
  result. Runs are capacity-limited to 8 and execute in Tokio tasks. Child
  stdio/HTTP MCP connections and permits belong to a run, are recreated on
  resume, and are shut down with bounded timeouts. Up to 64 terminal sessions
  are retained by a lazy count-bounded LRU policy.
- **Dependencies and size.** Agents/skills add YAML and TOML parsing,
  UUID v7 identifiers, cancellation utilities, URL validation, and rmcp
  client/child-process/streamable-HTTP features. Release settings remain
  `opt-level = "z"`, fat LTO, one codegen unit, and stripping: the release-size
  goal remains a minimal binary; the measured macOS build is about 6.5 MB and
  varies by target.

The agents lifecycle is deliberately split:

```text
AgentDefinition
      ↓
AgentSession
      ├── frozen definition and system context
      ├── provider-native conversation history
      ├── state, latest result/error, and activity
      └── no idle child MCP connections
            │
            ├── AgentRun #1
            │     ├── capacity permit
            │     ├── child MCP manager
            │     ├── provider/tool loop
            │     └── bounded cleanup
            │
            └── AgentRun #2
                  ├── fresh permit
                  ├── fresh child MCP manager
                  ├── same conversation
                  └── bounded cleanup
```

`AgentSession` is not an MCP transport session, and `AgentRun` is not an
`AgentSession`. The former is the retained logical subagent; the latter is one
bounded execution using disposable external connections. No automatic context
compaction or persistent conversation store exists.

## Testing architecture

Offline integration suites spawn the real binary over stdio for the original
servers and the skills/agents registries. `tests/skills_server.rs` covers
discovery, activation, precedence, validation, manifests, and containment.
`tests/agents_server.rs` covers definition discovery, tool schemas, lifecycle,
limits, credential isolation, redirect handling, and the Responses adapter
with local fixtures. Unit suites cover child MCP safety, parsers, and shared
skill loading. A live-provider smoke session is optional rather than an E2E
suite. The older OpenRouter chat acceptance harness covers its existing
catalog; it is not a claim of coverage for the new agents tools.

## Docs site

VitePress 2 builds `docs/`; the custom theme is in
`docs/.vitepress/theme/`. The publishing workflow copies `cargo doc` output
under `/rustdoc/`.
