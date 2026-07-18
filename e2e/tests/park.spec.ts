import { test, expect, type Page } from '../fixtures/weaver';

// The fleet list's resting shelf + manual drag order (docs/loom-ui.md "resting
// shelf"). Parking keeps a session — terminal, worktree, history all intact —
// but collapses it out of the live list so a stale fleet doesn't drag the eye.

async function parkView(baseUrl: string, id: string) {
  return (await (await fetch(`${baseUrl}/api/sessions/${id}`)).json()) as {
    park: string | null;
    sort_order: number | null;
  };
}

test.describe('parking sessions', () => {
  test('park from the row menu shelves a session; keep-live returns it', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Refactor the auth module', name: 'auth-refactor' });
    await page.goto(weaver.baseUrl);
    const inLive = page.getByTestId('session-list').locator(`[data-session-id="${s.id}"]`);
    await expect(inLive).toBeVisible();

    // Park via the ⋯ menu (the keyboard / no-drag path).
    await page.locator(`[data-session-id="${s.id}"]`).getByTestId('row-actions').click();
    await page.getByTestId('row-action-park').click();

    // It leaves the live list and rests on the shelf, labelled.
    await expect(inLive).toHaveCount(0);
    const onShelf = page.getByTestId('parked-shelf').locator(`[data-session-id="${s.id}"]`);
    await expect(onShelf).toBeVisible();
    await expect(onShelf).toContainText('parked');
    await expect.poll(async () => (await parkView(weaver.baseUrl, s.id)).park).toBe('parked');

    // Keep-live returns it to the live list and records the override.
    await onShelf.getByTestId('parked-keep-live').click();
    await expect(inLive).toBeVisible();
    await expect.poll(async () => (await parkView(weaver.baseUrl, s.id)).park).toBe('active');
  });

  test('a session awaiting external review stays live (sunk, not shelved)', async ({ page, weaver }) => {
    // A review-wait mark sinks a row below the calm default but must NOT hide it
    // on the shelf — an open PR awaiting review is still yours to glance at.
    const live = await weaver.seedSession({ goal: 'Add a health endpoint', name: 'health' });
    const review = await weaver.seedSession({ goal: 'Wire the readiness probe', name: 'probe' });
    await weaver.setTag(review, 'awaiting', 'review'); // PARKED sort ladder value

    await page.goto(weaver.baseUrl);

    // The review row stays in the live list — never banished to the shelf.
    await expect(
      page.getByTestId('session-list').locator(`[data-session-id="${review.id}"]`),
    ).toBeVisible();
    await expect(
      page.getByTestId('parked-shelf').locator(`[data-session-id="${review.id}"]`),
    ).toHaveCount(0);
    // The calm one stays live too.
    await expect(
      page.getByTestId('session-list').locator(`[data-session-id="${live.id}"]`),
    ).toBeVisible();
  });

  test('a loud signal keeps a hand-parked thread in the live list', async ({ page, weaver }) => {
    // Parked by hand, but the agent then raises attention: it must surface.
    const s = await weaver.seedSession({ goal: 'Rework the payment flow', name: 'payment' });
    await fetch(`${weaver.baseUrl}/api/sessions/${s.id}`, {
      method: 'PATCH',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ park: 'parked' }),
    });
    await weaver.setStatus(s, 'attention', 'Ready for review');

    await page.goto(weaver.baseUrl);
    await expect(
      page.getByTestId('session-list').locator(`[data-session-id="${s.id}"]`),
    ).toBeVisible();
  });

  test('dragging a row reorders the live list and persists the order', async ({ page, weaver }) => {
    // Created oldest→newest, so the default order is newest-first: ccc, bbb, aaa.
    const a = await weaver.seedSession({ goal: 'first', name: 'aaa' });
    await weaver.seedSession({ goal: 'second', name: 'bbb' });
    const c = await weaver.seedSession({ goal: 'third', name: 'ccc' });

    await page.goto(weaver.baseUrl);
    const rows = page.getByTestId('session-list').getByTestId('session-card');
    await expect(rows).toHaveCount(3);
    await expect(rows.first()).toHaveAttribute('data-session-id', c.id); // newest first

    // Drag the oldest (aaa) up onto the top of the newest (ccc) → aaa leads.
    await synthDrag(page, a.id, c.id);

    await expect(rows.first()).toHaveAttribute('data-session-id', a.id);
    // The dragged row now carries a manual sort key (persisted — poll past the
    // in-flight PATCH), and the order survives a reload.
    await expect
      .poll(async () => (await parkView(weaver.baseUrl, a.id)).sort_order)
      .not.toBeNull();
    await page.reload();
    await expect(
      page.getByTestId('session-list').getByTestId('session-card').first(),
    ).toHaveAttribute('data-session-id', a.id);
  });
});

// HTML5 drag-and-drop via synthetic dispatch with a shared DataTransfer — a real
// mouse drag is unreliable here (see the shared-panel drag note in memory), so we
// fire dragstart on the grip and dragover/drop on the target row's top half.
async function synthDrag(page: Page, sourceId: string, targetId: string) {
  await page.evaluate(
    ({ sourceId, targetId }) => {
      const dt = new DataTransfer();
      const grip = document.querySelector(
        `[data-session-id="${sourceId}"] [data-testid="session-drag"]`,
      )!;
      const target = document.querySelector(`[data-session-id="${targetId}"]`)!;
      const rect = target.getBoundingClientRect();
      const at = { clientX: rect.left + 12, clientY: rect.top + 2 }; // top half → drop above
      const ev = (type: string, extra: object = {}) =>
        new DragEvent(type, { bubbles: true, cancelable: true, dataTransfer: dt, ...extra });
      grip.dispatchEvent(ev('dragstart'));
      target.dispatchEvent(ev('dragover', at));
      target.dispatchEvent(ev('drop', at));
      grip.dispatchEvent(ev('dragend'));
    },
    { sourceId, targetId },
  );
}
