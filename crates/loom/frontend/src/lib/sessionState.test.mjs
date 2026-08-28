import test from 'node:test';
import assert from 'node:assert/strict';
import { canSend, conversationState, effectiveAttention, messageOf } from './sessionState.ts';

test('an orphaned ACP runtime surfaces Loom recovery guidance', () => {
  const session = {
    status: 'orphaned',
    last_activity_at: '2026-08-18T20:00:00Z',
    branch: {
      description: 'Agent status from before the disconnect',
      tags: [
        {
          key: 'runtime',
          value: 'attention',
          note: 'Loom lost its connection. Select Adopt to reconnect.',
          set_by: 'loom',
          set_at: '2026-08-18T20:00:00Z',
        },
      ],
    },
  };

  assert.equal(messageOf(session), 'Loom lost its connection. Select Adopt to reconnect.');
  assert.deepEqual(effectiveAttention(session), {
    key: 'runtime',
    level: 'attention',
    by: 'loom',
    raisedBy: 'watch',
    note: 'Loom lost its connection. Select Adopt to reconnect.',
    stale: false,
  });
});

test('a handoff is presented as a paused lifecycle transition', () => {
  const session = {
    status: 'running',
    transition: {
      kind: 'handoff',
      step: 'Transferring context to codex',
      started_at: '2026-08-28T20:00:00Z',
    },
    branch: { tags: [] },
  };

  assert.equal(canSend(session), false);
  assert.deepEqual(conversationState(session), {
    glyph: '▶',
    label: 'Handing off — Transferring context to codex',
    tone: 'info',
  });
});
