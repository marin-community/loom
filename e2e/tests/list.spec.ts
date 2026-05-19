import { test, expect } from '../fixtures/weaver';

test.describe('workspace list view', () => {
  test('shows an empty state when there are no workspaces', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);
    await expect(page.getByRole('heading', { name: 'Workspaces' })).toBeVisible();
    await expect(page.getByText('No workspaces yet.')).toBeVisible();
    await expect(page.getByTestId('workspace-card')).toHaveCount(0);
  });

  test('renders seeded workspaces with name, status and goal', async ({ page, weaver }) => {
    const a = await weaver.seedWorkspace({ goal: 'Add a health endpoint', name: 'alpha-task' });
    const b = await weaver.seedWorkspace({ goal: 'Fix the login bug', name: 'beta-task' });

    await page.goto(weaver.baseUrl);

    const cards = page.getByTestId('workspace-card');
    await expect(cards).toHaveCount(2);

    const cardA = page.locator(`[data-workspace-id="${a.id}"]`);
    await expect(cardA).toContainText('alpha-task');
    await expect(cardA).toContainText('Add a health endpoint');
    await expect(cardA.getByTestId('status-badge')).toBeVisible();

    const cardB = page.locator(`[data-workspace-id="${b.id}"]`);
    await expect(cardB).toContainText('beta-task');
    await expect(cardB).toContainText('Fix the login bug');
  });

  test('clicking a card navigates to the detail view', async ({ page, weaver }) => {
    const ws = await weaver.seedWorkspace({ goal: 'Navigate to me', name: 'nav-task' });

    await page.goto(weaver.baseUrl);
    await page.locator(`[data-workspace-id="${ws.id}"]`).click();

    await expect(page).toHaveURL(new RegExp(`#/w/${ws.id}$`));
    await expect(page.getByRole('heading', { name: 'nav-task' })).toBeVisible();
    await expect(page.locator('textarea').first()).toHaveValue('Navigate to me');
  });
});
