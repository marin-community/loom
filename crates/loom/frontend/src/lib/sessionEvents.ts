// One shared `/api/sessions/{id}/events` EventSource per session id.
//
// Browsers cap HTTP/1.1 at 6 connections per origin, and that cap is a hard
// wall: with 6 held open, an ordinary `fetch()` never resolves — no error, no
// timeout, nothing in the server log, because the request never leaves the
// browser. SessionDetail, ArtifactsPanel, and TerminalConversation all want the
// *same* per-session stream, so opening one each spent half the page's entire
// connection budget on duplicates of a single stream.
//
// Subscribers share one connection here and the stream closes when the last one
// leaves. Each subscriber gets its own handle, so components stay free to open
// and close independently (keep-alive activate/deactivate) without tracking each
// other.

type Listener = (ev: MessageEvent) => void;

interface Shared {
  es: EventSource;
  /** kind -> live subscriber callbacks */
  listeners: Map<string, Set<Listener>>;
  /** kinds already wired onto the EventSource (wire each at most once) */
  wired: Set<string>;
  refs: number;
}

const streams = new Map<string, Shared>();

export interface SessionEventsHandle {
  /** Subscribe this handle to one event kind. */
  on(kind: string, fn: Listener): void;
  /** Drop this handle's subscriptions; closes the stream when it was the last. */
  close(): void;
}

export function openSessionEvents(id: string): SessionEventsHandle {
  let shared = streams.get(id);
  if (!shared) {
    shared = {
      es: new EventSource(`/api/sessions/${id}/events`),
      listeners: new Map(),
      wired: new Set(),
      refs: 0,
    };
    streams.set(id, shared);
  }
  const stream = shared;
  stream.refs += 1;

  const mine: Array<[string, Listener]> = [];
  let closed = false;

  return {
    on(kind: string, fn: Listener) {
      if (closed) return;
      let set = stream.listeners.get(kind);
      if (!set) {
        set = new Set();
        stream.listeners.set(kind, set);
      }
      set.add(fn);
      mine.push([kind, fn]);
      if (stream.wired.has(kind)) return;
      stream.wired.add(kind);
      // Resolve subscribers at delivery time, not wiring time, so a handle that
      // subscribes later still receives this kind. One subscriber throwing must
      // not stop delivery to the rest.
      stream.es.addEventListener(kind, (e) => {
        for (const cb of [...(stream.listeners.get(kind) ?? [])]) {
          try {
            cb(e as MessageEvent);
          } catch {
            /* a subscriber's handler must not break the fan-out */
          }
        }
      });
    },
    close() {
      if (closed) return;
      closed = true;
      for (const [kind, fn] of mine) stream.listeners.get(kind)?.delete(fn);
      mine.length = 0;
      stream.refs -= 1;
      if (stream.refs > 0) return;
      stream.es.close();
      // Only drop the registry entry if it is still ours: a component that
      // re-opened the same id mid-teardown has already installed a fresh one.
      if (streams.get(id) === stream) streams.delete(id);
    },
  };
}
