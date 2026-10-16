import test from 'node:test';
import assert from 'node:assert/strict';
import { restoreDraft, saveDraft } from './composerDraft.ts';
function memoryStorage() {
  const map = new Map();
  return {
    getItem: (key) => (map.has(key) ? map.get(key) : null),
    setItem: (key, value) => void map.set(key, value),
    removeItem: (key) => void map.delete(key),
    _map: map,
  };
}

test('a saved draft restores verbatim, keyed per session', () => {
  const storage = memoryStorage();
  saveDraft('s-one', 'half-typed thought', storage);
  saveDraft('s-two', 'a different one', storage);

  assert.equal(restoreDraft('s-one', storage), 'half-typed thought');
  assert.equal(restoreDraft('s-two', storage), 'a different one');
});

test('an empty save clears the slot — a sent message stays sent', () => {
  const storage = memoryStorage();
  saveDraft('s-one', 'on its way', storage);
  saveDraft('s-one', '', storage);

  assert.equal(restoreDraft('s-one', storage), '');
  assert.equal(storage._map.size, 0);
});

test('an unknown session restores as the empty string', () => {
  const storage = memoryStorage();
  assert.equal(restoreDraft('never-seen', storage), '');
});

test('an unavailable storage reads as no draft and never blocks a save', () => {
  assert.equal(restoreDraft('s-one', null), '');
  assert.doesNotThrow(() => saveDraft('s-one', 'still typing', null));
  assert.doesNotThrow(() => saveDraft('s-one', '', null));
});

test('storage failures are swallowed — persistence is best-effort', () => {
  const throwing = {
    getItem: () => {
      throw new Error('quota');
    },
    setItem: () => {
      throw new Error('quota');
    },
    removeItem: () => {
      throw new Error('quota');
    },
  };

  assert.equal(restoreDraft('s-one', throwing), '');
  assert.doesNotThrow(() => saveDraft('s-one', 'still typing', throwing));
  assert.doesNotThrow(() => saveDraft('s-one', '', throwing));
});
