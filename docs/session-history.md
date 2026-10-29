# Session history and search

Loom exposes one provider-neutral record contract for agent recall:

```text
POST /api/sessions/history/list    { session, ... }
POST /api/sessions/history/search  { session, q, ... }
```

Both routes accept `offset` (default 0), `limit=1..200` (default 100),
`kinds` (an array of record kinds), and `role` (`user`, `assistant`, or `context`).
Offsets count matching records backward from the newest, after filters and
search. Each page's `records` are returned in chronological display order.
Pass `next_offset`, when present, as `offset` with the same filters to continue.
For example, `{ "session": "...", "kinds": ["message"], "role": "user",
"limit": 20, "offset": 0 }` returns the latest 20 human messages without tools.
Live history can grow between requests, shifting offsets. Record cursors remain
stable references for citations, not pagination arguments.

Search is case-insensitive literal matching over the same normalized records;
it is not semantic or repository-wide search.

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

The built-in `loom/sessions/read@v1` capability set exposes `session_history`
and `session_search` tools. Their schemas have no session selector. The aggregate stdio server resolves
`LOOM_SESSION_ID`, uses the session-scoped `LOOM_TOKEN`, and calls the REST
routes above. Profiles can select the `session` MCP group (or `all`); missing
capabilities in an older profile revision are ignored at launch.

## Handoff reconstruction

Handoff works even when the source agent is unavailable. After pausing the
source, Loom gives the incoming runtime's temporary summarizer the current goal,
prior handoff digest, bounded recent records, and the existing `session_history`
/ `session_search` MCP tools. These read the durable session journal through
the ordinary REST API. There is no separate history server, copied transcript,
or compactor-specific pagination.

A short-lived scoped engine credential grants session reads without runtime
write authority. It remains usable when recovering a failed source session,
cannot be widened by the session's unrestricted runtime policy, and is revoked
when summarization ends (or expires after 180 seconds if interrupted). The
summarizer process receives the history MCP server with that credential;
unrelated MCP servers and runtime write credentials are removed.

The summarizer is instructed to page through human messages to recover old
constraints, investigate decisions, inspect tool evidence selectively, and cite
record cursors. Normal history page limits apply. Within its 180-second deadline,
it must return a summary and report gaps rather than claim exhaustive coverage.

The replacement receives the digest and the bounded recent conversation; full
history remains available through its normal session tools. Failure to generate
a summary retains the recent-conversation and history-lookup fallback.
