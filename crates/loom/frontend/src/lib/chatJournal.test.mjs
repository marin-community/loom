import test from 'node:test';
import assert from 'node:assert/strict';
import { ChatJournalReconciler } from './chatJournal.ts';

const block = (text, outcome = null) => ({
  turn: 2,
  seq: 4,
  text,
  outcome,
});

test('a stream commit wins over a snapshot that was already in flight', () => {
  const blocks = new Map();
  const journal = new ChatJournalReconciler(blocks);
  const request = journal.beginSnapshot();

  const resolved = block('permission', { option_id: 'allow-once' });
  journal.applyStream(resolved);
  const accepted = journal.applySnapshot(request, [block('permission')]);

  assert.deepEqual(accepted, []);
  assert.equal(blocks.get('2:4'), resolved);
});

test('a later snapshot repairs a block after a missed stream update', () => {
  const blocks = new Map();
  const journal = new ChatJournalReconciler(blocks);
  journal.applyStream(block('open'));

  const request = journal.beginSnapshot();
  const repaired = block('durable winner');
  assert.deepEqual(journal.applySnapshot(request, [repaired]), [repaired]);
  assert.equal(blocks.get('2:4'), repaired);
});

test('snapshot and stream blocks at different positions merge normally', () => {
  const blocks = new Map();
  const journal = new ChatJournalReconciler(blocks);
  const request = journal.beginSnapshot();
  journal.applyStream({ turn: 3, seq: 0, text: 'live' });
  journal.applySnapshot(request, [{ turn: 2, seq: 9, text: 'snapshot' }]);

  assert.deepEqual([...blocks.keys()].sort(), ['2:9', '3:0']);
});

test('reset invalidates an older snapshot request', () => {
  const blocks = new Map();
  const journal = new ChatJournalReconciler(blocks);
  const oldRequest = journal.beginSnapshot();

  journal.reset();
  assert.deepEqual(journal.applySnapshot(oldRequest, [block('old session')]), []);
  assert.equal(blocks.size, 0);
});
