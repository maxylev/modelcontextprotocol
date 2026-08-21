# Skills server

`modelcontextprotocol skills <DIR>` starts `mcp-skills` for one workspace.
It discovers reusable repository instructions and exposes one tool when at
least one valid skill is available.

## Discovery and precedence

The workspace is canonicalized and must be a directory. The server snapshots
these roots at startup, in descending precedence:

1. `.agents/skills`
2. `.claude/skills`
3. `.opencode/skills`

Each direct child directory may contain `SKILL.md`. Canonical paths must stay
inside the workspace. Candidates are ordered by root precedence and then
lexical canonical path. A canonical directory is considered once, and the
first definition of a colliding skill name wins. Malformed candidates are
ignored with a stderr warning; they do not prevent other skills from loading.
The registry is not refreshed while the server runs.

## Skill format

`SKILL.md` is at most 1 MiB and must begin with YAML frontmatter:

```md
---
name: release-notes
description: Prepare a concise release summary.
---

Use the repository history and the changed files.
```

`name` is required and must be at most 64 characters matching
`^[a-z0-9]+(-[a-z0-9]+)*$`. `description` is required and nonempty. Extra
frontmatter metadata is accepted. The body after frontmatter is the skill's
instructions.

## Progressive disclosure

The discovery catalog contains names and descriptions, not instruction bodies.
Call `activate_skill` to load one skill. Supporting files are listed, rather
than read automatically, so the caller can read only files required by the
instructions.

| Tool             | Input                                                            | Output                                                                                              |
| ---------------- | ---------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `activate_skill` | `{ "name": "<available skill name>" }`; no additional properties | Structured `{name, description, skill_dir, instructions, resources}` plus a short text confirmation |

The name input is an enum of the startup catalog. On activation, `SKILL.md` is
canonicalized and reparsed; a moved file or changed name is rejected. `resources`
is a sorted relative-path manifest excluding `SKILL.md`. It contains at most
1,000 files, follows directories only to depth 8, deduplicates canonical file
targets, and rejects a resource target outside the skill directory.

Activation reads instructions and builds this manifest. It does **not** execute
instructions, scripts, commands, or resources.

## Security

Skill content is repository-provided instruction data, not trusted program
code. Review it before enabling it for a client. Workspace containment and
symlink checks constrain discovery and the resource manifest, but they are
access controls, not an OS sandbox. Only use a workspace whose skills you
trust.
