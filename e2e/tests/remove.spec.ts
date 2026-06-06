import { test, expect } from '../fixtures/weaver';

test.describe('removing a session', () => {
  test('Remove (confirmed) deletes the session and returns to the list', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Delete me', name: 'remove-task' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await expect(page.getByRole('heading', { name: 'remove-task' })).toBeVisible();

    // Lifecycle actions live on the Overview tab, off the terminal-first default.
    await page.getByRole('button', { name: 'Overview' }).click();

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
    await page.getByRole('button', { name: 'Overview' }).click();

    page.once('dialog', (dialog) => dialog.dismiss());
    await page.getByRole('button', { name: 'Remove' }).click();

    // Still on the detail page, still present server-side.
    await expect(page).toHaveURL(new RegExp(`/s/${s.id}$`));
    const all = await weaver.listSessions();
    expect(all).toHaveLength(1);
  });
});
