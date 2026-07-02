import { test, expect } from '../fixtures/weaver';

// The Watches panel is a master–detail split: every watch is a row on the
// left (active dot, name, program, outcome), and the selected watch's
// activity log, script source, and config live in the right pane.
test.describe('watch panel', () => {
  test('shows an empty state when there are no watches', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/watches`);
    await expect(page.getByRole('heading', { name: 'Watches' })).toBeVisible();
    await expect(page.getByTestId('watch-empty')).toBeVisible();
    await expect(page.getByTestId('watch-row')).toHaveCount(0);
  });

  test('lists a watch and auto-selects it into the detail pane', async ({
    page,
    weaver,
  }) => {
    await weaver.seedWatch({
      name: 'status-check',
      trigger: { cron: '0 * * * *' },
      scope: { attention: '!ok' },
      params: { prompt: 'flag stuck sessions' },
    });

    await page.goto(`${weaver.baseUrl}/watches`);
    const row = page.getByTestId('watch-row');
    await expect(row).toHaveCount(1);
    await expect(row).toContainText('status-check');
    await expect(row).toContainText('builtin:status');
    // Seeds start disabled → hollow dot.
    await expect(row.getByTestId('watch-active-dot')).toHaveAttribute('data-active', 'false');
    // Never run yet → the outcome badge reads "never run".
    await expect(row.getByTestId('outcome-badge')).toHaveAttribute('data-outcome', 'none');

    // With no explicit selection the first watch fills the detail pane, its
    // trigger and scope readable as chips.
    const detail = page.getByTestId('watch-detail');
    await expect(detail).toContainText('status-check');
    await expect(detail).toContainText('cron 0 * * * *');
    await expect(detail).toContainText('attention ≠ ok');
  });

  test('shows the builtin script source under the Script tab', async ({ page, weaver }) => {
    await weaver.seedWatch({ name: 'sourced' });
    await page.goto(`${weaver.baseUrl}/watches`);

    await page.getByTestId('watch-tab-script').click();
    const source = page.getByTestId('watch-script');
    await expect(source).toBeVisible();
    // The embedded builtin:status program is a real Python script.
    await expect(source).toContainText('def ');
  });

  test('creates a watch through the form in the detail pane', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/watches`);
    await page.getByTestId('watch-new').click();

    await page.getByTestId('watch-name').fill('form-made');
    await page.getByTestId('cap-nudge').check();
    await page.getByTestId('watch-create').click();

    const row = page.getByTestId('watch-row');
    await expect(row).toHaveCount(1);
    await expect(row).toContainText('form-made');
    // Created watches go live immediately.
    await expect(row.getByTestId('watch-active-dot')).toHaveAttribute('data-active', 'true');

    // It really persisted, with the ticked grant on top of the implicit observe.
    const all = (await (await fetch(`${weaver.baseUrl}/api/watches`)).json()) as {
      name: string;
      capabilities: string[];
    }[];
    expect(all.map((o) => o.name)).toContain('form-made');
    const made = all.find((o) => o.name === 'form-made')!;
    expect(made.capabilities).toContain('nudge');
    expect(made.capabilities).toContain('observe');
  });

  test('toggles enabled, dry-runs, and surfaces the outcome + the run log', async ({
    page,
    weaver,
  }) => {
    const o = await weaver.seedWatch({
      name: 'dry-runner',
      scope: { attention: '!ok' },
    });

    await page.goto(`${weaver.baseUrl}/watches`);
    const row = page.locator(`[data-watch-id="${o.id}"]`);
    await expect(row).toBeVisible();
    // The only watch is auto-selected into the detail pane.
    await expect(page.getByTestId('watch-title')).toHaveText('dry-runner');

    // Enable it (seeds start disabled). The toggle reflects the new state, and
    // the row's dot flips to active.
    const toggle = page.getByTestId('watch-enabled-toggle');
    await expect(toggle).toHaveAttribute('aria-checked', 'false');
    await toggle.click();
    await expect(toggle).toHaveAttribute('aria-checked', 'true');
    await expect(row.getByTestId('watch-active-dot')).toHaveAttribute('data-active', 'true');

    // Dry-run: with no sessions in scope the stock program reports a no-op.
    await page.getByTestId('watch-dryrun').click();
    const result = page.getByTestId('watch-run-result');
    await expect(result).toBeVisible();
    await expect(result).toContainText(/surveyed 0 sessions in scope/i);

    // The round lands in the activity log, marked as a dry run — and the fresh
    // round auto-expands its captured execution log.
    const runRows = page.getByTestId('watch-run-row');
    await expect(runRows.first()).toBeVisible();
    await expect(runRows.first()).toContainText(/surveyed 0 sessions in scope/i);
    await expect(runRows.first().getByTestId('outcome-badge')).toHaveAttribute(
      'data-outcome',
      'noop',
    );
    await expect(runRows.first()).toContainText(/run \(dry\)/i);
    const stdout = page.getByTestId('watch-run-stdout').first();
    await expect(stdout).toBeVisible();
    await expect(stdout).toContainText(/noop|surveyed 0/i);
  });

  test('edits the prompt and capabilities under the Config tab', async ({ page, weaver }) => {
    const o = await weaver.seedWatch({ name: 'editable', params: { prompt: 'original' } });

    await page.goto(`${weaver.baseUrl}/watches/${o.id}`);
    await page.getByTestId('watch-tab-config').click();
    await expect(page.getByTestId('watch-prompt')).toContainText('original');

    await page.getByTestId('watch-edit').click();
    await page.getByTestId('watch-prompt-input').fill('revised judgement');
    await page.getByTestId('cap-interrupt').check();
    await page.getByTestId('watch-save').click();

    await expect(page.getByTestId('watch-prompt')).toContainText('revised judgement');

    const fresh = (await (
      await fetch(`${weaver.baseUrl}/api/watches/${o.id}`)
    ).json()) as { params: { prompt?: string }; capabilities: string[] };
    expect(fresh.params.prompt).toBe('revised judgement');
    expect(fresh.capabilities).toContain('interrupt');
  });

  test('surfaces warm-session state under the Config tab', async ({ page, weaver }) => {
    // An ordinary watch runs each round fresh — no warm session.
    const fresh = await weaver.seedWatch({ name: 'fresh-runner' });
    await page.goto(`${weaver.baseUrl}/watches/${fresh.id}`);
    await page.getByTestId('watch-tab-config').click();
    await expect(page.getByTestId('watch-warm-off')).toBeVisible();
    await expect(page.getByTestId('watch-warm-terminal')).toHaveCount(0);

    // A warm watch that has not run yet shows the pending note (the engine
    // creates its persistent session on the next round).
    const warm = await weaver.seedWatch({ name: 'warm-runner', params: { warm: true } });
    await page.goto(`${weaver.baseUrl}/watches/${warm.id}`);
    await page.getByTestId('watch-tab-config').click();
    await expect(page.getByTestId('watch-warm-pending')).toBeVisible();
    await expect(page.getByTestId('watch-warm-off')).toHaveCount(0);
  });

  test('deletes a custom watch; builtin watches offer no delete', async ({ page, weaver }) => {
    // A builtin watch (the daemon-seeded shape: named after its program) can
    // only be disabled — the Config tab shows no delete button.
    const builtin = await weaver.seedWatch({ name: 'status', program: 'builtin:status' });
    await page.goto(`${weaver.baseUrl}/watches/${builtin.id}`);
    await page.getByTestId('watch-tab-config').click();
    await expect(page.getByTestId('watch-edit')).toBeVisible();
    await expect(page.getByTestId('watch-delete')).toHaveCount(0);

    // A custom-named watch deletes from the Config tab.
    const doomed = await weaver.seedWatch({ name: 'doomed' });
    await page.goto(`${weaver.baseUrl}/watches/${doomed.id}`);
    await page.getByTestId('watch-tab-config').click();
    page.on('dialog', (d) => d.accept());
    await page.getByTestId('watch-delete').click();

    // Selection falls back to the remaining watch.
    await expect(page).toHaveURL(new RegExp(`/watches/${builtin.id}$`));
    await expect(page.getByTestId('watch-row')).toHaveCount(1);
  });
});
