import { test, expect } from '../fixtures/weaver';

test.describe('creating a session via the UI form', () => {
  const repoPlaceholder = 'owner/name or /home/you/code/project';

  test('opens the form, submits, and the session appears in the list', async ({
    page,
    weaver,
  }) => {
    await page.goto(weaver.baseUrl);

    // Form is hidden until "New session" is clicked.
    await expect(page.getByPlaceholder(repoPlaceholder)).toBeHidden();
    await page.getByRole('button', { name: 'New session' }).click();

    const repoInput = page.getByPlaceholder(repoPlaceholder);
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

    const repoInput = page.getByPlaceholder(repoPlaceholder);
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

    await page.getByPlaceholder(repoPlaceholder).fill(weaver.repoPath);
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

  test('agent selection drives model and effort choices', async ({ page, weaver }) => {
    const registry = (await (await fetch(`${weaver.baseUrl}/api/agents`)).json()) as {
      agents: { kind: string; models: { label: string }[]; efforts: { label: string }[] }[];
    };
    const codex = registry.agents.find((agent) => agent.kind === 'codex')!;
    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New session' }).click();

    const form = page.locator('form');
    await expect(form.getByRole('radio', { name: /Claude/ })).toBeVisible();
    await expect(form.getByRole('radio', { name: /Codex/ })).toBeVisible();
    await expect(form.getByRole('radio', { name: /Shell/ })).toBeVisible();

    await form.getByRole('radio', { name: /Claude/ }).click();
    await expect(form.getByRole('button', { name: 'Default' }).first()).toBeVisible();
    await expect(form.getByRole('button', { name: 'Haiku' })).toBeVisible();
    await expect(form.getByRole('button', { name: 'Sonnet' })).toBeVisible();
    await expect(form.getByRole('button', { name: 'Opus' })).toBeVisible();
    await expect(form.getByRole('button', { name: 'Fable' })).toBeVisible();
    await expect(form.getByRole('button', { name: 'Max' })).toBeVisible();

    await form.getByRole('radio', { name: /Codex/ }).click();
    for (const model of codex.models) {
      await expect(form.getByRole('button', { name: model.label, exact: true })).toBeVisible();
    }
    for (const effort of codex.efforts) {
      await expect(form.getByRole('button', { name: effort.label, exact: true })).toBeVisible();
    }
    await expect(form.getByRole('button', { name: 'Haiku' })).toHaveCount(0);
    await expect(form.getByRole('button', { name: 'Sonnet' })).toHaveCount(0);
    await expect(form.getByRole('button', { name: 'Opus' })).toHaveCount(0);
    await expect(form.getByRole('button', { name: 'Fable' })).toHaveCount(0);
  });

  test('Cancel hides the form again', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New session' }).click();
    await expect(page.getByPlaceholder('Add a /health endpoint')).toBeVisible();
    // There are now two "Cancel" buttons — the top toggle and the form's own
    // bottom action. Scope to the form so this targets the bottom one uniquely.
    await page.locator('form').getByRole('button', { name: 'Cancel' }).click();
    await expect(page.getByPlaceholder('Add a /health endpoint')).toBeHidden();
  });
});
