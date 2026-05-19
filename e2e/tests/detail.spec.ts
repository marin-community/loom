import { test, expect } from '../fixtures/weaver';

test.describe('workspace detail view', () => {
  test('renders goal, description, status and branch', async ({ page, weaver }) => {
    const ws = await weaver.seedWorkspace({ goal: 'Render my details', name: 'detail-task' });

    await page.goto(`${weaver.baseUrl}/#/w/${ws.id}`);

    await expect(page.getByRole('heading', { name: 'detail-task' })).toBeVisible();
    // Goal textarea is the first textarea on the page.
    await expect(page.locator('textarea').first()).toHaveValue('Render my details');
    // Metadata line includes id, branch and base branch.
    await expect(page.getByText(ws.id, { exact: false })).toBeVisible();
    await expect(page.getByText(ws.branch, { exact: false })).toBeVisible();
    await expect(page.getByText(`base ${ws.base_branch}`, { exact: false })).toBeVisible();
    // Status badge is present in the header.
    await expect(page.getByTestId('status-badge').first()).toBeVisible();
  });

  test('editing the goal and saving persists across reload', async ({ page, weaver }) => {
    const ws = await weaver.seedWorkspace({ goal: 'Original goal', name: 'edit-task' });

    await page.goto(`${weaver.baseUrl}/#/w/${ws.id}`);

    const goalArea = page.locator('textarea').first();
    await expect(goalArea).toHaveValue('Original goal');
    await goalArea.fill('Updated goal text');
    await page.getByRole('button', { name: 'Save goal' }).click();
    await expect(page.getByText('Goal saved.')).toBeVisible();

    // Server-side state changed.
    const updated = await weaver.getWorkspace(ws.id);
    expect(updated.goal).toBe('Updated goal text');

    // And it survives a full reload.
    await page.reload();
    await expect(page.locator('textarea').first()).toHaveValue('Updated goal text');
  });

  test('sending a line reaches the agent and shows on the live screen', async ({
    page,
    weaver,
  }) => {
    const ws = await weaver.seedWorkspace({ goal: 'Receive a command', name: 'send-task' });

    await page.goto(`${weaver.baseUrl}/#/w/${ws.id}`);

    const sendInput = page.getByPlaceholder('Send a line to the agent…');
    await expect(sendInput).toBeVisible();
    await sendInput.fill('echo E2E_MARKER_OK');
    await page.getByRole('button', { name: 'Send' }).click();

    // The shell echoes the marker; the live <pre> updates via SSE.
    await expect(page.locator('pre').first()).toContainText('E2E_MARKER_OK', {
      timeout: 20_000,
    });

    // Confirm independently via the pane API.
    const pane = await weaver.waitForPane(ws.id, 'E2E_MARKER_OK');
    expect(pane).toContain('E2E_MARKER_OK');
  });
});
