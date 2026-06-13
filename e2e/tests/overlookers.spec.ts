import { test, expect } from '../fixtures/weaver';

test.describe('overlooker panel', () => {
  test('shows an empty state when there are no overlookers', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/overlookers`);
    await expect(page.getByRole('heading', { name: 'Overlookers' })).toBeVisible();
    await expect(page.getByTestId('overlooker-empty')).toBeVisible();
    await expect(page.getByTestId('overlooker-row')).toHaveCount(0);
  });

  test('renders a seeded overlooker with its trigger, program and outcome', async ({
    page,
    weaver,
  }) => {
    await weaver.seedOverlooker({
      name: 'status-check',
      trigger: { cron: '0 * * * *' },
      scope: { attention: '!ok' },
      params: { prompt: 'flag stuck sessions' },
    });

    await page.goto(`${weaver.baseUrl}/overlookers`);
    const row = page.getByTestId('overlooker-row');
    await expect(row).toHaveCount(1);
    await expect(row).toContainText('status-check');
    await expect(row).toContainText('cron 0 * * * *');
    await expect(row).toContainText('builtin:status');
    // Never run yet → the outcome badge reads "never run".
    await expect(row.getByTestId('outcome-badge')).toHaveAttribute('data-outcome', 'none');
  });

  test('creates an overlooker through the inline form', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/overlookers`);
    await page.getByTestId('overlooker-new').click();

    await page.getByTestId('overlooker-name').fill('form-made');
    await page.getByTestId('cap-nudge').check();
    await page.getByTestId('overlooker-create').click();

    const row = page.getByTestId('overlooker-row');
    await expect(row).toHaveCount(1);
    await expect(row).toContainText('form-made');

    // It really persisted, with the ticked grant on top of the implicit observe.
    const all = (await (await fetch(`${weaver.baseUrl}/api/overlookers`)).json()) as {
      name: string;
      capabilities: string[];
    }[];
    expect(all.map((o) => o.name)).toContain('form-made');
    const made = all.find((o) => o.name === 'form-made')!;
    expect(made.capabilities).toContain('nudge');
    expect(made.capabilities).toContain('observe');
  });

  test('toggles enabled, dry-runs, and surfaces the outcome + a run in history', async ({
    page,
    weaver,
  }) => {
    const o = await weaver.seedOverlooker({
      name: 'dry-runner',
      scope: { attention: '!ok' },
    });

    await page.goto(`${weaver.baseUrl}/overlookers`);
    const row = page.locator(`[data-overlooker-id="${o.id}"]`);
    await expect(row).toBeVisible();

    // Enable it (seeds start disabled). The toggle reflects the new state.
    const toggle = row.getByTestId('overlooker-enabled-toggle');
    await expect(toggle).toHaveAttribute('aria-checked', 'false');
    await toggle.click();
    await expect(toggle).toHaveAttribute('aria-checked', 'true');

    // Dry-run: with no sessions in scope the stock program reports a no-op, and
    // the summary surfaces inline on the row.
    await row.getByTestId('overlooker-dryrun').click();
    const result = row.getByTestId('overlooker-run-result');
    await expect(result).toBeVisible();
    await expect(result).toContainText(/surveyed 0 sessions in scope/i);

    // The detail page renders the round in the audit history.
    await row.getByTestId('overlooker-name-link').click();
    await expect(page).toHaveURL(new RegExp(`/overlookers/${o.id}$`));
    await expect(page.getByTestId('overlooker-title')).toHaveText('dry-runner');

    const runRows = page.getByTestId('overlooker-run-row');
    await expect(runRows.first()).toBeVisible();
    await expect(runRows.first()).toContainText(/surveyed 0 sessions in scope/i);
    await expect(runRows.first().getByTestId('outcome-badge')).toHaveAttribute(
      'data-outcome',
      'noop',
    );
    // The trigger reason marks it as a dry run.
    await expect(runRows.first()).toContainText(/run \(dry\)/i);
  });

  test('expands a run to show its captured execution log', async ({ page, weaver }) => {
    const o = await weaver.seedOverlooker({ name: 'logged', scope: { attention: '!ok' } });
    await page.goto(`${weaver.baseUrl}/overlookers/${o.id}`);

    // A dry-run produces a round; with no sessions in scope the script prints a
    // noop result line — which the run row captures as its stdout.
    await page.getByTestId('overlooker-dryrun').click();
    const runRows = page.getByTestId('overlooker-run-row');
    await expect(runRows.first()).toBeVisible();

    // Click the row to expand its execution log; the captured stdout is shown.
    await runRows.first().getByTestId('overlooker-run-toggle').click();
    const stdout = page.getByTestId('overlooker-run-stdout').first();
    await expect(stdout).toBeVisible();
    await expect(stdout).toContainText(/noop|surveyed 0/i);
  });

  test('edits the prompt and capabilities from the detail page', async ({ page, weaver }) => {
    const o = await weaver.seedOverlooker({ name: 'editable', params: { prompt: 'original' } });

    await page.goto(`${weaver.baseUrl}/overlookers/${o.id}`);
    await expect(page.getByTestId('overlooker-prompt')).toContainText('original');

    await page.getByTestId('overlooker-edit').click();
    await page.getByTestId('overlooker-prompt-input').fill('revised judgement');
    await page.getByTestId('cap-interrupt').check();
    await page.getByTestId('overlooker-save').click();

    await expect(page.getByTestId('overlooker-prompt')).toContainText('revised judgement');

    const fresh = (await (
      await fetch(`${weaver.baseUrl}/api/overlookers/${o.id}`)
    ).json()) as { params: { prompt?: string }; capabilities: string[] };
    expect(fresh.params.prompt).toBe('revised judgement');
    expect(fresh.capabilities).toContain('interrupt');
  });

  test('surfaces warm-session state on the detail page', async ({ page, weaver }) => {
    // An ordinary overlooker runs each round fresh — no warm session.
    const fresh = await weaver.seedOverlooker({ name: 'fresh-runner' });
    await page.goto(`${weaver.baseUrl}/overlookers/${fresh.id}`);
    await expect(page.getByTestId('overlooker-warm-off')).toBeVisible();
    await expect(page.getByTestId('overlooker-warm-terminal')).toHaveCount(0);

    // A warm overlooker that has not run yet shows the pending note (the engine
    // creates its persistent session on the next round).
    const warm = await weaver.seedOverlooker({ name: 'warm-runner', params: { warm: true } });
    await page.goto(`${weaver.baseUrl}/overlookers/${warm.id}`);
    await expect(page.getByTestId('overlooker-warm-pending')).toBeVisible();
    await expect(page.getByTestId('overlooker-warm-off')).toHaveCount(0);
  });

  test('lists builtin programs with read-only source and prefills the form', async ({
    page,
    weaver,
  }) => {
    await page.goto(`${weaver.baseUrl}/overlookers`);

    // The registry section lists the stock programs that ship with loom.
    const rows = page.getByTestId('program-row');
    await expect(rows).toHaveCount(3);
    const section = page.getByTestId('builtin-programs');
    for (const name of ['builtin:status', 'builtin:pr-label', 'builtin:archive-merged']) {
      await expect(section).toContainText(name);
    }

    // Every builtin is a script whose source is viewable read-only.
    const archive = rows.filter({ hasText: 'builtin:archive-merged' });
    await archive.getByTestId('program-source-toggle').click();
    await expect(archive.getByTestId('program-source')).toContainText(
      'flag sessions whose pull request has merged',
    );
    const status = rows.filter({ hasText: 'builtin:status' });
    await status.getByTestId('program-source-toggle').click();
    await expect(status.getByTestId('program-source')).toContainText(
      'stamp a triage mark',
    );

    // "Use" opens the create form prefilled with the program and a name.
    await archive.getByTestId('program-use').click();
    await expect(page.getByTestId('overlooker-form')).toBeVisible();
    await expect(page.getByTestId('overlooker-program')).toHaveValue('builtin:archive-merged');
    await expect(page.getByTestId('overlooker-name')).toHaveValue('archive-merged');
    await page.getByTestId('overlooker-create').click();

    const row = page.getByTestId('overlooker-row');
    await expect(row).toHaveCount(1);
    await expect(row).toContainText('builtin:archive-merged');

    // The detail page renders the same script source, read-only.
    await row.getByTestId('overlooker-name-link').click();
    await page.getByTestId('program-source-toggle').click();
    await expect(page.getByTestId('program-source')).toContainText(
      'flag sessions whose pull request has merged',
    );
  });

  test('deletes an overlooker from the detail page', async ({ page, weaver }) => {
    const o = await weaver.seedOverlooker({ name: 'doomed' });

    await page.goto(`${weaver.baseUrl}/overlookers/${o.id}`);
    // Auto-accept the confirm() dialog.
    page.on('dialog', (d) => d.accept());
    await page.getByTestId('overlooker-delete').click();

    await expect(page).toHaveURL(/\/overlookers$/);
    await expect(page.getByTestId('overlooker-empty')).toBeVisible();
  });
});
