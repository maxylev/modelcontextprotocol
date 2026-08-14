# Fetch server

Fetches URLs and converts HTML pages to markdown for the model, mirroring
the reference `mcp-server-fetch` server.

- **Identity:** `mcp-fetch` (crate version)
- **Capabilities:** tools, prompts
- **Invocation:** `modelcontextprotocol fetch` or
  `modelcontextprotocol --fetch`
- **Instructions published to clients:** the fetch tool obeys robots.txt
  unless the server was started with `--ignore-robots-txt`; the fetch
  prompt fetches without checking robots.txt

> **Security:** the fetch server can reach local and internal IP addresses;
> there is no SSRF protection. See [Security model](/security).

## Server options

| Option                      | Description                                                 |
| --------------------------- | ----------------------------------------------------------- |
| `--ignore-robots-txt`       | Skip robots.txt checks for the `fetch` tool                 |
| `--user-agent <USER_AGENT>` | Custom User-Agent for all requests, replacing both defaults |
| `--proxy-url <URL>`         | Route all requests through this HTTP(S) proxy               |

Default user agents (unless overridden):

```
ModelContextProtocol/1.0 (Autonomous; +https://github.com/modelcontextprotocol/servers)
ModelContextProtocol/1.0 (User-Specified; +https://github.com/modelcontextprotocol/servers)
```

The autonomous agent is used by the `fetch` tool; the user-specified agent
by the `fetch` prompt.

## Tool: fetch

Fetches a URL and optionally extracts its contents as markdown. The tool
description tells the model that it grants internet access.

| Parameter     | Required | Default | Bounds / notes                                                       |
| ------------- | -------- | ------- | -------------------------------------------------------------------- |
| `url`         | yes      | —       | `http` or `https` only; any other scheme is an error                 |
| `max_length`  | no       | `5000`  | integer, 1..999999; out of range is an error                         |
| `start_index` | no       | `0`     | integer, ≥ 0; out of range is an error                               |
| `raw`         | no       | `false` | `true` returns the page content without HTML→markdown simplification |

### Web search with DuckDuckGo Lite

Use DuckDuckGo's lightweight HTML endpoint as the `url` when you need search
results:

```text
https://lite.duckduckgo.com/lite/?q={query}&kl={kl}&kp={kp}
```

- `q` is the URL-encoded search query.
- `kl` selects the region and language, such as `us-en`.
- `kp` controls Safe Search: `1` = on, `-1` = moderate, `-2` = off.

For example, this searches for `mcp` in US English with Safe Search off:

```text
https://lite.duckduckgo.com/lite/?q=mcp&kl=us-en&kp=-2
```

Behavior:

- **HTML → markdown** by default: pages that look like HTML (first 100
  characters contain `<html`, or `text/html` content type, or empty
  content type) are simplified unless `raw: true`. `style`, `script`,
  `noscript`, and `template` content is suppressed, never leaked. If
  nothing extractable remains, the result is
  `<error>Page failed to be simplified from HTML</error>`.
- **Non-HTML content** (JSON, plain text, ...) is returned raw with a
  status prefix ("Content type ... cannot be simplified to markdown, but
  here is the raw content:").
- **Truncation** is character-based: `start_index` resumes a previous
  fetch; when content is cut off, a continuation hint is appended —
  `<error>Content truncated. Call the fetch tool with a start_index of N to
get more content.</error>` — and `start_index` beyond the end returns
  `<error>No more content available.</error>`.
- **HTTP status ≥ 400** surfaces as a tool error ("Failed to fetch ... -
  status code 404").
- **robots.txt** is consulted before fetching (unless
  `--ignore-robots-txt`): unreachable → blocked; 401/403 → blocked; other
  4xx → allowed; a disallow rule matching the autonomous user agent →
  blocked with guidance to try the fetch prompt.
- Every request has a **30-second timeout**.

## Prompt: fetch

A user-initiated fetch: takes `url`, fetches the page (no robots.txt
check), and returns the content as a user prompt message with a
description ("Contents of {url}"). A blank URL is rejected
(`invalid_params`); fetch failures become a prompt message describing the
failure rather than a protocol error.

## Request internals

- HTTP client: reqwest with the `ring` rustls backend (installed
  explicitly at startup), charset decoding enabled.
- No redirect restrictions, no size cap on the response body itself (the
  tool truncates the _returned text_, not the download).
