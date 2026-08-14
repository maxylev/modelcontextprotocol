# Memory server

Persistent knowledge-graph memory: entities, relations, and observations,
stored as a JSONL file, mirroring the reference
`@modelcontextprotocol/server-memory` server.

- **Identity:** `mcp-memory` (crate version)
- **Capabilities:** tools, resources, resource subscriptions
- **Invocation:** `modelcontextprotocol memory [--memory-file <PATH>]` or
  `modelcontextprotocol --memory [--memory-file <PATH>]`

## Storage

- **Format:** one JSON object per line — `{"type":"entity",...}` and
  `{"type":"relation",...}` lines in the reference server's format, so a
  file written by this server is readable by the reference server and vice
  versa.
- **File location precedence:** `--memory-file` flag, then the
  `MEMORY_FILE_PATH` environment variable, then `memory.jsonl` in the
  current working directory. Relative paths resolve against the working
  directory at startup.
- **Persistence model:** every mutation loads the file, applies the change,
  and rewrites it (a missing file starts as an empty graph). Mutations are
  serialized by a mutex. A corrupt line fails the operation with an
  explicit error.
- Written with the process's OS permissions; no encryption (see
  [Security model](/security)).

## Tools

9 tools. The graph shape is
`{ entities: [{ name, entityType, observations[] }], relations: [{ from,
to, relationType }] }` (camelCase on the wire).

### create_entities

Create multiple entities; names that already exist are ignored. Returns
the entities actually added (`{ "entities": [...] }`).

| Parameter  | Required | Default | Bounds / notes                                          |
| ---------- | -------- | ------- | ------------------------------------------------------- |
| `entities` | yes      | —       | array of `{ name, entityType, observations: string[] }` |

### create_relations

Create multiple directed relations; exact duplicates (`from`, `to`,
`relationType` all equal) are skipped. Relations should be in active
voice. Returns `{ "relations": [...] }`.

| Parameter   | Required | Default | Bounds / notes                        |
| ----------- | -------- | ------- | ------------------------------------- |
| `relations` | yes      | —       | array of `{ from, to, relationType }` |

### add_observations

Add observations to existing entities; fails ("Entity with name ... not
found") if any entity does not exist. Duplicates _within a single call_
are all added; only observations already present are filtered. Returns
`{ "results": [{ entityName, addedObservations[] }] }`.

| Parameter      | Required | Default | Bounds / notes                                |
| -------------- | -------- | ------- | --------------------------------------------- |
| `observations` | yes      | —       | array of `{ entityName, contents: string[] }` |

### delete_entities

Delete entities and cascade-delete their relations. Unknown names are
silently ignored. Returns `{ success: true, message: "Entities deleted
successfully" }`.

| Parameter     | Required | Default | Bounds / notes   |
| ------------- | -------- | ------- | ---------------- |
| `entityNames` | yes      | —       | array of strings |

### delete_observations

Delete specific observations from entities; missing entities or
observations are silently ignored. Returns a success object.

| Parameter   | Required | Default | Bounds / notes                                    |
| ----------- | -------- | ------- | ------------------------------------------------- |
| `deletions` | yes      | —       | array of `{ entityName, observations: string[] }` |

### delete_relations

Delete specific relations (matched by `from`, `to`, `relationType`);
missing relations are silently ignored. Returns a success object.

| Parameter   | Required | Default | Bounds / notes                        |
| ----------- | -------- | ------- | ------------------------------------- |
| `relations` | yes      | —       | array of `{ from, to, relationType }` |

### read_graph

Read the entire knowledge graph (zero parameters).

### search_nodes

Case-insensitive substring search across entity names, entity types, and
observation contents. Relations with at least one matching endpoint are
included, so connections to nodes outside the result set are discoverable.
A query with no matches returns an empty graph.

| Parameter | Required | Default | Bounds / notes |
| --------- | -------- | ------- | -------------- |
| `query`   | yes      | —       | string         |

### open_nodes

Open specific entities by name; unknown names are skipped. Relations with
at least one endpoint in the requested set are included.

| Parameter | Required | Default | Bounds / notes   |
| --------- | -------- | ------- | ---------------- |
| `names`   | yes      | —       | array of strings |

## Resource: knowledge-graph

- URI: `memory://knowledge-graph`, MIME `application/json`.
- Contents: the full graph, identical in shape to `read_graph`, as
  pretty-printed JSON.
- Listed under the name `knowledge-graph` in `resources/list`.

## Subscriptions

- **Modern (2026-07-28):** the server accepts `resource_subscriptions` for
  `memory://knowledge-graph` and holds `subscriptions/listen` open,
  sending `notifications/resources/updated` for the URI after every graph
  mutation.
- **Legacy:** the same notifications reach legacy `resources/subscribe`
  listeners (both flows share the mutation broadcast channel).

## Mutation → notification mapping

Every mutation tool (`create_entities`, `create_relations`,
`add_observations`, `delete_entities`, `delete_observations`,
`delete_relations`) broadcasts a graph-updated notification after a
successful write; read-only tools never notify.

## Annotations summary

| Tool                                                           | RO  | ID  | DEST |
| -------------------------------------------------------------- | --- | --- | ---- |
| `create_entities` / `create_relations` / `add_observations`    | no  | no  | no   |
| `delete_entities` / `delete_observations` / `delete_relations` | no  | yes | yes  |
| `read_graph` / `search_nodes` / `open_nodes`                   | yes | yes | no   |

All memory tools have `openWorldHint: false`.
