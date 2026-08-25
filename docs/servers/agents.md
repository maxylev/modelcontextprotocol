# Agents server

`modelcontextprotocol agents [DIR]` starts `mcp-agents` for one workspace.
When `DIR` is omitted, the workspace is the process's current directory.
It discovers local agent definitions, calls an external model provider, and
can expose configured child MCP tools to that provider. It is intended only
for trusted workspaces.

## Discovery, formats, and precedence

The workspace is canonicalized and must be a directory. Definitions are read
recursively (maximum depth 8), with canonical paths constrained to their root
and workspace. Roots and precedence are:

1. `.agents/agents` — canonical Markdown (`.md`) and TOML (`.toml`)
2. `.claude/agents` — Claude-flavored Markdown
3. `.codex/agents` — TOML
4. `.opencode/agents` — OpenCode-flavored Markdown

Candidates are ordered by root precedence then lexical canonical path; a
canonical file is used once and the first colliding agent name wins. Invalid
or unreadable definitions are ignored with a stderr warning. OpenCode agents
with `mode: primary` are not subagents. The catalog is a startup snapshot.

Canonical Markdown uses YAML frontmatter followed by nonempty instructions.
Canonical TOML uses `instructions` (or `developer_instructions`). Definitions
are limited to 1 MiB. Names are 1–64 characters: lowercase letters and digits,
with single `_` or `-` separators only. Every definition needs a description
and model; canonical Markdown also requires `model_provider`.

## Providers and credentials

The provider fields are `model`, `model_provider`, `base_url`, `env_key`, and
`wire_api` (camelCase aliases are accepted where that format supports them).
`openai` defaults to `https://api.openai.com/v1`, `OPENAI_API_KEY`, and
`responses`; `anthropic` defaults to `https://api.anthropic.com`,
`ANTHROPIC_API_KEY`, and `anthropic-messages`. A `custom` provider must set all
three of `base_url`, `env_key`, and `wire_api`. Endpoints require HTTPS, except
HTTP loopback endpoints.

The runtime implements OpenAI **Responses** and Anthropic **Messages** wire
APIs. Credentials are looked up at spawn time and again for a resumed run using
`env_key`, so agents may use different environment variables and refreshed
tokens. Literal API-key/token fields are rejected; keep secrets in the process
environment.

This canonical OpenRouter-compatible custom definition uses the Responses API:

```md
---
name: research
description: Research a focused question.
model: openai/gpt-5.6-luna
model_provider: custom
base_url: https://openrouter.ai/api/v1
env_key: OPENROUTER_API_KEY
wire_api: responses
---

Return sourced, concise findings.
```

## Tools and lifecycle

When the startup catalog is nonempty, the server exposes exactly three tools:

| Tool          | Input                                                                   | Behavior                                                                                                                                |
| ------------- | ----------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `spawn_agent` | `name` (available-agent enum), `task` (nonempty)                        | Starts a background session and promptly returns `{agent_id, name, status: "running"}`.                                                 |
| `send_input`  | `target`, `message` (both nonempty), `interrupt` (default `false`)      | Queues follow-up input for a session. With `interrupt: true`, cooperatively cancels its active run before continuing with queued input. |
| `wait_agent`  | `targets` (unique nonempty IDs), `timeout_ms` (0–300000, default 30000) | Returns each session's running/completed/failed state, result or error, and `timed_out`.                                                |

Save the `agent_id` from `spawn_agent`. `wait_agent` waits until all named
sessions are terminal or its maximum wait duration expires; it is not a sleep.
Finished sessions return immediately. A zero timeout is a snapshot and reports
`timed_out: true` if any target remains running. Calling it pauses the parent
model until it returns.

`spawn_agent` does not wait for child MCP startup, tool discovery, or a provider
request. Those operations belong to the background run. A later startup failure
therefore remains observable through the returned ID as
`child_mcp_startup_error`.

## Session lifetime

Agent sessions exist only in memory and only for the lifetime of the
`modelcontextprotocol agents` process. A completed retained agent may be
continued with `send_input` using the same `agent_id`; its frozen definition,
system context, and real provider-native conversation history remain available
to that subagent. Restarting the agents MCP process invalidates every old agent
ID. There is no disk or provider-side session persistence.

Subagents are designed for focused delegated tasks. They are not intended to
replace the primary long-lived conversation. The runtime preserves raw valid
Responses or Messages history while a session is retained and intentionally
does not summarize, prune, compact, or estimate that history. If it no longer
fits the selected provider/model, the run fails with `context_limit`; the
parent should normally spawn a fresh agent for a narrower task.

Terminal results are non-consuming and remain available while retained. The
runtime retains at most 64 terminal sessions. Exceeding that count evicts the
least-recently-used terminal session, with an `agent_id` tie-break; running
sessions are never evicted. `wait_agent`, `send_input`, and terminal completion
refresh recency. An evicted ID returns `unknown_agent`. This is best-effort
in-memory continuation, not durable persistence.

## Subagent activity and timing

Running `wait_agent` snapshots include safe, structured activity with `phase`,
`summary`, `tool`, `target`, and calculated `total_elapsed_ms` and
`activity_elapsed_ms`. `total_elapsed_ms` measures from session spawn and stays
stable after a terminal state; `activity_elapsed_ms` (the UI's `current` time)
measures from the most recent meaningful activity transition. When an active
operation has a known timeout, its activity may also include
`operation_timeout_remaining_ms`.

Transitions are coarse provider, child-MCP, and tool lifecycle events only.
They never include hidden reasoning, chain-of-thought, prompts, or raw tool
arguments. An active `wait_agent` may send standard request-scoped MCP progress
notifications for meaningful transitions, never timer ticks; tracing remains
on stderr and clients advance displayed timers locally. A wait timeout returns
control without cancelling the child or starting another wait. It is separate
from operation timeouts. A long current duration is a signal, not proof of a
hang. This adds no tools and does not use MCP Tasks.

## Limits and execution model

At most 8 agent runs execute concurrently; a ninth request fails with
`capacity_exceeded`. Retained terminal sessions consume no run permit, but a
terminal resume requires a new permit. A running session accepts up to 16 queued inputs. Agent
definitions default to 32 turns and allow 1–1,000. Provider requests have a
10-second connection timeout and 120-second request timeout. Child MCP startup
is bounded to 15 seconds, calls to 60 seconds, and shutdown to 5 seconds.

Agents cannot recursively spawn agents: they receive only their configured
child MCP tools, not this server's tools. Sessions, conversations, outputs,
and queues are process-local and are not persisted across server restarts.
An agent may preload named skills from the shared Skills registry; missing
configured skills make spawning fail rather than silently omitting context.

## Child MCP safety

Child servers may be stdio (`command`, `args`, `env`) or streamable HTTP
(`url`, `headers`). They must discover protocol `2026-07-28`. Stdio children
start with a cleared environment plus basic platform path/home variables and
only explicitly configured environment entries. HTTP children do not follow
redirects. Child tool descriptions are sanitized and capped at 4 KiB; rendered
tool output is capped at 1 MiB.

Child MCP configuration is frozen in the session definition, but live child
connections are owned by an active run. Each run connects and discovers its
configured children, then performs bounded shutdown on completion, failure, or
interruption. A retained terminal session has no live child process or HTTP
connection; resuming it creates fresh connections while preserving the same
conversation.

Only `${ENV_NAME}` interpolation is supported, and only in configured child
stdio `env` values and HTTP header values. Missing or malformed placeholders
fail startup; commands, arguments, and URLs are never interpolated.

## Security

An agent definition can authorize external provider requests and child MCP
commands or HTTP calls. Accepted configuration must correspond to real runtime
behavior. `isolation: none` is supported; `worktree` and `container` are
rejected. Sandbox `default` and `read-only` are supported;
`workspace-write` and `danger-full-access` are rejected. In read-only mode, a
child tool is exposed only when its annotations explicitly set
`readOnlyHint: true` and do not set `destructiveHint: true`; deny rules still
win.

Directory restriction and MCP tool filtering are access-control mechanisms,
not an operating-system sandbox. Treat agent definitions, their child servers,
and their configured skills as trusted code and instructions. Supply provider
keys through `env_key` environment variables; do not commit or log tokens.
