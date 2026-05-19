import { test, expect } from '../fixtures/weaver';

test.describe('removing a workspace', () => {
  test('Remove (confirmed) deletes the workspace and returns to the list', async ({
    page,
    weaver,
  }) => {
    const ws = await weaver.seedWorkspace({ goal: 'Delete me', name: 'remove-task' });

    await page.goto(`${weaver.baseUrl}/#/w/${ws.id}`);
    await expect(page.getByRole('heading', { name: 'remove-task' })).toBeVisible();

    // Remove uses a native confirm() dialog — accept it.
    page.once('dialog', (dialog) => {
      expect(dialog.type()).toBe('confirm');
      dialog.accept();
    });
    await page.getByRole('button', { name: 'Remove' }).click();

    // Router pushes back to the list.
    await expect(page).toHaveURL(/#\/$/);
    await expect(page.getByRole('heading', { name: 'Workspaces' })).toBeVisible();
    await expect(page.getByText('No workspaces yet.')).toBeVisible();

    // And it is gone server-side.
    const all = await weaver.listWorkspaces();
    expect(all).toHaveLength(0);
  });

  test('dismissing the confirm dialog keeps the workspace', async ({ page, weaver }) => {
    const ws = await weaver.seedWorkspace({ goal: 'Keep me', name: 'keep-task' });

    await page.goto(`${weaver.baseUrl}/#/w/${ws.id}`);

    page.once('dialog', (dialog) => dialog.dismiss());
    await page.getByRole('button', { name: 'Remove' }).click();

    // Still on the detail page, still present server-side.
    await expect(page).toHaveURL(new RegExp(`#/w/${ws.id}$`));
    const all = await weaver.listWorkspaces();
    expect(all).toHaveLength(1);
  });
});
