# Design: Native PTY + xterm.js terminal in the browser

**Status:** proposed
**Scope:** `loom` (orchestrator + Vue UI)
**Author:** design notes for implementation

## 1. Goal

Replace loom's current "render a tmux screenshot + a one-line submit box" UI with
a **real terminal in the browser**: a PTY owned by the server running
`tmux attach`, bridged to [xterm.js] over a WebSocket. The browser gets a true
terminal — colour, cursor, mouse, scrollback, and full-screen TUIs (the Claude
REPL, `lazygit`, `vim`) — instead of a polled, monochrome, read-only mirror with
a degraded line-input box bolted on.

This is **the single interaction surface**. Today there are two ways to "talk to"
an agent and they are both worse than a terminal:

- CLI: `loom attach` → a real `tmux attach` in your own terminal (good, but only
  from a shell on the host).
- Browser: type a line into the submit box → `POST /send` → `tmux send-keys`
  (degraded: line-at-a-time, no keys, no TUI, no feedback except the next
  screenshot).

The structured/status data plane (working / waiting / idle → lifecycle + the
branch's `attention` tag) already comes from the **weaver hooks → events → SSE**
pipeline. The browser stream's *only* job is interacting with a PTY. So we
collapse the two degraded input paths into one real one and delete the rest.

[xterm.js]: https://xtermjs.org/

## 2. Current architecture (what exists today)

```
claude/agent  ──hooks──▶  weaver hook --event ...  ──▶  events table
   │                                                       │
   │ runs inside                                           │ monitor loop (1.5s tick)
   ▼                                                       ▼
tmux session "weaver-<id>"  ◀──capture-pane──  monitor  ──record "status"/"screen"──▶ EventBus
   │                                                       │
   │                                                       ▼
   │                                              SSE /api/sessions/{id}/events
   ▼                                                       │
POST /send  (tmux send-keys)                               ▼
POST /interrupt (tmux send Escape)                  SessionDetail.vue
GET  /pane  (capture-pane snapshot)            <pre>{{ screen }}</pre> + submit box
```

Relevant code:

- `crates/loom/src/tmux.rs` — thin async wrapper over the `tmux` binary
  (`new_session`, `send_text`, `send_keys`, `capture`, `has_session`,
  `kill_session`). Sessions are created detached with no `-x/-y` (default 80×24).
- `crates/loom/src/monitor.rs` — 1.5s loop; `capture-pane` → hash → stillness /
  idle detection, and emits a `screen` event with the full pane text on every
  change.
- `crates/loom/src/web.rs` — axum router. Has `POST /sessions/{id}/send`,
  `POST /sessions/{id}/interrupt`, `GET /sessions/{id}/pane`, plus the rest.
- `crates/loom/frontend/src/views/SessionDetail.vue` — renders the `screen` SSE
  event into an auto-escaped `<pre>`, and a submit form (`send`/`stop`).

## 3. Target architecture

```
                         GET /api/sessions/{id}/terminal   (WebSocket)
browser (xterm.js)  ◀────────────── binary frames ──────────────▶  loom
   term.write(bytes)                                                  │
   term.onData ─▶ 0x00<utf8>                          spawn PTY: tmux attach -t =weaver-<id>
   fit/resize  ─▶ 0x01<cols><rows>                       │  master ↕ slave
                                                          ▼
                                              tmux session "weaver-<id>" (unchanged)
```

The hooks → events → SSE data plane and the monitor heartbeat are **untouched**.
The terminal is a parallel, purely-interactive channel.

### Interaction-model decision

| Concern | Plane | Keep? |
|---|---|---|
| Status (lifecycle + the agent's `attention` tag), pending prompt, notes, issues, diff | hooks → events → SSE | **keep** |
| Idle/orphan detection (screen stillness hash) | monitor `capture-pane` | **keep** (internal heartbeat) |
| Interacting with the agent (keystrokes, keys, TUIs) | new PTY WebSocket | **add** |
| Line input box (`POST /send`) | degraded interaction | **remove** |
| Interrupt button (`POST /interrupt`) | degraded interaction | **remove** (just press Esc/Ctrl-C in the terminal) |
| Read-only screen mirror (`<pre>` + `screen` SSE event) | degraded view | **remove** |
| Screen snapshot (`GET /pane`) | pull snapshot | **remove** — redundant: `tmux attach` repaints the live screen on connect, so there's no separate snapshot need. See §10 for the one case that would justify re-adding it. |

## 4. Backend: the WebSocket ⇄ PTY bridge

New module `crates/loom/src/terminal.rs`, registered as `pub mod terminal;` in
`crates/loom/src/lib.rs`, wired into the router in `web.rs`:

```rust
.route("/sessions/{id}/terminal", get(crate::terminal::terminal_ws))
```

### 4.1 Dependencies

`crates/loom/Cargo.toml`:

```toml
axum = { version = "0.8", features = ["ws"] }   # add the `ws` feature
portable-pty = "0.9"                            # wezterm's cross-platform PTY
futures-util = "0.3"                            # split the socket into sink/stream
```

`portable-pty` 0.9 API (verified):

- `native_pty_system() -> Box<dyn PtySystem + Send>`
- `PtySystem::openpty(PtySize { rows, cols, pixel_width, pixel_height }) -> PtyPair`
- `PtyPair { master, slave }`
- `SlavePty::spawn_command(CommandBuilder) -> Box<dyn Child + Send + Sync>`
- `MasterPty::try_clone_reader() -> Box<dyn Read + Send>`
- `MasterPty::take_writer() -> Box<dyn Write + Send>`
- `MasterPty::resize(PtySize)`

axum 0.8 `Message` (verified): `Binary(Bytes)`, `Text(Utf8Bytes)`, `Ping/Pong(Bytes)`,
`Close(Option<CloseFrame>)`. `Vec<u8>: Into<Bytes>`, so we construct output frames
with `Message::Binary(chunk.into())`.

### 4.2 Wire protocol

Binary frames both directions; one-byte opcode prefix on the client→server hot
path (the ecosystem-standard ttyd/terminado/VS Code shape):

- **server → client:** raw PTY output bytes → `term.write(uint8array)`.
- **client → server:**
  - `0x00 <utf8…>` — keystrokes → PTY writer.
  - `0x01 <cols:u16_be> <rows:u16_be>` — resize → `master.resize(...)`.

No JSON on the hot path. (Text frames are tolerated as raw input for robustness,
but the frontend always sends binary.)

### 4.3 Handler sketch

```rust
pub async fn terminal_ws(
    ws: WebSocketUpgrade,
    State(st): State<AppState>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !same_origin(&headers) {                       // CSWSH defense, see §7
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    let session = match require_session(&st.db, &key).await {   // require_session is now `pub`
        Ok((s, _)) => s,
        Err(_) => return (StatusCode::NOT_FOUND, "no such session").into_response(),
    };
    if !tmux::has_session(&session.tmux_session).await {        // orphaned → no PTY to attach
        return (StatusCode::CONFLICT, "session has no running tmux — adopt it first").into_response();
    }
    let target = session.tmux_session.clone();
    ws.on_upgrade(move |socket| async move {
        let _ = bridge(socket, target).await;
    })
}
```

`bridge()` does the pumping:

1. `openpty(80×24)` (initial; client sends a real size immediately after fit).
2. `CommandBuilder::new("tmux")` with `args(["attach-session", "-t", &tmux::exact(&target)])`,
   inherit `std::env::vars()` (same user/uid → same tmux socket) and force
   `TERM=xterm-256color`. `slave.spawn_command(cmd)` → `child`. `drop(slave)` so
   the PTY sees EOF when the child exits.
3. **PTY → ws:** `try_clone_reader()` read loop on a dedicated `std::thread`
   (blocking I/O) → `tokio::sync::mpsc` → async task → `sink.send(Binary)`.
4. **ws → PTY:** async task reads `stream.next()`, decodes `0x00`/`0x01`,
   forwards input bytes over an mpsc to a blocking writer thread
   (`take_writer()`), and calls `master.resize(...)` on `0x01`.
5. `tokio::select!` on the two async tasks; whichever ends first aborts the other.
6. **Teardown:** `child.kill(); child.wait();` — kills **only the `tmux attach`
   client**, never the session. The session (and the agent) keep running
   detached, so a refresh reconnects and orphan/adopt is unaffected. Blocking
   threads unwind when their channels/fds close.

Why blocking threads, not async PTY: `portable-pty` exposes blocking
`Read`/`Write`. Wrapping each in a dedicated thread feeding an mpsc is simpler and
more portable than a `tokio::fs::File`-from-rawfd dance, and a viewer count of
O(humans-watching) makes the thread cost a non-issue.

Backpressure: bounded mpsc (256). If the browser is slow, `blocking_send` blocks
the reader thread, which back-pressures the PTY, which back-pressures `tmux
attach` — correct and lossless. (Output coalescing is a possible later
optimisation; not needed for v1.)

## 5. tmux changes (`crates/loom/src/tmux.rs`)

1. **Exact-match targets.** Add a helper and use it everywhere a session name is a
   tmux `-t` target, so a name containing `:`/`.`/`%` can't retarget another
   window/pane:

   ```rust
   pub fn exact(name: &str) -> String { format!("={name}") }
   ```

   Use it in `has_session`, `kill_session` (already do `={name}` inline — switch
   to the helper), `capture`, and the new `attach` target.

2. **Let the browser drive size.** In `new_session`, after creating the session:

   ```rust
   let t = exact(name);
   let _ = run(&["set-option", "-t", &t, "window-size", "latest"]).await;
   let _ = run(&["set-option", "-t", &t, "aggressive-resize", "on"]).await;
   ```

   `window-size latest` makes the window track the most-recently-active client
   instead of clamping to the *smallest* attached client (which would pin every
   viewer to 80×24 with dead space). When the browser resizes the PTY → SIGWINCH
   → `tmux attach` client resizes → window follows.

3. **Remove dead code.** After `POST /send` / `/interrupt` are gone, `send_text`
   and `send_keys` have no callers — delete them. `capture` stays (the monitor
   uses it).

## 6. Monitor change (`crates/loom/src/monitor.rs`)

Keep the `capture-pane` + hash for stillness/idle/orphan detection. Delete only
the line that pushes the screen to clients:

```rust
// DELETE: events::emit(&state.bus, &session.branch_id, "screen", json!({ "content": screen }));
```

Nothing consumes `screen` once the detail view uses the PTY, so leaving it would
be dead events on the bus.

## 7. Security

loom binds localhost with `CorsLayer::permissive()` and no auth — fine for a
single-user local tool, but two things matter for a WebSocket that can *type at a
shell*:

- **CSWSH (Cross-Site WebSocket Hijacking).** A localhost bind is **not**
  protection — a malicious page in the operator's browser can open
  `ws://localhost:PORT`. CORS does not apply to WebSockets. Mitigation: validate
  the `Origin` header **before** `on_upgrade`. Require `Origin`'s authority to
  equal the request `Host` (same-origin); allow a *missing* `Origin` (non-browser
  clients like the CLI/tests don't send one). This is cheap and proportionate.
  A short-lived per-connection **ticket** minted from an authenticated REST call
  is the right upgrade *when loom gains real auth* — building a ticket system on
  top of a zero-auth app now would be its own tech debt, so it's deferred (noted
  in code).
- **Terminal escape hazards.** xterm.js parses escapes into its own grid (no
  `innerHTML`), so output rendering is safe by construction. But:
  - **Never** bridge a terminal *report* channel (DECRQSS/DCS, CVE-2019-0542)
    back into the PTY — we only forward `term.onData` (user keystrokes), so this
    is satisfied by not wiring any "report" handler to the socket.
  - **Disable OSC 52** clipboard writes (don't let agent output silently set the
    user's clipboard).
  - **Don't reflect** agent-set window titles (OSC 0/2) into page chrome.
  - **Allowlist OSC 8 hyperlinks** to `http`/`https`, open with
    `rel="noopener noreferrer"`.
- **Bounds.** Cap input frame size and rate; ignore malformed frames.
- **Lifecycle safety.** Closing a tab kills the attach client only, never the
  agent/session.

## 8. Frontend

### 8.1 Dependencies (`crates/loom/frontend/package.json`)

Use the **scoped** packages (unscoped `xterm`/`xterm-addon-*` are deprecated).
Versions verified against this environment's npm:

```jsonc
"@xterm/xterm": "^6.0.0",
"@xterm/addon-fit": "^0.11.0",
"@xterm/addon-webgl": "^0.19.0",
"@xterm/addon-unicode11": "^0.9.0"
```

`build.rs` already runs `npm install` when `package.json`/lockfile change, then
`npx rspack build`. rspack has `experiments.css: true`, so the
`@xterm/xterm/css/xterm.css` import is handled.

### 8.2 New component `AgentTerminal.vue`

Responsibilities:

- `new Terminal({ convertEol: false, fontFamily: 'ui-monospace, …', theme, scrollback, allowProposedApi: true })`.
- `term.open(el)`, **then** load addons (order matters):
  - `FitAddon` — fit to container.
  - `Unicode11Addon` — and **activate** it: `term.unicode.activeVersion = '11'`.
  - `WebglAddon` — handle `onContextLoss` → `dispose()` → fall back to the DOM
    renderer (don't leave a dead canvas).
- WebSocket to `` `${wsBase}/api/sessions/${id}/terminal` `` where `wsBase` swaps
  `http`→`ws` / `https`→`wss` on the page origin. `binaryType = 'arraybuffer'`.
- **Output:** `ws.onmessage` → `term.write(new Uint8Array(ev.data))`.
- **Input:** `term.onData(s => ws.send(frame(0x00, utf8(s))))`.
- **Resize:** a `ResizeObserver` on the container → `fit.fit()` → send
  `0x01<cols><rows>`; also send once on open after the first fit.
- **Reconnect:** on `ws.onclose`, show a "disconnected — reconnect" affordance
  (and/or auto-retry with backoff). Because `tmux attach` repaints on connect, a
  reconnect restores the live screen with no extra snapshot.
- **Cleanup:** `onUnmounted` → close ws, `observer.disconnect()`, `term.dispose()`.

Frame helper:

```ts
function frame(op: number, payload: Uint8Array): Uint8Array {
  const out = new Uint8Array(payload.length + 1);
  out[0] = op; out.set(payload, 1); return out;
}
function resizeFrame(cols: number, rows: number): Uint8Array {
  const b = new Uint8Array(5);
  b[0] = 0x01;
  b[1] = cols >> 8; b[2] = cols & 0xff;
  b[3] = rows >> 8; b[4] = rows & 0xff;
  return b;
}
```

### 8.3 `SessionDetail.vue` changes

- Replace the `<pre ref="screenBox">{{ screen }}</pre>` block **and** the submit
  form with `<AgentTerminal :id="id" />`.
- Remove: the `screen` ref, the `screen` branch of the SSE `onStream` handler, the
  `/pane` fetch in `loadAll`, and the `send()` / `stop()` functions.
- Keep: everything else (title/goal/status editing, status/note/issue
  SSE handling, diff, archive, remove, adopt).

## 9. CLI (`crates/loom/src/bin/loom.rs`)

- `loom attach` stays — it already `exec`s a real `tmux attach`. Switch its
  target to `tmux::exact(session)` for consistency.
- Remove `loom send` and `loom interrupt` subcommands and their `cmd_send` /
  `cmd_interrupt` (their endpoints are gone). `loom attach` is the CLI's PTY
  interaction surface.
- Update the file's top doc comment (drops "send/interrupt").

## 10. Removed surface, blast radius & migration

Removed: `POST /sessions/{id}/send`, `POST /sessions/{id}/interrupt`,
`GET /sessions/{id}/pane`, the `screen` SSE event, `tmux::send_text`,
`tmux::send_keys`, `loom send`, `loom interrupt`, the `<pre>` renderer, the submit
box.

Consumers to update (verified by grep):

- `crates/loom/tests/integration.rs` — the "text sent to the session reaches the
  pane" block (uses `/send` + `/pane`) and the "interrupt" block (uses
  `/interrupt`). **Replace** with a WebSocket round-trip smoke test: connect to
  `/api/sessions/{id}/terminal`, send `0x00` + `"echo WS_MARKER\n"`, read output
  frames until the marker appears, assert `has_session` still true afterward. Add
  `tokio-tungstenite` (matching axum's `0.29`) as a dev-dependency. (No `Origin`
  header → passes `same_origin`.)
- `e2e/tests/detail.spec.ts` — the third test "sending a line … live screen"
  uses the submit box, `<pre>`, and `waitForPane`. **Replace** with: load detail,
  assert `.xterm` (the terminal) renders; optionally type `echo MARKER` and assert
  it appears in `.xterm-rows`.
- `e2e/fixtures/weaver.ts` — `waitForPane()` (hits `/pane`) becomes unused;
  remove it.

`SessionList.vue` does **not** render the screen, so dashboard tiles are
unaffected.

### The one case that would justify keeping `GET /pane`

At-a-glance **tile previews** on the dashboard (a thumbnail of each agent's
screen without opening a full PTY per tile) is a legitimately different
cardinality from interaction. If we want that later, re-add `GET /pane` as a
cheap *pull* endpoint (consumed by the 3s list refresh) — **not** the `screen`
SSE push, and **not** a PTY per tile. It's out of scope here because no tile
preview UI exists today; adding the endpoint now with no consumer would be the
exact tech debt we're removing.

## 11. Testing plan

- **Rust unit:** `same_origin()` cases (missing Origin → allow; matching → allow;
  cross → reject).
- **Rust integration:** the WebSocket round-trip described in §10 (a shell session
  echoes the marker; session survives disconnect).
- **e2e (Playwright):** terminal renders; (optional) typed command echoes.
- **Manual smoke:** launch a claude session, open the detail view, confirm the
  full REPL works (arrow keys, Esc to interrupt, resize reflows), close the tab,
  reopen → reconnects to the same live session; `loom ps` still shows it running.

## 12. Implementation checklist (suggested order)

1. `Cargo.toml`: add `axum` `ws` feature, `portable-pty`, `futures-util`. ✅ (done)
2. `tmux.rs`: `exact()`, use it in `has_session`/`kill_session`/`capture`;
   `window-size latest` + `aggressive-resize on` in `new_session`; remove
   `send_text`/`send_keys`.
3. `terminal.rs`: the bridge (handler + `bridge()` + `same_origin()` + tests).
4. `lib.rs`: `pub mod terminal;`.
5. `web.rs`: `pub` on `require_session`; add the `/terminal` route; remove
   `/send`, `/interrupt`, `/pane` routes + handlers + `SendReq`/pane structs;
   update module doc.
6. `monitor.rs`: delete the `screen` emit.
7. `bin/loom.rs`: drop `send`/`interrupt` subcommands; `exact()` in `attach`.
8. Frontend: deps; `AgentTerminal.vue`; rewire `SessionDetail.vue`.
9. Tests: rewrite the integration `/send`+`/pane`+`/interrupt` block as a WS
   round-trip; rewrite `detail.spec.ts`; drop `waitForPane`.
10. `cargo fmt`, `cargo clippy`, `cargo test -p loom`, frontend build, e2e.

## 13. Open questions / future work

- **Per-viewer independent sizing.** With `window-size latest`, multiple
  simultaneous viewers fight over the window size. True per-viewer geometry needs
  a window-per-viewer + `aggressive-resize on`, or tmux control-mode (`-CC`). Not
  needed for the common single-viewer case; revisit if multi-watch becomes real.
- **Output coalescing / flow control** for very chatty agents (batch PTY reads
  per animation frame). Defer until measured.
- **Auth/tickets** when loom grows beyond single-user localhost (see §7).
- **Scrollback** beyond xterm's buffer relies on tmux copy-mode; fine for v1.
```
