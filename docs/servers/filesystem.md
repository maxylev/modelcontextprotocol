# Filesystem server

Secure read/write access to a set of allowed directories, mirroring the
reference `@modelcontextprotocol/server-filesystem` server.

- **Identity:** `mcp-filesystem` (crate version)
- **Capabilities:** tools
- **Invocation:** `modelcontextprotocol filesystem [DIR ...]` or
  `modelcontextprotocol --filesystem [DIR ...]` — defaults to the process's
  current directory when no directory is supplied
- **Instructions published to clients:** use `list_allowed_directories`
  first to see which directories are accessible

All operations are restricted to the allowed directories; paths outside —
including `..` escapes and symlinks pointing outside — are rejected.
Relative paths resolve against the first allowed directory. See
[Security model](/security) for the full path/symlink controls.

## Tools

14 tools. Annotation legend: **RO** = `readOnlyHint`, **ID** =
`idempotentHint`, **DEST** = `destructiveHint`, **OW** = `openWorldHint`.
All filesystem tools have `openWorldHint: false`.

### read_text_file

Read the complete contents of a file as text; optionally only the first or
last N lines. Handles various text encodings; operates on the file as text
regardless of extension.

| Parameter | Required | Default | Bounds / notes                        |
| --------- | -------- | ------- | ------------------------------------- |
| `path`    | yes      | —       | must be within an allowed directory   |
| `head`    | no       | none    | `u32`; returns only the first N lines |
| `tail`    | no       | none    | `u32`; returns only the last N lines  |

`head` and `tail` are mutually exclusive: passing both returns an error
("Cannot specify both head and tail parameters simultaneously").

### read_file (deprecated alias)

Identical schema and behavior to `read_text_file`. Kept for compatibility
with the reference server; its description instructs clients to use
`read_text_file` instead.

### read_media_file

Read a file and return it as a base64-encoded content block with its MIME
type (detected from the extension via `mime_guess`, defaulting to
`application/octet-stream`).

| Parameter | Required | Default | Bounds / notes                      |
| --------- | -------- | ------- | ----------------------------------- |
| `path`    | yes      | —       | must be within an allowed directory |

- `image/*` files → an image content block.
- `audio/*` files → an audio content block.
- Anything else → an embedded blob resource (`file://` URI).

### read_multiple_files

Read several files at once; each file's content is returned with its path
as a reference. Failed reads for individual files do not stop the
operation.

| Parameter | Required | Default | Bounds / notes                                                                    |
| --------- | -------- | ------- | --------------------------------------------------------------------------------- |
| `paths`   | yes      | —       | array of strings, must be non-empty; each path must be within allowed directories |

An empty list is rejected: "At least one file path must be provided".

### write_file

Create a new file or completely overwrite an existing one. Handles text
content with proper encoding.

| Parameter | Required | Default | Bounds / notes                      |
| --------- | -------- | ------- | ----------------------------------- |
| `path`    | yes      | —       | must be within an allowed directory |
| `content` | yes      | —       | string                              |

Overwrites without warning. The write is atomic (temp file + rename) so a
symlink appearing between validation and the write is never followed.

### edit_file

Line-based edits: each edit replaces an exact text sequence (or a
whitespace-tolerant line match) with new content, and the tool returns a
git-style diff of the changes.

| Parameter | Required | Default | Bounds / notes                                                    |
| --------- | -------- | ------- | ----------------------------------------------------------------- |
| `path`    | yes      | —       | must be within an allowed directory                               |
| `edits`   | yes      | —       | array of `{ oldText: string, newText: string }`, applied in order |
| `dryRun`  | no       | `false` | when `true`, returns the diff preview without applying            |

Matching mirrors the reference server: exact substring match first, then
line-by-line matching that ignores leading/trailing whitespace per line and
preserves the indentation of the first matched line. CRLF is normalized to
LF. An edit that cannot be matched fails the whole call ("Could not find
exact match for edit: ...").

### create_directory

Create a new directory (and any missing parents) or ensure it exists.

| Parameter | Required | Default | Bounds / notes                      |
| --------- | -------- | ------- | ----------------------------------- |
| `path`    | yes      | —       | must be within an allowed directory |

Succeeds silently if the directory already exists.

### list_directory

Detailed listing of a directory, sorted by name, with `[DIR]` and `[FILE]`
prefixes.

| Parameter | Required | Default | Bounds / notes                      |
| --------- | -------- | ------- | ----------------------------------- |
| `path`    | yes      | —       | must be within an allowed directory |

### list_directory_with_sizes

Like `list_directory`, plus human-readable sizes (e.g. `1.50 KB`) and a
totals footer ("Total: N files, M directories" / "Combined size: ...").

| Parameter | Required | Default  | Bounds / notes                                                                  |
| --------- | -------- | -------- | ------------------------------------------------------------------------------- |
| `path`    | yes      | —        | must be within an allowed directory                                             |
| `sortBy`  | no       | `"name"` | exactly `"name"` or `"size"` (size sorts descending); anything else is an error |

### directory_tree

Recursive tree view as a JSON structure: each entry has `name`, `type`
(`file` or `directory`), and `children` for directories (possibly empty).
Output is 2-space-indented JSON.

| Parameter         | Required | Default | Bounds / notes                                                                                                                            |
| ----------------- | -------- | ------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `path`            | yes      | —       | must be within an allowed directory                                                                                                       |
| `excludePatterns` | no       | `[]`    | glob patterns matched against paths relative to the root; patterns without `*` also match any path that contains or ends with the pattern |

### move_file

Move or rename files and directories in one operation.

| Parameter     | Required | Default | Bounds / notes                      |
| ------------- | -------- | ------- | ----------------------------------- |
| `source`      | yes      | —       | must be within an allowed directory |
| `destination` | yes      | —       | must be within an allowed directory |

**Destination-exists behavior is platform-dependent:** it is implemented
with `std::fs::rename`. On Unix-like systems the rename may replace an
existing destination; on Windows the operation can fail. Do not rely on
either outcome; the tool description says the operation "will fail" when
the destination exists, but that is not guaranteed across platforms and is
not asserted by the test suite.

### search_files

Recursively search for files and directories matching a glob pattern.
Patterns match paths relative to the search root; `*` does not cross `/`,
`**` does, and dotfiles are matched (minimatch-compatible semantics).
Returns full paths; "No matches found" when nothing matches.

| Parameter         | Required | Default | Bounds / notes                                               |
| ----------------- | -------- | ------- | ------------------------------------------------------------ |
| `path`            | yes      | —       | starting directory, within allowed directories               |
| `pattern`         | yes      | —       | glob-style, e.g. `*.rs` or `**/*.rs`                         |
| `excludePatterns` | no       | `[]`    | glob patterns; matching entries are removed from the results |

Entries that resolve outside the allowed roots (e.g. escaping symlinks)
are skipped.

### get_file_info

Detailed metadata about a file or directory: `size`, `created`,
`modified`, `accessed` (RFC 3339 UTC), `isFile`, `isDirectory`, and
`permissions` (Unix octal mode like `644`; `"unknown"` on non-Unix).

| Parameter | Required | Default | Bounds / notes                      |
| --------- | -------- | ------- | ----------------------------------- |
| `path`    | yes      | —       | must be within an allowed directory |

### list_allowed_directories

Zero-parameter tool that returns the allowed roots (as configured and
canonicalized) — use it before any other call to know what is accessible.

## Annotations summary

| Tool                                                              | RO  | ID  | DEST | OW    |
| ----------------------------------------------------------------- | --- | --- | ---- | ----- |
| `read_file` / `read_text_file`                                    | yes | no  | no   | false |
| `read_media_file`                                                 | yes | no  | no   | false |
| `read_multiple_files`                                             | yes | no  | no   | false |
| `write_file`                                                      | no  | yes | yes  | false |
| `edit_file`                                                       | no  | no  | yes  | false |
| `create_directory`                                                | no  | yes | no   | false |
| `list_directory` / `list_directory_with_sizes` / `directory_tree` | yes | no  | no   | false |
| `move_file`                                                       | no  | no  | yes  | false |
| `search_files` / `get_file_info` / `list_allowed_directories`     | yes | no  | no   | false |
