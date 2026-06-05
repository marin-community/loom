import { test, expect } from '../fixtures/weaver';

test.describe('creating a session via the UI form', () => {
  test('opens the form, submits, and the session appears in the list', async ({
    page,
    weaver,
  }) => {
    await page.goto(weaver.baseUrl);

    // Form is hidden until "New session" is clicked.
    await expect(page.getByPlaceholder('/home/you/code/project')).toBeHidden();
    await page.getByRole('button', { name: 'New session' }).click();

    const repoInput = page.getByPlaceholder('/home/you/code/project');
    const goalInput = page.getByPlaceholder('Add a /health endpoint');
    await expect(repoInput).toBeVisible();

    await repoInput.fill(weaver.repoPath);
    await goalInput.fill('Implement the new feature');
    await page.getByRole('button', { name: 'Create' }).click();

    // The list reloads after creation; the new card should show up.
    const card = page.getByTestId('session-card');
    await expect(card).toHaveCount(1);
    await expect(card.first()).toContainText('Implement the new feature');

    // It was created with the shell agent (settings default) and persisted server-side.
    const all = await weaver.listSessions();
    expect(all).toHaveLength(1);
    expect(all[0].branch.goal).toBe('Implement the new feature');
    expect(all[0].agent_kind).toBe('shell');
  });

  test('the repository field offers recently-used repos', async ({ page, weaver }) => {
    // Seed a session so its repo is recorded as recently used.
    const s = await weaver.seedSession({ goal: 'seed', name: 'seed-ws' });

    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New session' }).click();

    const repoInput = page.getByPlaceholder('/home/you/code/project');
    // The dropdown stays hidden until the repository field is focused.
    await expect(page.getByTestId('recent-repo')).toBeHidden();
    await repoInput.focus();

    const recent = page.getByTestId('recent-repo');
    await expect(recent).toHaveCount(1);
    await expect(recent.first()).toContainText(s.branch.repo_root);

    // Picking a recent repo fills the field and closes the dropdown.
    await recent.first().click();
    await expect(repoInput).toHaveValue(s.branch.repo_root);
    await expect(page.getByTestId('recent-repo')).toBeHidden();
  });

  test('attached scratch files land in the new worktree', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New session' }).click();

    await page.getByPlaceholder('/home/you/code/project').fill(weaver.repoPath);
    await page.getByPlaceholder('Add a /health endpoint').fill('Investigate the attached trace');

    // Drop two reference files via the (hidden) file input behind the dropper.
    const input = page.getByTestId('scratch-picker-dropzone').locator('input[type=file]');
    await input.setInputFiles([
      { name: 'trace.log', mimeType: 'text/plain', buffer: Buffer.from('panic at line 42\n') },
      { name: 'shot.png', mimeType: 'image/png', buffer: Buffer.from([0x89, 0x50, 0x4e, 0x47]) },
    ]);
    await expect(page.getByTestId('scratch-picker-file')).toHaveCount(2);

    await page.getByRole('button', { name: 'Create' }).click();
    await expect(page.getByTestId('session-card')).toHaveCount(1);

    // The server dropped them into the worktree's scratch/ dir, exposed via the
    // per-session scratch endpoint.
    const all = await weaver.listSessions();
    const res = await fetch(`${weaver.baseUrl}/api/sessions/${all[0].id}/scratch`);
    const files = (await res.json()) as { name: string }[];
    expect(files.map((f) => f.name).sort()).toEqual(['shot.png', 'trace.log']);
  });

  test('Cancel hides the form again', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New session' }).click();
    await expect(page.getByPlaceholder('Add a /health endpoint')).toBeVisible();
    await page.getByRole('button', { name: 'Cancel' }).click();
    await expect(page.getByPlaceholder('Add a /health endpoint')).toBeHidden();
  });
});
