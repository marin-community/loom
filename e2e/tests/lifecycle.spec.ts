import { test, expect } from '../fixtures/weaver';

// The lifecycle actions (Archive / Remove) live in the header's ⌄ details menu —
// reachable from any surface and scroll position, not buried at the foot of the
// Overview tab.
test.describe('session lifecycle actions', () => {
  test('Remove (confirmed) deletes the session and returns to the list', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Delete me', name: 'remove-task' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await expect(page.getByRole('heading', { name: 'remove-task' })).toBeVisible();

    await page.getByRole('button', { name: 'details' }).click();

    // Remove uses a native confirm() dialog — accept it.
    page.once('dialog', (dialog) => {
      expect(dialog.type()).toBe('confirm');
      dialog.accept();
    });
    await page.getByRole('button', { name: 'Remove' }).click();

    // Router pushes back to the list.
    await expect(page).toHaveURL(/\/$/);
    await expect(page.getByRole('heading', { name: 'Sessions' })).toBeVisible();
    await expect(page.getByText('No sessions yet.')).toBeVisible();

    // And it is gone server-side.
    const all = await weaver.listSessions();
    expect(all).toHaveLength(0);
  });

  test('dismissing the confirm dialog keeps the session', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Keep me', name: 'keep-task' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole('button', { name: 'details' }).click();

    page.once('dialog', (dialog) => dialog.dismiss());
    await page.getByRole('button', { name: 'Remove' }).click();

    // Still on the detail page, still present server-side.
    await expect(page).toHaveURL(new RegExp(`/s/${s.id}$`));
    const all = await weaver.listSessions();
    expect(all).toHaveLength(1);
  });

  test('Archive (confirmed) tears down the session but keeps its record', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Archive me', name: 'archive-task' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole('button', { name: 'details' }).click();

    page.once('dialog', (dialog) => {
      expect(dialog.type()).toBe('confirm');
      dialog.accept();
    });
    await page.getByRole('button', { name: 'Archive' }).click();

    // The header reloads into the archived state: the lifecycle badge appears
    // and the popover's Archive button goes away (archiving twice is a no-op).
    await expect(page.getByTestId('status-badge')).toHaveText(/archived/i);
    await expect(page.getByRole('button', { name: 'Archive' })).toHaveCount(0);

    // Server-side the session row survives — archived, not deleted.
    const updated = await weaver.getSession(s.id);
    expect(updated.status).toBe('archived');
  });

  test('lifecycle actions stay on-screen in a short window', async ({ page, weaver }) => {
    // Regression: the details popover used to grow past the bottom of the page
    // in a short window, clipping Archive/Remove out of reach. The popover now
    // caps its height to the viewport and scrolls the metadata instead, keeping
    // the actions pinned and clickable.
    const s = await weaver.seedSession({ goal: 'Stay reachable', name: 'short-window-task' });

    await page.setViewportSize({ width: 1280, height: 300 });
    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole('button', { name: 'details' }).click();

    for (const name of ['Archive', 'Remove']) {
      const box = await page.getByRole('button', { name }).boundingBox();
      expect(box, `${name} button should render`).not.toBeNull();
      expect(box!.y).toBeGreaterThanOrEqual(0);
      expect(box!.y + box!.height).toBeLessThanOrEqual(300);
    }

    // And Remove genuinely works from here.
    page.once('dialog', (dialog) => dialog.accept());
    await page.getByRole('button', { name: 'Remove' }).click();
    await expect(page).toHaveURL(/\/$/);
    expect(await weaver.listSessions()).toHaveLength(0);
  });
});
