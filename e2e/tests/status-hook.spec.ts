import { test, expect } from '../fixtures/weaver';

test.describe('status reflects hook and attention events', () => {
  test('detail view: hooks drive lifecycle + attention via SSE', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Watch my status', name: 'hook-detail' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    const status = page.getByTestId('status-badge').first();
    // The agent's attention is shown once, on the conversation-state strip.
    const conv = page.getByTestId('conversation-state');
    await expect(status).toBeVisible();

    // Any hook means the agent process is alive → lifecycle `running`.
    await weaver.hook(s, 'working');
    await expect(status).toHaveText(/running/i);

    // A `waiting` hook (Claude blocked on the user) raises the attention axis.
    await weaver.hook(s, 'waiting');
    await expect(conv).toHaveText(/needs attention/i);
  });

  test('detail view: weaver set-status sets level + message via SSE', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Declare my status', name: 'status-detail' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    const conv = page.getByTestId('conversation-state');
    await expect(conv).toBeVisible();

    await weaver.setStatus(s, 'blocked', 'tests failing, need help');
    await expect(conv).toHaveText(/blocked/i);
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
    await expect(stuckCard.getByTestId('attention-badge').first()).toHaveText(/attention/i, {
      timeout: 10_000,
    });

    // Filtering to "needs attention" hides the OK session.
    await page.getByTestId('filter-attention').click();
    await expect(stuckCard).toBeVisible();
    await expect(fineCard).toHaveCount(0);
  });
});
