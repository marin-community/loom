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

  test('directly submitted remote repos are registered before session creation', async ({
    page,
    weaver,
  }) => {
    const destination = await weaver.seedSession({
      goal: 'Mock destination',
      name: 'remote-registration-destination',
    });
    const calls: string[] = [];
    await page.route('**/api/repos', async (route) => {
      if (route.request().method() !== 'POST') return route.continue();
      calls.push('register');
      expect(route.request().postDataJSON()).toEqual({
        repo: 'octo/direct-submit',
      });
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          slug: 'octo/direct-submit',
          remote_url: 'https://github.com/octo/direct-submit.git',
          path: '/managed/octo/direct-submit',
          created_at: '2026-07-25T00:00:00Z',
        }),
      });
    });
    await page.route('**/api/sessions', async (route) => {
      if (route.request().method() !== 'POST') return route.continue();
      calls.push('create');
      expect(route.request().postDataJSON().repo).toBe('octo/direct-submit');
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify(destination),
      });
    });

    await page.goto(`${weaver.baseUrl}/sessions/new`);
    await page.getByPlaceholder(repoPlaceholder).fill('octo/direct-submit');
    await page.getByPlaceholder('Add a /health endpoint').fill('Launch directly from typed slug');
    // Submit without selecting the transient “Clone new repo” suggestion.
    await page.getByRole('button', { name: 'Create session' }).click();

    await expect(page).toHaveURL(new RegExp(`/s/${destination.id}$`));
    expect(calls).toEqual(['register', 'create']);
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
    await expect(page.getByTestId('clone-profile')).toHaveCount(0);
    await expect(page.getByTestId('clone-profile-open')).toBeEnabled();
  });

  test('composes write-only environment while saving a new profile', async ({ page, weaver }) => {
    let cloneProposal: Record<string, unknown> | undefined;
    page.on('request', (request) => {
      if (
        request.method() === 'POST' &&
        request.url().includes('/profiles/ui-environment-source/clone')
      ) {
        cloneProposal = request.postDataJSON() as Record<string, unknown>;
      }
    });
    const source = {
      name: 'ui-environment-source',
      description: 'Environment composition source',
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
    };
    await fetch(`${weaver.baseUrl}/api/profiles`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(source),
    });
    for (const [name, value] of [
      ['KEEP', 'kept'],
      ['REMOVE_ME', 'removed'],
    ]) {
      await fetch(`${weaver.baseUrl}/api/profiles/${source.name}/env/${name}`, {
        method: 'PUT',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ value }),
      });
    }

    await page.goto(`${weaver.baseUrl}/sessions/new`);
    await page.getByTestId(`profile-option-${source.name}`).click();
    await page.getByTestId('clone-profile-open').click();
    const editor = page.getByTestId('profile-editor');
    await editor.getByLabel('Name', { exact: true }).fill('ui-environment-target');
    const environment = page.getByTestId('profile-environment-editor');
    await environment.getByLabel(/REMOVE_ME/).uncheck();
    await environment.getByLabel('Environment name').fill('ADDED');
    await environment.getByLabel('Environment value', { exact: true }).fill('new write-only value');
    await environment.getByRole('button', { name: 'Set' }).click();
    await page.getByTestId('clone-profile').click();
    await expect(page.getByTestId('profile-option-ui-environment-target')).toBeVisible();
    expect(cloneProposal?.environment).toEqual({
      inherit: true,
      remove: ['REMOVE_ME'],
      set: [{ name: 'ADDED', value: 'new write-only value' }],
    });

    const saved = (await (
      await fetch(`${weaver.baseUrl}/api/profiles/ui-environment-target`)
    ).json()) as { env: { name: string; source: string }[] };
    expect(saved.env.map((entry) => entry.name)).toEqual(['ADDED', 'KEEP']);
    expect(JSON.stringify(saved)).not.toContain('new write-only value');
  });

  test('ignores a branch response after the repository is cleared', async ({ page, weaver }) => {
    let release!: () => void;
    const delayed = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route('**/api/repos/branches?cwd=*', async (route) => {
      await delayed;
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify([{ name: 'stale-branch', current: false, worktree: null }]),
      });
    });
    await page.goto(`${weaver.baseUrl}/sessions/new`);
    await page.getByPlaceholder(repoPlaceholder).fill(weaver.repoPath);
    await page.getByText('Advanced branch controls', { exact: true }).click();
    await page.getByRole('button', { name: 'Existing branch' }).click();
    await page.getByPlaceholder(repoPlaceholder).fill('');
    release();
    await page.getByLabel(/Existing branch - weaver reuses/).focus();
    await expect(page.getByTestId('branch-option')).toHaveCount(0);
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
    await expect(profileDelete.getByRole('button', { name: 'Confirm' })).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(profileDelete.getByRole('button', { name: 'Delete' })).toBeFocused();
    await profileDelete.getByRole('button', { name: 'Delete' }).click();
    await profileDelete.getByRole('button', { name: 'Cancel' }).click();
    await expect(profileDelete.getByRole('button', { name: 'Delete' })).toBeFocused();
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
    await page.getByTestId('clone-profile-open').click();
    const cells = page.getByTestId('profile-editor').locator(':scope > div.grid');
    await expect(cells.nth(0).getByLabel('Name', { exact: true })).toBeVisible();
    await expect(cells.nth(1).getByLabel('Agent', { exact: true })).toBeVisible();
    const access = page.getByRole('group', { name: 'MCP access' });
    await expect(access.getByRole('radio', { name: 'none' })).toBeChecked();
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
