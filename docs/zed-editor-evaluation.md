# Evaluation: Zed as the editing backend (instead of code-server)

**Status:** rejected — keep code-server ([embedded-ide.md](embedded-ide.md) is the design of record)
**Scope:** the embedded editor that loom serves beside the terminal
**Question:** Zed is a fast Rust editor — could it replace VS Code (code-server) as loom's editing backend?

## TL;DR

**No — not as a drop-in, and not in any form that preserves loom's model.** The
embedded editor exists because code-server *is VS Code compiled to a web app*:
loom reverse-proxies its HTTP+WebSocket server and renders it in an `<iframe>` on
loom's own origin ([embedded-ide.md §3](embedded-ide.md)). **Zed has no web
frontend.** It is a native GPU application (its own GPUI framework, Metal /
Vulkan / DirectX) with no browser or WASM rendering target. There is nothing to
put in the iframe. A web build is a stated *post-1.0* aspiration with no shipping
timeline — 1.0 itself was targeted for spring 2026 and the web version comes
after that, if at all. Zed's speed advantage is real but **inaccessible** here:
it lives in native rendering, which is exactly the part loom can't use.

## Why the current design forecloses Zed

loom's whole value is the **thin browser client**: the operator reviews a session
from anywhere through the dashboard, no install, no second login. The editor
rides that — an iframe under `require_auth`, same cookie, loopback code-server
behind the proxy. The hard requirement the backend must satisfy is therefore:

> serve its full UI over HTTP + WebSocket so a browser can render it.

code-server meets this by construction. Zed cannot meet it at all today.

## The three ways one *could* use Zed, and why none fit

| Option | What it is | Verdict |
|---|---|---|
| **A. Browser backend (what's asked)** | Render Zed in the iframe like code-server | **Impossible today.** No web/WASM target. Blocked on an unscheduled post-1.0 web build. |
| **B. Native client + Zed remote server** | Run headless Zed in the session container; operators connect with their *local desktop Zed* over SSH | **Breaks the product.** Kills zero-install / browser-only / review-from-anywhere. Every operator must install Zed, hold SSH into the container, and leave the dashboard. Desktop-client-only (no mobile, no shared link). This is a different tool, not a backend swap. |
| **C. Zed-as-ACP-client** | Zed natively speaks the Agent Client Protocol and can *drive* an external agent | **Inverted integration**, not an editor backend. Interesting as a separate "let a Zed user attach to a weaver agent" story; irrelevant to embedding an editor in loom. |

Only **A** answers the question, and **A** does not exist.

## code-server vs Zed, head to head (for this use case)

| Dimension | code-server (current) | Zed |
|---|---|---|
| **Browser-embeddable** | **Yes** — it's the product | **No** — native only, no web target |
| **Fits loom's iframe proxy** | Yes, already shipped | No surface to proxy |
| **Raw editor speed / memory** | Heavier (Electron-lineage, bundles Node, ~150–250 MB image cost) | Much faster & lighter **natively** — but that path is unreachable in-browser |
| **Remote/headless model** | Server-rendered HTML/WS → any browser | Headless server ↔ **native** GPUI client over SSH |
| **Extensions** | Full VS Code ecosystem (Open VSX in code-server) | Smaller, growing; Wasm extensions |
| **License** | MIT — redistribute freely, no obligations | GPL-3.0 editor core — redistributing the binary in our image carries source-availability/share-alike obligations (compliable, but a new constraint vs MIT) |
| **Maturity for this role** | Battle-tested browser IDE | No browser role to be mature in |

The only column that favors Zed (native speed/footprint) is precisely the one
loom cannot consume. Everything loom actually needs from the backend,
code-server already provides.

## What would change the answer

- **Zed ships a real browser/WASM build** that serves its UI over HTTP+WS. Then
  revisit: it could in principle slot into the existing `ide.rs` proxy (which is
  upstream-agnostic) behind the `ide.command` lever. Until that exists this is
  speculative; track the [web discussion](https://github.com/zed-industries/zed/discussions/22953)
  and the [roadmap](https://zed.dev/roadmap).
- **loom's premise changes** to "operators run a native desktop editor" — at
  which point Option B becomes coherent, but that's a product decision, not an
  editor swap.

## Recommendation

**Keep code-server.** It is the right tool for a browser-embedded editor and the
integration is already clean and isolated. If the motivation behind the question
is *weight / speed* rather than Zed specifically, the productive lever is within
the web-native family — e.g. trimming the image, tuning code-server, or (if ever
needed) a lighter web editor — not Zed, which is the wrong axis for an
in-browser surface. Re-open this only if Zed ships a browser build.

## Sources

- Zed remote development (native client ↔ headless server over SSH): <https://zed.dev/docs/remote-development>, <https://zed.dev/blog/remote-development>
- No web/WASM frontend; web build is post-1.0: <https://github.com/zed-industries/zed/discussions/22953>, <https://zed.dev/roadmap>
- Licensing (GPL-3.0 editor core; GPUI Apache-2.0): <https://github.com/zed-industries/zed/blob/main/LICENSE-GPL>, <https://zed.dev/blog/zed-is-now-open-source>
- code-server (VS Code in the browser, MIT): <https://github.com/coder/code-server>
</content>
</invoke>
