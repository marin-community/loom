import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
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

  test('selects a profile, overrides it, launches, and drops an attachment', async ({
    page,
    weaver,
  }) => {
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

  test('a cached session routes drop and browse files only into the next launch', async ({
    page,
    weaver,
  }) => {
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
    await page.getByPlaceholder(repoPlaceholder).fill(weaver.repoPath);
    await page
      .getByPlaceholder('Add a /health endpoint')
      .fill('Verify cached route attachment bytes');
    const dropzone = page.getByTestId('scratch-picker-dropzone');
    const dataTransfer = await page.evaluateHandle(() => {
      const dt = new DataTransfer();
      dt.items.add(new File([new Uint8Array([0, 255, 1, 2])], 'empty-type.bin'));
      dt.items.add(
        new File(['plain text with a misleading MIME'], 'misleading.txt', {
          type: 'image/png',
        }),
      );
      Object.defineProperty(dt, 'types', { value: [] });
      return dt;
    });
    await dropzone.dispatchEvent('drop', { dataTransfer });
    await expect(page.getByTestId('scratch-picker-file')).toHaveCount(2);
    await dropzone.locator('input[type="file"]').setInputFiles({
      name: 'browsed.dat',
      mimeType: 'application/x-weaver-misleading',
      buffer: Buffer.from([9, 8, 7, 6]),
    });
    await expect(page.getByTestId('scratch-picker-file')).toHaveCount(3);
    expect(
      await page.evaluate(() => (window as Window & { __dropListeners?: number }).__dropListeners),
    ).toBe(0);

    const oldScratch = await fetch(`${weaver.baseUrl}/api/sessions/${existing.id}/scratch`);
    expect((await oldScratch.json()) as { name: string }[]).toEqual([]);

    await page.getByRole('button', { name: 'Create session' }).click();
    await expect(page).toHaveURL(/\/s\/[^/]+$/);
    const launched = (await weaver.listSessions()).find(
      (session) => session.branch.goal === 'Verify cached route attachment bytes',
    )!;
    const listed = (await (
      await fetch(`${weaver.baseUrl}/api/sessions/${launched.id}/scratch`)
    ).json()) as { name: string; bytes: number }[];
    expect(listed.map((file) => file.name).sort()).toEqual([
      'browsed.dat',
      'empty-type.bin',
      'misleading.txt',
    ]);
    expect(await readFile(join(launched.work_dir, 'scratch/empty-type.bin'))).toEqual(
      Buffer.from([0, 255, 1, 2]),
    );
    expect(await readFile(join(launched.work_dir, 'scratch/misleading.txt'), 'utf8')).toBe(
      'plain text with a misleading MIME',
    );
    expect(await readFile(join(launched.work_dir, 'scratch/browsed.dat'))).toEqual(
      Buffer.from([9, 8, 7, 6]),
    );
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

  test('invalidates the approved preview before resolving an edited selection', async ({
    page,
    weaver,
  }) => {
    await page.route('**/api/session-launches/resolve', async (route) => {
      const body = route.request().postDataJSON() as {
        selection?: { overrides?: { mode?: string } };
      };
      if (body.selection?.overrides?.mode) {
        await new Promise((resolve) => setTimeout(resolve, 500));
      }
      await route.continue();
    });
    await page.goto(`${weaver.baseUrl}/sessions/new`);
    await expect(page.getByTestId('provenance-mode')).toBeVisible();
    await page.getByTestId('clone-profile-open').click();
    await page
      .getByTestId('profile-editor')
      .getByLabel('Name', { exact: true })
      .fill('stale-preview-clone');
    await expect(page.getByTestId('clone-profile')).toBeEnabled();

    await page.getByRole('button', { name: /One-launch overrides/ }).click();
    await page.getByTestId('override-mode-toggle').check();

    await expect(page.getByTestId('create-session')).toBeDisabled();
    await expect(page.getByTestId('clone-profile')).toHaveCount(0);
    await expect(page.getByTestId('resolved-launch-summary')).toContainText('Resolving…');
    await expect(page.getByTestId('provenance-mode')).toHaveCount(0);

    await expect(page.getByTestId('provenance-mode')).toHaveText('launch override');
    await expect(page.getByTestId('clone-profile')).toBeEnabled();
  });

  test('refreshes added, edited, and deleted profiles without losing the cached draft', async ({
    page,
    weaver,
  }) => {
    await page.goto(`${weaver.baseUrl}/sessions/new`);
    await page.getByPlaceholder(repoPlaceholder).fill(weaver.repoPath);
    await page.getByPlaceholder('Add a /health endpoint').fill('Keep this draft');

    await page.getByRole('link', { name: 'Edit profile templates in Settings' }).click();
    await expect(page).toHaveURL(`${weaver.baseUrl}/settings`);
    await expect(page.getByTestId('profile-selector')).toBeVisible();
    await page.getByRole('button', { name: '+ Add profile' }).click();
    await page.getByLabel('Name', { exact: true }).fill('cached-template');
    await page.getByLabel('Description', { exact: true }).fill('first revision');
    await page.getByTestId('profile-agent').selectOption('shell');
    await page.getByLabel('Protocol', { exact: true }).selectOption('terminal');
    await page.getByTestId('profile-save').click();
    await expect(page.getByText('Saved cached-template.')).toBeVisible();
    await page.getByLabel('Description', { exact: true }).fill('second revision');
    await page.getByTestId('profile-save').click();
    await expect(page.getByTestId('profile-option-cached-template')).toContainText('r2');
    await page.goBack();

    await expect(page.getByPlaceholder(repoPlaceholder)).toHaveValue(weaver.repoPath);
    await expect(page.getByPlaceholder('Add a /health endpoint')).toHaveValue('Keep this draft');
    await expect(page.getByTestId('profile-option-cached-template')).toContainText('r2');
    await page.getByTestId('profile-option-cached-template').click();
    await page.getByRole('button', { name: /One-launch overrides/ }).click();
    await page.getByTestId('override-mode-toggle').check();
    await page.getByTestId('override-mode').selectOption('plan');
    const staged = await page.evaluateHandle(() => {
      const transfer = new DataTransfer();
      transfer.items.add(new File(['preserved'], 'preserved.txt'));
      return transfer;
    });
    await page.getByTestId('scratch-picker-dropzone').dispatchEvent('drop', {
      dataTransfer: staged,
    });

    await page.getByRole('link', { name: 'Edit profile templates in Settings' }).click();
    await page.getByTestId('profile-option-cached-template').click();
    const profileDelete = page.getByTestId('profile-delete');
    await profileDelete.getByRole('button', { name: 'Delete' }).click();
    await profileDelete.getByRole('button', { name: 'Confirm' }).click();
    await expect(page.getByTestId('profile-option-cached-template')).toHaveCount(0);
    await page.goBack();

    await expect(page.getByTestId('profile-option-cached-template')).toHaveCount(0);
    await expect(page.getByPlaceholder(repoPlaceholder)).toHaveValue(weaver.repoPath);
    await expect(page.getByPlaceholder('Add a /health endpoint')).toHaveValue('Keep this draft');
    await expect(page.getByTestId('override-mode-toggle')).toBeChecked();
    await expect(page.getByTestId('override-mode')).toHaveValue('plan');
    await expect(page.getByTestId('scratch-picker-file')).toContainText('preserved.txt');
  });

  test('the focused launch surface does not overflow a 320px viewport', async ({
    page,
    weaver,
  }) => {
    await page.setViewportSize({ width: 320, height: 720 });
    await page.goto(`${weaver.baseUrl}/sessions/new`);
    await expect(page.getByTestId('new-session-drawer')).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(() => ({
          client: document.documentElement.clientWidth,
          scroll: document.documentElement.scrollWidth,
        })),
      )
      .toEqual({ client: 320, scroll: 320 });
  });

  test('Cancel discards the cached launch draft', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New session' }).click();
    await page.getByPlaceholder(repoPlaceholder).fill(weaver.repoPath);
    await page.getByPlaceholder('Add a /health endpoint').fill('Discard this task');
    const dataTransfer = await page.evaluateHandle(() => {
      const transfer = new DataTransfer();
      transfer.items.add(new File(['discard'], 'discard.txt', { type: 'text/plain' }));
      return transfer;
    });
    await page.getByTestId('scratch-picker-dropzone').dispatchEvent('drop', { dataTransfer });
    await expect(page.getByTestId('scratch-picker-file')).toContainText('discard.txt');

    await page.locator('form').getByRole('button', { name: 'Cancel' }).click();
    await expect(page).toHaveURL(`${weaver.baseUrl}/`);
    await page.getByRole('button', { name: 'New session' }).click();
    await expect(page.getByPlaceholder(repoPlaceholder)).toHaveValue('');
    await expect(page.getByPlaceholder('Add a /health endpoint')).toHaveValue('');
    await expect(page.getByTestId('scratch-picker-file')).toHaveCount(0);
  });
});
