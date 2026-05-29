import { test, expect } from '../fixtures/weaver';

test.describe('session detail view', () => {
  test('renders goal, description, status and branch', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Render my details', name: 'detail-task' });

    await page.goto(`${weaver.baseUrl}/#/s/${s.id}`);

    await expect(page.getByRole('heading', { name: 'detail-task' })).toBeVisible();
    // Goal textarea is the first textarea on the page.
    await expect(page.locator('textarea').first()).toHaveValue('Render my details');
    // Metadata line includes id, branch and base branch.
    await expect(page.getByText(s.id, { exact: false })).toBeVisible();
    await expect(page.getByText(s.branch.branch, { exact: false })).toBeVisible();
    await expect(page.getByText(`base ${s.branch.base_branch}`, { exact: false })).toBeVisible();
    // Status badge is present in the header.
    await expect(page.getByTestId('status-badge').first()).toBeVisible();
  });

  test('editing the goal and saving persists across reload', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Original goal', name: 'edit-task' });

    await page.goto(`${weaver.baseUrl}/#/s/${s.id}`);

    const goalArea = page.locator('textarea').first();
    await expect(goalArea).toHaveValue('Original goal');
    await goalArea.fill('Updated goal text');
    await page.getByRole('button', { name: 'Save goal' }).click();
    await expect(page.getByText('Goal saved.')).toBeVisible();

    // Server-side state changed.
    const updated = await weaver.getSession(s.id);
    expect(updated.branch.goal).toBe('Updated goal text');

    // And it survives a full reload.
    await page.reload();
    await expect(page.locator('textarea').first()).toHaveValue('Updated goal text');
  });

  test('renders an interactive terminal that connects to the agent', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Receive a command', name: 'term-task' });

    await page.goto(`${weaver.baseUrl}/#/s/${s.id}`);

    // The xterm.js terminal mounts.
    await expect(page.locator('.xterm')).toBeVisible();
    await expect(page.locator('.xterm-screen')).toBeVisible();

    // It connects: the connection-state overlay (connecting/reconnecting/
    // disconnected) clears once the WebSocket reaches the PTY. This is
    // renderer-independent; the keystroke→PTY→output byte round-trip itself is
    // covered deterministically by the Rust integration test (WebGL draws to a
    // canvas, so asserting rendered text here would be renderer-dependent).
    await expect(page.getByTestId('term-status')).toHaveCount(0, { timeout: 20_000 });
  });
});
