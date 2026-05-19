import { test, expect } from '../fixtures/weaver';

test.describe('status reflects hook events', () => {
  test('detail view updates the status badge via SSE after a hook', async ({ page, weaver }) => {
    const ws = await weaver.seedWorkspace({ goal: 'Watch my status', name: 'hook-detail' });

    await page.goto(`${weaver.baseUrl}/#/w/${ws.id}`);
    const badge = page.getByTestId('status-badge').first();
    await expect(badge).toBeVisible();

    // Flip status via the hook API; the detail view subscribes to SSE.
    await weaver.hook(ws.id, 'working');
    await expect(badge).toHaveText(/working/i);

    await weaver.hook(ws.id, 'waiting');
    await expect(badge).toHaveText(/waiting/i);
  });

  test('list view picks up a hooked status via polling', async ({ page, weaver }) => {
    const ws = await weaver.seedWorkspace({ goal: 'Watch list status', name: 'hook-list' });

    await page.goto(weaver.baseUrl);
    const card = page.locator(`[data-workspace-id="${ws.id}"]`);
    await expect(card.getByTestId('status-badge')).toBeVisible();

    await weaver.hook(ws.id, 'working');

    // The list polls every 3s; allow generous time for it to refresh.
    await expect(card.getByTestId('status-badge')).toHaveText(/working/i, { timeout: 10_000 });
  });
});
