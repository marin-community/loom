import test from 'node:test';
import assert from 'node:assert/strict';
import {
  canSend,
  conversationState,
  effectiveAttention,
  lifecycleActions,
  messageOf,
  remedyAction,
  remedyButtonAction,
  transitionProgress,
} from './sessionState.ts';

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

test('a suspended session reads as dormant but reachable', () => {
  const session = {
    status: 'suspended',
    last_activity_at: '2026-10-01T20:00:00Z',
    branch: { description: 'What the agent was doing', tags: [] },
  };

  // Sending is a wake signal, so the composer stays open.
  assert.equal(canSend(session), true);
  assert.deepEqual(conversationState(session), {
    glyph: '◦',
    label: 'Suspended',
    tone: 'muted',
  });
  // Wake stays a menu action; the standalone remedy button treats dormancy as
  // calm, not a fault to cure.
  assert.equal(remedyAction(session)?.verb, 'wake');
  assert.equal(remedyButtonAction(session), null);
  assert.deepEqual(
    lifecycleActions(session).map((a) => a.verb),
    ['wake', 'archive', 'remove'],
  );
  // A suspended session's description survives — the workstream is intact.
  assert.equal(messageOf(session), 'What the agent was doing');
});

test('the remedy button still surfaces genuine faults', () => {
  const orphaned = {
    status: 'orphaned',
    last_activity_at: '2026-10-01T20:00:00Z',
    branch: { tags: [] },
  };
  assert.equal(remedyButtonAction(orphaned)?.verb, 'adopt');
  const archived = {
    status: 'archived',
    last_activity_at: '2026-10-01T20:00:00Z',
    branch: { tags: [] },
  };
  assert.equal(remedyButtonAction(archived)?.verb, 'recover');
});

test('a running session offers suspend alongside archive', () => {
  const session = {
    status: 'running',
    last_activity_at: '2026-10-01T20:00:00Z',
    branch: { tags: [] },
  };
  assert.deepEqual(
    lifecycleActions(session).map((a) => a.verb),
    ['suspend', 'archive', 'remove'],
  );
  assert.equal(remedyAction(session), null);
});

test('suspending and waking transitions get their own labels', () => {
  const suspending = {
    status: 'running',
    transition: { kind: 'suspending', step: 'Stopping agent', started_at: '2026-10-01T20:00:00Z' },
    branch: { tags: [] },
  };
  assert.deepEqual(conversationState(suspending), {
    glyph: '▶',
    label: 'Suspending — Stopping agent',
    tone: 'info',
  });

  const waking = {
    status: 'suspended',
    transition: { kind: 'waking', step: 'Resuming agent', started_at: '2026-10-01T20:00:00Z' },
    branch: { tags: [] },
  };
  assert.deepEqual(conversationState(waking), {
    glyph: '▶',
    label: 'Waking — Resuming agent',
    tone: 'info',
  });
});

test('the progress strip narrates handoff, waking, and suspending only', () => {
  const withTransition = (kind, step) => ({
    status: 'running',
    transition: { kind, step, started_at: '2026-10-01T20:00:00Z' },
    branch: { tags: [] },
  });

  // Handoff: the original strip — paused, with its live step or default.
  assert.deepEqual(transitionProgress(withTransition('handoff', undefined)), {
    testid: 'handoff-progress',
    title: 'Handoff in progress',
    detail: 'Transferring session context',
    paused: true,
  });
  assert.equal(
    transitionProgress(withTransition('handoff', 'Switching to codex')).detail,
    'Switching to codex',
  );

  // Waking and suspending: the runtime coming back / being parked — not paused.
  assert.deepEqual(transitionProgress(withTransition('waking', undefined)), {
    testid: 'waking-progress',
    title: 'Waking session',
    detail: 'Restarting the session runtime',
    paused: false,
  });
  assert.equal(
    transitionProgress(withTransition('suspending', 'Stopping agent')).detail,
    'Stopping agent',
  );

  // Every other transition reads through the header's state line instead.
  assert.equal(transitionProgress(withTransition('archiving', 'Removing worktree')), null);
  assert.equal(transitionProgress(withTransition('adopting', undefined)), null);
  assert.equal(transitionProgress({ status: 'running', branch: { tags: [] } }), null);
});
