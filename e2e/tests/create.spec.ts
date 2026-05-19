import { test, expect } from '../fixtures/weaver';

test.describe('creating a workspace via the UI form', () => {
  test('opens the form, submits, and the workspace appears in the list', async ({
    page,
    weaver,
  }) => {
    await page.goto(weaver.baseUrl);

    // Form is hidden until "New workspace" is clicked.
    await expect(page.getByPlaceholder('/home/you/code/project')).toBeHidden();
    await page.getByRole('button', { name: 'New workspace' }).click();

    const repoInput = page.getByPlaceholder('/home/you/code/project');
    const goalInput = page.getByPlaceholder('Add a /health endpoint');
    await expect(repoInput).toBeVisible();

    await repoInput.fill(weaver.repoPath);
    await goalInput.fill('Implement the new feature');
    await page.getByRole('button', { name: 'Create' }).click();

    // The list reloads after creation; the new card should show up.
    const card = page.getByTestId('workspace-card');
    await expect(card).toHaveCount(1);
    await expect(card.first()).toContainText('Implement the new feature');

    // It was created with the shell agent (settings default) and persisted server-side.
    const all = await weaver.listWorkspaces();
    expect(all).toHaveLength(1);
    expect(all[0].goal).toBe('Implement the new feature');
    expect(all[0].agent_kind).toBe('shell');
  });

  test('Cancel hides the form again', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New workspace' }).click();
    await expect(page.getByPlaceholder('Add a /health endpoint')).toBeVisible();
    await page.getByRole('button', { name: 'Cancel' }).click();
    await expect(page.getByPlaceholder('Add a /health endpoint')).toBeHidden();
  });
});
