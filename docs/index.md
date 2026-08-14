---
layout: home

hero:
  name: modelcontextprotocol
  text: Four MCP servers in one Rust binary
  tagline: Filesystem, Fetch, Memory, and Shell servers implementing the Model Context Protocol 2026-07-28 specification over stdio.
  actions:
    - theme: brand
      text: Get started
      link: /getting-started
    - theme: alt
      text: View on GitHub
      link: https://github.com/maxylev/modelcontextprotocol

features:
  - title: Filesystem server
    details: 'Secure read/write access to a set of allowed directories: read, write, edit, search, tree listings, file info, and more, with path and symlink protection.'
    link: /servers/filesystem
  - title: Fetch server
    details: 'Fetches URLs and converts HTML pages to markdown for the model, with robots.txt enforcement, truncation pagination, and a user-initiated fetch prompt.'
    link: /servers/fetch
  - title: Memory server
    details: 'Persistent knowledge-graph memory (entities, relations, observations) stored as JSONL, exposed through tools, a resource, and live subscriptions.'
    link: /servers/memory
  - title: Shell server
    details: 'Executes local programs directly with an explicit argv, a restricted working directory, bounded output capture, and a per-command timeout.'
    link: /servers/shell
  - title: Modern protocol
    details: 'Stateless MCP 2026-07-28 via rmcp: server/discover without an initialize handshake, cache hints on tools/list, and tool annotations.'
    link: /protocol
  - title: Verified behavior
    details: 'Offline integration suites cover every parameter and access-control edge; a gated real-network OpenRouter acceptance suite exercises the whole stack.'
    link: /verification
---
