import { test, expect } from '../fixtures/weaver';

// The lifecycle actions (Adopt / Recover / Archive / Remove) are reachable from
// two places: the detail header's ⋯ manage menu, and each fleet-list row's ⋯
// menu. A stuck session (orphaned/archived) also carries its remedy as a plain
// button next to the status badge, on both surfaces.
test.describe('session lifecycle actions', () => {
  test('Remove (confirmed) deletes the session and returns to the list', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Delete me', name: 'remove-task' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await expect(page.getByRole('heading', { name: 'remove-task' })).toBeVisible();

    await page.getByRole('button', { name: 'manage' }).click();

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
    await page.getByRole('button', { name: 'manage' }).click();

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
    await page.getByRole('button', { name: 'manage' }).click();

    page.once('dialog', (dialog) => {
      expect(dialog.type()).toBe('confirm');
      dialog.accept();
    });
    await page.getByTestId('action-archive').click();

    // The header reloads into the archived state: the lifecycle badge appears
    // and the popover's Archive button goes away (archiving twice is a no-op).
    await expect(page.getByTestId('status-badge')).toHaveText(/archived/i);
    await expect(page.getByTestId('action-archive')).toHaveCount(0);

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
    await page.getByRole('button', { name: 'manage' }).click();

    for (const [name, id] of [
      ['Archive', 'action-archive'],
      ['Remove', 'action-remove'],
    ]) {
      const box = await page.getByTestId(id).boundingBox();
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

  test('a fleet-list row can archive its session without opening it', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Archive from the list', name: 'row-archive' });

    await page.goto(`${weaver.baseUrl}/`);
    const row = page.locator(`[data-session-id="${s.id}"]`);
    await expect(row).toBeVisible();

    // The ⋯ menu is revealed by hovering the row, and holds the verbs.
    await row.hover();
    await row.getByTestId('row-actions').click();
    await expect(row.getByTestId('row-actions-menu')).toBeVisible();

    page.once('dialog', (dialog) => {
      expect(dialog.type()).toBe('confirm');
      dialog.accept();
    });
    await row.getByTestId('row-action-archive').click();

    // Archived server-side — and we never left the list.
    await expect.poll(async () => (await weaver.getSession(s.id)).status).toBe('archived');
    await expect(page).toHaveURL(/\/$/);
  });

  test('a session can opt out of automatic archive from its manage menu', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Keep me live', name: 'no-auto-archive' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole('button', { name: 'manage' }).click();
    await page.getByTestId('action-auto-archive').click();

    await expect(page.getByTestId('tag-pill')).toContainText('auto-archive: disabled');
    await expect
      .poll(async () => (await weaver.getSession(s.id)).branch.tags)
      .toContainEqual(expect.objectContaining({ key: 'auto-archive', value: 'disabled' }));

    await expect(page.getByTestId('action-auto-archive')).toContainText('Enable auto-archive');
    await page.getByTestId('action-auto-archive').click();

    await expect(page.getByTestId('tag-pill')).toHaveCount(0);
    await expect
      .poll(async () => (await weaver.getSession(s.id)).branch.tags)
      .not.toContainEqual(expect.objectContaining({ key: 'auto-archive' }));
  });

  test('an archived session offers Recover next to its badge, on both surfaces', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Recover me', name: 'recover-task' });
    await weaver.archiveSession(s.id);

    // On the fleet list (archived rows are behind the reveal chip), the row
    // carries its own remedy — no need to open the session to find it. It is the
    // same component the detail header renders, hence the same test id.
    await page.goto(`${weaver.baseUrl}/`);
    await page.getByRole('button', { name: /archived/ }).click();
    const row = page.locator(`[data-session-id="${s.id}"]`);
    await expect(row.getByTestId('remedy-recover')).toBeVisible();

    // And on the detail page, sitting against the ARCHIVED badge.
    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await expect(page.getByTestId('status-badge')).toHaveText(/archived/i);
    await expect(page.getByTestId('remedy-recover')).toBeVisible();
  });
});
