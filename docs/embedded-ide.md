# Design: Embedded VS Code (code-server) beside the agent

**Status:** accepted — design of record for this change
**Scope:** `loom` (orchestrator + Vue UI), the Docker image
**Sibling:** [browser-terminal.md](browser-terminal.md) — the same shape, one level down

## 1. Goal

Give every session a **real VS Code in the browser**, rooted at that session's
worktree, sitting **beside the live terminal** — pull it in from the right,
side-by-side with the agent. It replaces the bespoke "Files" tab: instead of
loom's hand-rolled tree + Monaco viewer, the operator gets the full editor —
file tree, multi-file edit, search, git gutters, extensions, an integrated
terminal — pointed at the same files the agent is working.

This mirrors what the terminal already did for *interaction*: take a real,
server-side tool and bridge it to the browser on loom's own origin, under loom's
auth, with no second login. The terminal bridges a **PTY**; this bridges a
**code-server HTTP+WebSocket server**.

## 2. Why this fits loom

The terminal established the template ([browser-terminal.md](browser-terminal.md)):
a server-side interactive surface, reachable only through loom, embedded in the
SPA. The editor is the same idea with a heavier engine:

| Concern | Terminal | Editor (this) |
|---|---|---|
| Server-side engine | `tapestry` PTY supervisor | `code-server` process, rooted at the worktree |
| Per session | yes (one supervisor) | yes (one code-server, lazy) |
| Transport through loom | WebSocket bridge (`terminal.rs`) | HTTP + WS **reverse proxy** (`ide.rs`) |
| Origin / auth | loom origin, `require_auth` | loom origin, `require_auth` — **free**, same cookie |
| Browser surface | `AgentTerminal.vue` (xterm) | `IdeFrame.vue` (`<iframe>`) |

Because the editor is served **under loom's own origin** behind the existing
auth middleware, the iframe's requests and WebSockets carry the loom session
cookie automatically. code-server itself runs `--auth none`, bound to loopback,
reachable only through loom's authenticated proxy — no second password, nothing
exposed.

## 3. Architecture

```
                GET/WS /api/sessions/{id}/ide/*   (loom origin, require_auth)
browser <iframe> ───────────── HTTP + WebSocket ─────────────▶  loom reverse proxy
  src=/api/.../ide/?folder=<work_dir>                              │  ide.rs
                                                                   │  strip ".../ide" prefix
                                                                   ▼
                                            code-server  (127.0.0.1:<ephemeral>, --auth none)
                                              spawned lazily, rooted at the session's work_dir
```

### 3.1 Lifecycle (`IdeManager`, in `AppState`)

One `code-server` per session, **lazy** and **idle-reaped** — modeled on how
tapestry is one-supervisor-per-session, but simpler: the code-server is a
**child of `loom`**, not a detached process. Rationale: unlike the terminal
(whose detached supervisor must outlive a loom restart to preserve a running
agent), the editor holds **no irreplaceable state** — the files live on disk. If
loom restarts, its code-servers die with it and respawn on next access; the
iframe reconnects and the workbench reloads. This avoids an orphan-tracking table
and a DB migration entirely; the manager is an in-memory map.

- **`ensure(session_id, work_dir) -> port`** — return the running instance's
  port (bumping its last-access), else spawn one and parse the bound port from
  code-server's `HTTP server listening on http://127.0.0.1:<port>/` log line.
  Concurrent first-hits share one spawn via a per-session `OnceCell`.
- **Reaper** — a background task (spawned in `server::serve`, beside the monitor)
  kills instances idle longer than `ide.idle_timeout_secs`.
- **`kill(session_id)`** — called from `archive`/`remove` teardown, so a
  torn-down session leaves no stray editor.
- **Availability** — probed once (`code-server --version`) and cached; surfaced
  to the UI so a host without code-server degrades to a clear message, not a
  broken iframe. loom never bundles code-server in the binary; it's an external
  tool on `PATH` (installed in the Docker image; optional for a local dev loom).

Per-instance state (user-data-dir, extensions-dir) lives under
`$WEAVER_HOME/ide/<session_id>/` and is removed on kill. A throwaway
`CODE_SERVER_CONFIG` keeps the operator's `~/.config/code-server/config.yaml`
from overriding the launch flags. The launch seeds `settings.json` with
`workbench.startupEditor: "none"` so a fresh instance lands straight in the
Explorer.

### 3.2 The reverse proxy (`ide.rs`)

The one genuinely new piece of infra. A single handler bound to
`ANY /api/sessions/{id}/ide` and `ANY /api/sessions/{id}/ide/{*rest}` in the
**protected** (authenticated) router. Per request:

1. Parse `{id}` and the suffix from the original URI. code-server derives its own
   base path per-request from relative URLs and has **no base-path flag**, so the
   proxy **strips** the `/api/sessions/{id}/ide` prefix and forwards only the
   suffix. A request to the bare `…/ide` (no trailing slash) **308-redirects** to
   `…/ide/` — the trailing slash is load-bearing for code-server's relative-root
   math.
2. `ensure()` the code-server, get its loopback port.
3. **Plain HTTP** (the bulk — assets, API): forward via a pooled
   `hyper_util` legacy client to `http://127.0.0.1:<port>/<suffix>`, stream the
   response body back. Hop-by-hop headers stripped.
4. **WebSocket upgrade** (the extension host, the integrated terminal): a
   transparent passthrough — open a TCP connection to the port, replay the
   upgrade request **preserving `Host` and `Origin`** (code-server runs a
   same-origin check on every WS upgrade; mismatched/absent `Host` → 403), relay
   the `101`, then `copy_bidirectional` between the two upgraded streams.

Because everything is same-origin under loom, the browser sends `Origin` =
`Host` = loom's external host on the WS; forwarding both unchanged satisfies
code-server's check. Webviews require a **secure context** (HTTPS, or
`localhost`) — satisfied by the TLS-terminating proxy in the deploy and by
`http://127.0.0.1` in local dev.

### 3.3 Frontend

- **`IdeFrame.vue`** — fetches `GET /api/sessions/{id}/ide-info`; if code-server
  is available it mounts `<iframe src="/api/sessions/{id}/ide/?folder=<work_dir>">`,
  else it shows a short "code-server not installed" note with the install hint.
  A reload control re-spawns/reconnects.
- **`SessionDetail.vue`** — a **resizable horizontal split**: the live terminal
  on the left, a collapsible editor panel on the right. Closed by default (so a
  session-open does **not** spawn a code-server); a grab handle / "Editor" toggle
  on the right edge pulls it in. The split width persists locally (a presentation
  preference, not feature state). Opening the panel is the lazy trigger.
- The **Files tab is removed** — the editor is the file/editing surface. The
  `/s/:id/files` route and `FileBrowser.vue` go away; the shared `MarkdownView`
  rendering pipeline stays (still used by Artifacts).

## 4. Settings (`weaver_core::config::registry()`, group "Editor")

| Key | Kind | Default | Meaning |
|---|---|---|---|
| `ide.enabled` | Bool | `true` | Master switch; off ⇒ no panel, proxy 503s. |
| `ide.idle_timeout_secs` | Int | `1800` | Reap a code-server idle this long. |
| `ide.command` | String | `""` | Override the `code-server` binary/command (empty ⇒ `code-server` on `PATH`). |

`WEAVER_IDE_CMD` overrides `ide.command` for tests/dev (point it at a stub).

## 5. Security

- **Auth is inherited.** The proxy lives behind `require_auth`; the iframe rides
  the loom cookie. code-server is `--auth none`, **loopback-only**, never exposed
  except through the authenticated proxy.
- **WS origin check.** Preserve `Host`/`Origin` on the upgrade so code-server's
  own CSWSH defense holds (and is not bypassed).
- **Secure context.** Webviews need HTTPS/localhost — documented; the deploy is
  behind TLS.
- **Path containment.** The proxy only ever targets `127.0.0.1:<the session's
  own port>`; `{id}` resolves through `require_session`, so one session can't
  proxy into another's editor by URL-fiddling.

## 6. Deploy

The runtime Docker image gains code-server (the `.deb`, which bundles its own
Node — no system-Node coupling; ~150–250 MB on disk). This is the real cost of
the feature and is deliberate. A local dev loom without code-server still runs;
the editor panel just reports "not installed".

## 7. Testing

- **Unit (`ide.rs`):** the `HTTP server listening on …` port parser; the
  prefix-strip / trailing-slash-redirect logic.
- **Integration (`tests/integration/ide.rs`):** stand up a stub upstream (a tiny
  axum app with a marker HTTP route and a WS echo), register it for a session via
  the manager, and drive the proxy end-to-end: HTTP suffix round-trips with the
  prefix stripped, the bare `…/ide` 308-redirects, and a WS frame echoes through.
  This exercises the novel proxy code without needing code-server in CI.
- **e2e (Playwright):** the editor panel pulls in from the right beside the
  terminal and — since CI has no code-server — shows the graceful "not
  installed" state.
- **Removed/repointed:** `tests/integration/files.rs` (the file-viewer endpoints)
  goes with the UI; `e2e/tests/markdown.spec.ts` repoints onto the Artifacts
  surface, which renders the same `MarkdownView` pipeline. (`e2e/tests/tree.spec.ts`
  is the unrelated *session-tree* test and is untouched.)

## 8. Removed surface

`FileBrowser.vue`, the `/s/:id/files` route + Files tab, the frontend
`writeFile` helper and `FileTree`/`FileContent` types, and the backend
`GET /sessions/{id}/tree`, `GET/PUT /sessions/{id}/file` routes + handlers (their
only consumers are the file browser and the unused `writeFile`). **Kept:**
`GET /sessions/{id}/raw` (markdown inline images), `/diff`, `/log`, `MarkdownView`,
Monaco (Artifacts), Artifacts.

## 9. Open questions / future work

- **Shared vs per-session code-server.** Per-session is the clean isolation model
  and matches tapestry; if process weight ever bites, a single instance with
  `?folder=` per session is the fallback (loses isolation, gains one process).
- **Extension persistence.** Per-session extensions-dir means extensions don't
  carry across sessions. A shared, read-mostly extensions-dir is a later option
  if operators want a standing toolbelt.
- **Built-in port proxy.** code-server's `/proxy/<port>` app-proxy is not wired
  through; if we later want to preview an agent's dev server, that's an additive
  proxy path, not a redesign.
