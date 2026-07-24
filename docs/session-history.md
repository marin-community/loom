# Session history and search

Loom exposes one provider-neutral record contract for agent recall:

```text
GET /api/sessions/{id}/history
GET /api/sessions/{id}/history/search?q=<literal>
```

Both routes accept `before=<opaque cursor>`, `limit=1..200` (default 100), and
`kinds=<comma-separated kinds>`. A response is a newest-tail page whose
`records` are in chronological display order. If `older_cursor` is present,
pass it as the next request's exclusive `before` cursor. Search is
case-insensitive literal matching over the same normalized records; it is not
semantic or repository-wide search.

The record kinds are `message`, `reasoning`, `tool_call`, `tool_result`,
`context`, `event`, and `image`. Common prose is in `content`. Tool records can
also carry `tool_name`, `tool_status`, `is_error`, `locations`, and optionally
`tool_input`.

Optional fields are capability claims, not empty compatibility slots. In
particular, ACP's `ToolCall` update supplies a title, kind, status, result
content, and locations but no invocation-arguments field. ACP history therefore
omits `tool_input`; it never relabels result content as input. Claude and Codex
terminal transcripts can include tool input, so their normalized records retain
it when present.

## Sources and durability

- ACP sessions read the durable SQLite `chat_blocks` journal, preserving its
  stable `(turn, seq)` positions behind opaque cursors.
- Terminal sessions normalize the provider's native JSONL transcript through
  Iris on read. The existing file-fingerprint cache avoids repeated parsing,
  and the archived Iris `chat.json` is the durable fallback after teardown.

Terminal records are intentionally not imported into a second live SQLite
store. The native transcript remains mutable while an agent runs, and Loom
already has a single-flight, fingerprint-invalidated read path plus archive
capture. Importing would require a second cursor, synchronization, partial-write
recovery, and provenance policy while duplicating the same content. Read-time
normalization is the smaller coherent model; a future import should replace it
only if cross-session indexing needs durable materialization.

## Authorization and MCP

REST owns session resolution, pagination, filters, search, and authorization.
The normal session-token grant applies: a caller can read its bound session
tree, but an unrelated sibling session receives `403`.

The built-in `mcp/history/self@v1` capability set exposes `history` and `search`
tools. Their schemas have no session selector. The stdio adapter resolves
`LOOM_SESSION_ID`, uses the session-scoped `LOOM_TOKEN`, and calls the REST
routes above. Profiles can select the `history` MCP group (or `all`); the
session's pinned capability snapshot still determines the exact tools launched.
