import { test, expect } from '../fixtures/weaver';

test.describe('session list view', () => {
  test('shows an empty state when there are no sessions', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);
    await expect(page.getByRole('heading', { name: 'Sessions' })).toBeVisible();
    await expect(page.getByText('No sessions yet.')).toBeVisible();
    await expect(page.getByTestId('session-card')).toHaveCount(0);
  });

  test('renders seeded sessions with name, status and goal', async ({ page, weaver }) => {
    const a = await weaver.seedSession({ goal: 'Add a health endpoint', name: 'alpha-task' });
    const b = await weaver.seedSession({ goal: 'Fix the login bug', name: 'beta-task' });

    await page.goto(weaver.baseUrl);

    const cards = page.getByTestId('session-card');
    await expect(cards).toHaveCount(2);

    const cardA = page.locator(`[data-session-id="${a.id}"]`);
    await expect(cardA).toContainText('alpha-task');
    await expect(cardA).toContainText('Add a health endpoint');
    await expect(cardA.getByTestId('status-badge')).toBeVisible();

    const cardB = page.locator(`[data-session-id="${b.id}"]`);
    await expect(cardB).toContainText('beta-task');
    await expect(cardB).toContainText('Fix the login bug');
  });

  test('attention and lifecycle live in separate columns', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Refactor auth', name: 'auth' });
    await weaver.setStatus(s, 'attention', 'ready for review');

    await page.goto(weaver.baseUrl);
    const card = page.locator(`[data-session-id="${s.id}"]`);
    // The agent's single signal (attention) and the mechanical lifecycle are
    // each their own badge — not stacked in one cell.
    await expect(card.getByTestId('attention-badge')).toHaveAttribute('data-level', 'attention');
    await expect(card.getByTestId('status-badge')).toBeVisible();
    await expect(card).toContainText('ready for review');
  });

  test('an archived session stops asking for attention', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Old pass', name: 'old-pass' });
    // The agent had flagged it; then the user archives the workstream.
    await weaver.setStatus(s, 'attention', 'Waiting for input');
    await fetch(`${weaver.baseUrl}/api/sessions/${s.id}/archive`, { method: 'POST' });

    await page.goto(weaver.baseUrl);
    const card = page.locator(`[data-session-id="${s.id}"]`);
    await expect(card).toBeVisible();
    // Attention reads OK; the lifecycle badge shows it's archived.
    await expect(card.getByTestId('attention-badge')).toHaveAttribute('data-level', 'ok');
    await expect(card.getByTestId('status-badge')).toHaveText(/archived/i);
    // The stale "Waiting for input" reason is suppressed…
    await expect(card).not.toContainText('Waiting for input');
    // …and it isn't counted among the sessions that need a human.
    await expect(page.getByTestId('filter-attention')).toContainText('0');
  });

  test('clicking a card navigates to the detail view', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Navigate to me', name: 'nav-task' });

    await page.goto(weaver.baseUrl);
    await page.locator(`[data-session-id="${s.id}"]`).click();

    await expect(page).toHaveURL(new RegExp(`#/s/${s.id}$`));
    await expect(page.getByRole('heading', { name: 'nav-task' })).toBeVisible();
    await expect(page.locator('textarea').first()).toHaveValue('Navigate to me');
  });
});
