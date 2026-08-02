// A session's event feed, as one topic on the tab's multiplexed stream.
//
// SessionDetail, ArtifactsPanel, and TerminalConversation all want the *same*
// per-session feed, so they share a subscription here — and `eventStream` in
// turn folds every topic in the tab onto a single connection, so the whole UI
// costs one of the browser's 6 per-origin sockets instead of three. See
// `lib/eventStream.ts` for why that cap matters.
//
// Each subscriber gets its own handle, so components stay free to open and close
// independently (keep-alive activate/deactivate) without tracking each other.

import { openTopic, type TopicHandle } from './eventStream';
import type { WeaverEvent } from '../types';

type Listener = (event: WeaverEvent) => void;

export interface SessionEventsHandle {
  /** Subscribe this handle to one event kind. */
  on(kind: string, fn: Listener): void;
  /** Drop this handle's subscriptions. */
  close(): void;
}

export function openSessionEvents(id: string): SessionEventsHandle {
  const topic: TopicHandle = openTopic(`session:${id}`);
  return {
    on(kind: string, fn: Listener) {
      topic.on(kind, (data) => fn(data as WeaverEvent));
    },
    close() {
      topic.close();
    },
  };
}
