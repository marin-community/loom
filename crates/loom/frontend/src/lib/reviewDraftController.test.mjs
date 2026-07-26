import test from 'node:test';
import assert from 'node:assert/strict';
import { ReviewDraftController } from './reviewDraftController.ts';

const draft = (revision, summary = '') => ({ draft_revision: revision, summary });

test('serializes revisions, freezes finalization, and rejects stale refreshes', async () => {
  const seen = [];
  let releaseFirst;
  const firstGate = new Promise((resolve) => {
    releaseFirst = resolve;
  });
  const controller = new ReviewDraftController({
    async saveSummary(current, summary) {
      seen.push(['summary', current?.draft_revision, summary]);
      return draft((current?.draft_revision ?? 0) + 1, summary);
    },
    onDraft() {},
    onSummary() {},
  });
  controller.hydrate(draft(1));

  const staleRefresh = controller.beginRefresh();
  const edit = controller.command(async (current) => {
    seen.push(['edit', current.draft_revision]);
    await firstGate;
    return draft(current.draft_revision + 1, current.summary);
  });
  const add = controller.command(async (current) => {
    seen.push(['add', current.draft_revision]);
    return draft(current.draft_revision + 1, current.summary);
  });
  const submit = controller.freeze(async (current) => {
    seen.push(['submit', current.draft_revision]);
    return { draft: null, result: current.draft_revision };
  });

  await assert.rejects(
    controller.command(async () => draft(99)),
    /finalizing/,
  );
  releaseFirst();
  assert.deepEqual(await Promise.all([edit, add, submit]), [draft(2), draft(3), 3]);
  assert.deepEqual(seen, [
    ['edit', 1],
    ['add', 2],
    ['submit', 3],
  ]);
  assert.equal(controller.acceptRefresh(staleRefresh, draft(1)), false);
  assert.equal(controller.draft, null);
});

test('flush preserves summary typed while an earlier save is in flight', async () => {
  let releaseSave;
  const saveGate = new Promise((resolve) => {
    releaseSave = resolve;
  });
  const saved = [];
  const controller = new ReviewDraftController({
    async saveSummary(current, summary) {
      saved.push(summary);
      if (saved.length === 1) await saveGate;
      return draft((current?.draft_revision ?? 0) + 1, summary);
    },
    onDraft() {},
    onSummary() {},
  });
  controller.hydrate(draft(1));
  controller.editSummary('first');
  const flushing = controller.flush();
  await Promise.resolve();
  controller.editSummary('newest');
  releaseSave();
  await flushing;
  assert.deepEqual(saved, ['first', 'newest']);
  assert.equal(controller.draft.summary, 'newest');
  assert.equal(controller.summaryDirty, false);

  const releaseBarrier = await controller.barrier();
  const settledRefresh = controller.beginRefresh();
  assert.equal(controller.acceptRefresh(settledRefresh, draft(3, 'newest')), true);
  await assert.rejects(
    controller.command(async () => draft(99)),
    /finalizing/,
  );
  releaseBarrier();
  assert.equal(
    (
      await controller.command(async (current) =>
        draft(current.draft_revision + 1, current.summary),
      )
    ).draft_revision,
    4,
  );
});
