// One `/api/events` EventSource for the whole tab.
//
// Browsers cap HTTP/1.1 at 6 connections per origin and that cap is a hard wall:
// with 6 held open an ordinary `fetch()` never resolves — no error, no timeout,
// nothing in the server log, because the request never leaves the browser. The
// UI wants four live streams (fleet layout, a session's events, its ACP chat,
// the operator log tail), so one tab used to spend 3 slots and two tabs spent
// every one of them. They are multiplexed onto a single connection here.
//
// Subscribers name a topic (`layout`, `logs`, `session:<id>`, `chat:<id>`) and
// get a handle whose lifetime they own, so components stay free to open and
// close independently (keep-alive activate/deactivate) without tracking each
// other.
//
// Reconnects are deliberately one-directional: adding a topic reopens the
// connection, dropping one does not. SSE has no client→server channel, so a
// topic set only changes by reconnecting, and reconnecting on every unsubscribe
// would churn the shared stream on each navigation. A dropped topic simply stops
// having listeners and is pruned at the next reconnect, which keeps returning to
// a recently-viewed session free.

type Listener = (data: unknown) => void;

interface Sub {
  topic: string;
  /** event name -> this subscriber's callbacks */
  listeners: Map<string, Set<Listener>>;
  /** fired on every (re)connect that covers this topic */
  opens: Set<() => void>;
  closed: boolean;
}

export interface TopicHandle {
  /** Subscribe to one event name within the topic. */
  on(event: string, fn: Listener): void;
  /**
   * Run `fn` whenever the underlying stream (re)connects, including immediately
   * if it is already connected. A reconnect can drop frames, so this is where a
   * subscriber re-snapshots.
   */
  onOpen(fn: () => void): void;
  /**
   * Force the shared connection to reopen, re-resolving every topic on it
   * server-side. Needed when a topic's *source* was replaced rather than its
   * content changing — an ACP provider handoff swaps the task behind
   * `chat:<id>`, and only a reconnect re-binds it.
   */
  refresh(): void;
  /** Drop this handle's subscriptions. */
  close(): void;
}

/** topic -> live subscribers */
const subs = new Map<string, Set<Sub>>();

let source: EventSource | null = null;
/** Topics the *current* connection was opened with. */
let connected = new Set<string>();
let reconnectQueued = false;
let forceReconnect = false;
let retryTimer: ReturnType<typeof setTimeout> | null = null;
let retryDelay = 0;

const BASE_RETRY_MS = 250;
const MAX_RETRY_MS = 5000;

function liveTopics(): string[] {
  return [...subs.keys()];
}

/** A frame the server multiplexes onto the stream. */
interface Frame {
  topic: string;
  event: string;
  data: unknown;
}

function dispatch(frame: Frame): void {
  for (const sub of [...(subs.get(frame.topic) ?? [])]) {
    if (sub.closed) continue;
    for (const fn of [...(sub.listeners.get(frame.event) ?? [])]) {
      try {
        fn(frame.data);
      } catch {
        /* a subscriber's handler must not break the fan-out */
      }
    }
  }
}

function fireOpen(topics: Iterable<string>): void {
  for (const topic of topics) {
    for (const sub of [...(subs.get(topic) ?? [])]) {
      if (sub.closed) continue;
      for (const fn of [...sub.opens]) {
        try {
          fn();
        } catch {
          /* one subscriber's re-snapshot must not block the others */
        }
      }
    }
  }
}

function closeSource(): void {
  source?.close();
  source = null;
  connected = new Set();
}

function connect(): void {
  closeSource();
  if (retryTimer) {
    clearTimeout(retryTimer);
    retryTimer = null;
  }
  const topics = liveTopics();
  if (topics.length === 0) return;

  const stream = new EventSource(`/api/events?topics=${encodeURIComponent(topics.join(','))}`);
  source = stream;
  connected = new Set(topics);

  stream.onmessage = (e: MessageEvent) => {
    let frame: Frame;
    try {
      frame = JSON.parse(e.data) as Frame;
    } catch {
      return; // ignore a malformed frame
    }
    dispatch(frame);
  };

  stream.onopen = () => {
    // A superseded connection must not re-snapshot subscribers against the
    // topic set its replacement now owns.
    if (source !== stream) return;
    retryDelay = 0;
    fireOpen(connected);
  };

  // EventSource retries transient failures itself, but browsers do not
  // consistently reconnect after a clean EOF (a server restart, a provider
  // handoff closing the task broadcast). Drive the retry explicitly, backing off
  // so a server that stays down is not hammered.
  stream.onerror = () => {
    if (source !== stream) return;
    if (stream.readyState !== EventSource.CLOSED) return;
    closeSource();
    if (retryTimer || liveTopics().length === 0) return;
    retryDelay = retryDelay ? Math.min(retryDelay * 2, MAX_RETRY_MS) : BASE_RETRY_MS;
    retryTimer = setTimeout(() => {
      retryTimer = null;
      connect();
    }, retryDelay);
  };
}

/**
 * Reconnect only if some live topic is missing from the current connection.
 * Coalesced onto a microtask: one navigation re-subscribes several components
 * (detail, conversation, artifacts, terminal) and they must cost one reconnect,
 * not four.
 */
function ensureConnected(force = false): void {
  if (force) forceReconnect = true;
  if (reconnectQueued) return;
  reconnectQueued = true;
  queueMicrotask(() => {
    reconnectQueued = false;
    const forced = forceReconnect;
    forceReconnect = false;
    const topics = liveTopics();
    if (topics.length === 0) {
      closeSource();
      return;
    }
    if (!forced && source && topics.every((t) => connected.has(t))) return;
    connect();
  });
}

export function openTopic(topic: string): TopicHandle {
  const sub: Sub = { topic, listeners: new Map(), opens: new Set(), closed: false };
  let set = subs.get(topic);
  if (!set) {
    set = new Set();
    subs.set(topic, set);
  }
  set.add(sub);
  ensureConnected();

  return {
    on(event: string, fn: Listener) {
      if (sub.closed) return;
      let handlers = sub.listeners.get(event);
      if (!handlers) {
        handlers = new Set();
        sub.listeners.set(event, handlers);
      }
      handlers.add(fn);
    },
    onOpen(fn: () => void) {
      if (sub.closed) return;
      sub.opens.add(fn);
      // Already streaming this topic: no reconnect is coming, so give this
      // subscriber its "connected" edge anyway or it would never snapshot.
      if (source && source.readyState === EventSource.OPEN && connected.has(topic)) {
        queueMicrotask(() => {
          if (!sub.closed) fn();
        });
      }
    },
    refresh() {
      if (sub.closed) return;
      ensureConnected(true);
    },
    close() {
      if (sub.closed) return;
      sub.closed = true;
      sub.listeners.clear();
      sub.opens.clear();
      const peers = subs.get(topic);
      peers?.delete(sub);
      if (peers && peers.size === 0) subs.delete(topic);
      // Deliberately no reconnect: the topic keeps flowing on the existing
      // connection with no listeners until something else forces a reconnect,
      // and is dropped then.
      if (subs.size === 0) closeSource();
    },
  };
}

/** Test seam: drop all state and close the connection. */
export function resetEventStream(): void {
  subs.clear();
  closeSource();
  if (retryTimer) {
    clearTimeout(retryTimer);
    retryTimer = null;
  }
  retryDelay = 0;
}
