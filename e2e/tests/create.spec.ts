import { test, expect } from '../fixtures/weaver';

test.describe('creating a session via the UI form', () => {
  const repoPlaceholder = 'owner/name or /home/you/code/project';

  test('keeps arrow navigation inside the repository picker', async ({ page, weaver }) => {
    await weaver.seedSession({
      goal: 'make this repository recent',
      name: 'recent-repository',
    });
    await page.goto(`${weaver.baseUrl}/sessions/new`);

    const repo = page.getByPlaceholder(repoPlaceholder);
    await repo.focus();
    const option = page.getByTestId('recent-repo').filter({ hasText: weaver.repoPath });
    await expect(option).toBeVisible();

    await repo.press('ArrowDown');
    await expect(option).toHaveAttribute('aria-selected', 'true');
    await expect(option).toHaveClass(/bg-subtle/);
    await expect(page).toHaveURL(`${weaver.baseUrl}/sessions/new`);

    await repo.press('Enter');
    await expect(repo).toHaveValue(weaver.repoPath);
    await expect(page.getByTestId('recent-repos')).toBeHidden();
  });

  test('launches from the workbench with canonical profile and title', async ({ page, weaver }) => {
    await fetch(`${weaver.baseUrl}/api/profiles`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        name: 'ui-launch',
        description: 'Playwright launch template',
        agent_kind: 'shell',
        model: '',
        effort: '',
        protocol: 'terminal',
        mode: 'auto',
        class: 'interactive',
        strict: false,
        env_clear: false,
        ambient_allowlist: [],
        max_concurrent: 0,
        prelude: 'weaver',
        restricted: false,
        runtime_permissions: [],
        mcp_access: { mode: 'none', groups: [] },
      }),
    });
    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New session' }).click();

    const repo = page.getByPlaceholder(repoPlaceholder);
    const title = page.getByLabel('Title', { exact: true });
    const goal = page.getByPlaceholder('Add a /health endpoint');
    await expect(repo).toHaveAttribute('autocomplete', 'off');
    await expect(title).toHaveAttribute('autocomplete', 'off');
    await expect(goal).toHaveAttribute('autocomplete', 'off');

    await repo.fill(weaver.repoPath);
    await goal.fill('Investigate the attached trace');
    await page.getByTestId('launch-profile-picker').selectOption('ui-launch');
    await page.getByTestId('override-mode').selectOption('plan');
    await expect(page.getByTestId('launch-settings')).toContainText('changed');
    await page.getByTestId('clone-profile-open').click();
    await page.getByTestId('clone-profile-name').fill('ui-launch-plan');
    await page.getByTestId('clone-profile').click();
    await expect(page.getByTestId('launch-profile-picker')).toHaveValue('ui-launch-plan');

    // Drop is accepted from files.length even when DataTransfer.types is empty.
    const dataTransfer = await page.evaluateHandle(() => {
      const transfer = new DataTransfer();
      transfer.items.add(new File(['panic at line 42\n'], 'trace.log', { type: 'text/plain' }));
      Object.defineProperty(transfer, 'types', { value: [] });
      return transfer;
    });
    await page.getByTestId('scratch-picker-dropzone').dispatchEvent('drop', { dataTransfer });
    await expect(page.getByTestId('scratch-picker-file')).toContainText('trace.log');

    await expect(page.getByTestId('create-session')).toBeEnabled();
    await page.getByPlaceholder('Add a /health endpoint').press('Control+Enter');
    await expect(page).toHaveURL(/\/s\/[^/]+$/);

    const all = await weaver.listSessions();
    const session = all[0];
    expect(session.profile).toBe('ui-launch-plan');
    expect(session.launch_mode).toBe('plan');
    expect(session.resolved_launch?.provenance.mode).toBe('profile');
    expect(session.branch.title).toBe('Investigate the attached trace');
    expect(session.branch.title_provenance).toBe('derived');
    const res = await fetch(`${weaver.baseUrl}/api/sessions/${session.id}/scratch`);
    const files = (await res.json()) as { name: string; bytes: number }[];
    expect(files).toEqual([{ name: 'trace.log', bytes: Buffer.byteLength('panic at line 42\n') }]);

    await page.locator('[data-rail="sessions"]').click();
    const card = page.getByTestId('session-card');
    await expect(card).toContainText('Investigate the attached trace');
    await expect(card).not.toContainText('Inbox / Investigate the attached trace');
    await card.click();
    await page.getByRole('button', { name: /Details/ }).click();
    await page.getByText('Advanced', { exact: true }).click();
    await expect(page.getByTestId('action-open-editor')).toBeVisible();
  });

  test('refreshes added, edited, and deleted profiles without losing the cached draft', async ({
    page,
    weaver,
  }) => {
    await page.goto(`${weaver.baseUrl}/sessions/new`);
    await page.getByPlaceholder(repoPlaceholder).fill(weaver.repoPath);
    await page.getByPlaceholder('Add a /health endpoint').fill('Keep this draft');

    await page.getByRole('link', { name: 'Manage profiles' }).click();
    await expect(page).toHaveURL(`${weaver.baseUrl}/settings`);
    const addProfile = page.getByRole('button', { name: '+ Add profile' });
    await expect(page.getByTestId('profile-picker')).toHaveValue('default');
    await addProfile.click();
    await page.getByLabel('Name', { exact: true }).fill('cached-template');
    await page.getByLabel('Description', { exact: true }).fill('first revision');
    await page.getByTestId('profile-agent').selectOption('shell');
    await page.getByLabel('Protocol', { exact: true }).selectOption('terminal');
    await page.getByTestId('profile-save').click();
    await expect(page.getByText('Saved cached-template.')).toBeVisible();
    await page.getByLabel('Description', { exact: true }).fill('second revision');
    await page.getByTestId('profile-save').click();
    await expect(page.getByTestId('profile-summary')).toContainText('r2');
    await page.goBack();

    await expect(page.getByPlaceholder(repoPlaceholder)).toHaveValue(weaver.repoPath);
    await expect(page.getByPlaceholder('Add a /health endpoint')).toHaveValue('Keep this draft');
    await expect(
      page.getByTestId('launch-profile-picker').locator('option[value="cached-template"]'),
    ).toHaveCount(1);
    await page.getByTestId('launch-profile-picker').selectOption('cached-template');
    await page.getByTestId('override-mode').selectOption('plan');
    const staged = await page.evaluateHandle(() => {
      const transfer = new DataTransfer();
      transfer.items.add(new File(['preserved'], 'preserved.txt'));
      return transfer;
    });
    await page.getByTestId('scratch-picker-dropzone').dispatchEvent('drop', {
      dataTransfer: staged,
    });

    await page.getByRole('link', { name: 'Manage profiles' }).click();
    await page.getByTestId('profile-picker').selectOption('cached-template');
    const profileDelete = page.getByTestId('profile-delete');
    await profileDelete.getByRole('button', { name: 'Delete' }).click();
    await expect(profileDelete.getByRole('button', { name: 'Confirm' })).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(profileDelete.getByRole('button', { name: 'Delete' })).toBeFocused();
    await profileDelete.getByRole('button', { name: 'Delete' }).click();
    await profileDelete.getByRole('button', { name: 'Cancel' }).click();
    await expect(profileDelete.getByRole('button', { name: 'Delete' })).toBeFocused();
    await profileDelete.getByRole('button', { name: 'Delete' }).click();
    await profileDelete.getByRole('button', { name: 'Confirm' }).click();
    await expect(
      page.getByTestId('profile-picker').locator('option[value="cached-template"]'),
    ).toHaveCount(0);
    await page.goBack();

    await expect(
      page.getByTestId('launch-profile-picker').locator('option[value="cached-template"]'),
    ).toHaveCount(0);
    await expect(page.getByPlaceholder(repoPlaceholder)).toHaveValue(weaver.repoPath);
    await expect(page.getByPlaceholder('Add a /health endpoint')).toHaveValue('Keep this draft');
    await expect(page.getByTestId('override-mode')).toHaveValue('plan');
    await expect(page.getByTestId('scratch-picker-file')).toContainText('preserved.txt');
  });
});
