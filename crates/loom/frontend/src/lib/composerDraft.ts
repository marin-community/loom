// The conversation composer's in-progress text, kept in browser-local Web
// Storage so a page refresh (or a lost tab) never eats a half-typed message.
// Best-effort by design: a storage failure is swallowed — persistence must
// never block typing, and a draft that fails to save still sends fine.

export interface DraftStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

const KEY_PREFIX = 'loom.composer-draft.';

// Some privacy modes fence off localStorage so that accessing it throws, and
// callers evaluate arguments before the helpers' guards run — resolve the
// storage itself behind a guard so an unavailable one reads as "none".
export function draftStorage(): DraftStorage | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

function draftKey(sessionId: string): string {
  return `${KEY_PREFIX}${sessionId}`;
}

export function restoreDraft(sessionId: string, storage: DraftStorage | null): string {
  if (!storage) return '';
  try {
    return storage.getItem(draftKey(sessionId)) ?? '';
  } catch {
    return '';
  }
}

// An empty draft clears the slot, so a successful send forgets the message.
export function saveDraft(sessionId: string, text: string, storage: DraftStorage | null): void {
  if (!storage) return;
  try {
    const key = draftKey(sessionId);
    if (text) storage.setItem(key, text);
    else storage.removeItem(key);
  } catch {
    /* a draft that cannot be stored is still usable in memory */
  }
}
