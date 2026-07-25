import { test, expect } from '../fixtures/weaver';

test.describe('creating a session via the UI form', () => {
  const repoPlaceholder = 'owner/name or /home/you/code/project';

  test('opens the form, submits, and the session appears in the list', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);

    // Form is hidden until "New session" is clicked.
    await expect(page.getByPlaceholder(repoPlaceholder)).toBeHidden();
    await page.getByRole('button', { name: 'New session' }).click();

    const repoInput = page.getByPlaceholder(repoPlaceholder);
    const goalInput = page.getByPlaceholder('Add a /health endpoint');
    await expect(repoInput).toBeVisible();

    await repoInput.fill(weaver.repoPath);
    await goalInput.fill('Implement the new feature');
    await expect(page).toHaveURL(`${weaver.baseUrl}/sessions/new`);
    await page.getByRole('button', { name: 'Create session' }).click();

    // A successful focused launch opens the new session. Returning to the fleet
    // shows the same persisted row.
    await expect(page).toHaveURL(/\/s\/[^/]+$/);
    await page.locator('[data-rail="sessions"]').click();
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

  test('selects a profile, overrides it, launches, and drops an attachment', async ({ page, weaver }) => {
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

    await page.getByPlaceholder(repoPlaceholder).fill(weaver.repoPath);
    await page.getByPlaceholder('Add a /health endpoint').fill('Investigate the attached trace');
    await page.getByTestId('profile-option-ui-launch').click();
    await page.getByRole('button', { name: /One-launch overrides/ }).click();
    await page.getByTestId('override-mode-toggle').check();
    await page.getByTestId('override-mode').selectOption('plan');
    await expect(page.getByTestId('provenance-mode')).toHaveText('launch override');

    // Drop is accepted from files.length even when DataTransfer.types is empty.
    const dataTransfer = await page.evaluateHandle(() => {
      const transfer = new DataTransfer();
      transfer.items.add(new File(['panic at line 42\n'], 'trace.log', { type: 'text/plain' }));
      Object.defineProperty(transfer, 'types', { value: [] });
      return transfer;
    });
    await page.getByTestId('scratch-picker-dropzone').dispatchEvent('drop', { dataTransfer });
    await expect(page.getByTestId('scratch-picker-file')).toContainText('trace.log');

    await page.getByRole('button', { name: 'Create session' }).click();
    await expect(page).toHaveURL(/\/s\/[^/]+$/);

    const all = await weaver.listSessions();
    expect(all[0].profile).toBe('ui-launch');
    expect(all[0].launch_mode).toBe('plan');
    expect(all[0].resolved_launch?.provenance.mode).toBe('launch_override');
    const res = await fetch(`${weaver.baseUrl}/api/sessions/${all[0].id}/scratch`);
    const files = (await res.json()) as { name: string; bytes: number }[];
    expect(files).toEqual([{ name: 'trace.log', bytes: Buffer.byteLength('panic at line 42\n') }]);
  });

  test('a cached session does not consume new-session file drops', async ({ page, weaver }) => {
    await page.addInitScript(() => {
      const original = window.addEventListener.bind(window);
      (window as Window & { __dropListeners?: number }).__dropListeners = 0;
      window.addEventListener = ((
        type: string,
        listener: EventListenerOrEventListenerObject,
        options?: boolean | AddEventListenerOptions,
      ) => {
        if (type === 'drop') {
          (window as Window & { __dropListeners?: number }).__dropListeners! += 1;
        }
        return original(type, listener, options);
      }) as typeof window.addEventListener;
    });
    const existing = await weaver.seedSession({
      goal: 'Keep this session warm',
      name: 'warm',
    });
    await page.goto(`${weaver.baseUrl}/s/${existing.id}`);
    await expect(page.getByTestId('scratch-panel')).toBeVisible();

    // SessionDetail remains cached, but both attachment surfaces are bounded.
    await page.locator('[data-rail="sessions"]').click();
    await page.getByRole('button', { name: 'New session' }).click();
    const dropzone = page.getByTestId('scratch-picker-dropzone');
    const dataTransfer = await page.evaluateHandle(() => {
      const dt = new DataTransfer();
      dt.items.add(new File(['image'], 'new-session.png', { type: 'image/png' }));
      return dt;
    });
    await dropzone.dispatchEvent('drop', { dataTransfer });
    await expect(page.getByTestId('scratch-picker-file')).toContainText('new-session.png');
    expect(await page.evaluate(() => (window as Window & { __dropListeners?: number }).__dropListeners)).toBe(0);

    const oldScratch = await fetch(`${weaver.baseUrl}/api/sessions/${existing.id}/scratch`);
    expect((await oldScratch.json()) as { name: string }[]).toEqual([]);
  });

  test('an agent startup failure opens its recoverable session', async ({ page, weaver }) => {
    const failed = await weaver.seedSession({
      goal: 'Recover me',
      name: 'failed-create',
    });
    await page.route('**/api/sessions', async (route) => {
      if (route.request().method() !== 'POST') return route.continue();
      await route.fulfill({
        status: 502,
        contentType: 'application/json',
        body: JSON.stringify({
          error: 'acp launch failed: agent did not respond',
          session_id: failed.id,
        }),
      });
    });
    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New session' }).click();
    await page.getByPlaceholder(repoPlaceholder).fill(weaver.repoPath);
    await page.getByPlaceholder('Add a /health endpoint').fill('Start a limited agent');
    await page.getByRole('button', { name: 'Create session', exact: true }).click();

    await expect(page).toHaveURL(new RegExp(`/s/${failed.id}$`));
    await expect(page.getByTestId('new-session-drawer')).toHaveCount(0);
  });

  test('launch selectors show profile and default provenance', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New session' }).click();

    await expect(page.getByTestId('profile-selector')).toBeVisible();
    await expect(page.getByTestId('provenance-agent')).toHaveText('profile');
    await expect(page.getByTestId('provenance-model')).toHaveText(/profile|agent default/);
  });

  test('keeps the launch draft while profile templates are edited', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/sessions/new`);
    await page.getByPlaceholder(repoPlaceholder).fill(weaver.repoPath);
    await page.getByPlaceholder('Add a /health endpoint').fill('Keep this draft');

    await page.getByRole('link', { name: 'Edit profile templates in Settings' }).click();
    await expect(page).toHaveURL(`${weaver.baseUrl}/settings`);
    await expect(page.getByTestId('profile-selector')).toBeVisible();
    await page.goBack();

    await expect(page.getByPlaceholder(repoPlaceholder)).toHaveValue(weaver.repoPath);
    await expect(page.getByPlaceholder('Add a /health endpoint')).toHaveValue('Keep this draft');
  });

  test('Cancel hides the form again', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New session' }).click();
    await expect(page.getByPlaceholder('Add a /health endpoint')).toBeVisible();
    await page.locator('form').getByRole('button', { name: 'Cancel' }).click();
    await expect(page).toHaveURL(`${weaver.baseUrl}/`);
  });
});
