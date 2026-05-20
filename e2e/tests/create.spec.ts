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

  test('the repository field offers recently-used repos', async ({ page, weaver }) => {
    // Seed a workspace so its repo is recorded as recently used.
    const ws = await weaver.seedWorkspace({ goal: 'seed', name: 'seed-ws' });

    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New workspace' }).click();

    const repoInput = page.getByPlaceholder('/home/you/code/project');
    // The dropdown stays hidden until the repository field is focused.
    await expect(page.getByTestId('recent-repo')).toBeHidden();
    await repoInput.focus();

    const recent = page.getByTestId('recent-repo');
    await expect(recent).toHaveCount(1);
    await expect(recent.first()).toContainText(ws.repo_root);

    // Picking a recent repo fills the field and closes the dropdown.
    await recent.first().click();
    await expect(repoInput).toHaveValue(ws.repo_root);
    await expect(page.getByTestId('recent-repo')).toBeHidden();
  });

  test('Cancel hides the form again', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New workspace' }).click();
    await expect(page.getByPlaceholder('Add a /health endpoint')).toBeVisible();
    await page.getByRole('button', { name: 'Cancel' }).click();
    await expect(page.getByPlaceholder('Add a /health endpoint')).toBeHidden();
  });
});
