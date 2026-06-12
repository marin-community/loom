import { test, expect } from '../fixtures/weaver';

test.describe('status reflects hook and attention events', () => {
  test('detail view: hooks drive lifecycle + attention via SSE', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Watch my status', name: 'hook-detail' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    // While calm, the agent's state shows on the quiet conversation-state strip.
    const conv = page.getByTestId('conversation-state');
    await expect(conv).toBeVisible();

    // Any hook means the agent process is alive → lifecycle `running`, the
    // silent default: the header shows no lifecycle badge for it.
    await weaver.hook(s, 'working');
    await expect(page.getByTestId('status-badge')).toHaveCount(0);

    // A `waiting` hook (Claude blocked on the user) raises the attention signal,
    // which surfaces as a chip.
    await weaver.hook(s, 'waiting');
    await expect(
      page.locator('[data-testid="signal-chip"][data-signal-key="attention"]'),
    ).toHaveAttribute('data-level', 'attention');
  });

  test('detail view: weaver set-status sets level + message via SSE', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Declare my status', name: 'status-detail' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    const conv = page.getByTestId('conversation-state');
    await expect(conv).toBeVisible();

    await weaver.setStatus(s, 'blocked', 'tests failing, need help');
    await expect(
      page.locator('[data-testid="signal-chip"][data-signal-key="attention"]'),
    ).toHaveAttribute('data-level', 'blocked');
    await expect(page.getByTestId('status-message')).toHaveText(/tests failing, need help/i);
  });

  test('list view: attention filter narrows to sessions that need a human', async ({
    page,
    weaver,
  }) => {
    const fine = await weaver.seedSession({ goal: 'All good here', name: 'fine-one' });
    const stuck = await weaver.seedSession({ goal: 'Help needed', name: 'stuck-one' });

    await weaver.setStatus(stuck, 'attention', 'waiting on PR feedback');

    await page.goto(weaver.baseUrl);
    const stuckCard = page.locator(`[data-session-id="${stuck.id}"]`);
    const fineCard = page.locator(`[data-session-id="${fine.id}"]`);

    // The list polls every 3s; allow time for the attention to propagate.
    await expect(
      stuckCard.locator('[data-testid="signal-chip"][data-signal-key="attention"]'),
    ).toHaveAttribute('data-level', 'attention', { timeout: 10_000 });

    // Filtering to "needs attention" hides the OK session.
    await page.getByTestId('filter-attention').click();
    await expect(stuckCard).toBeVisible();
    await expect(fineCard).toHaveCount(0);
  });
});
