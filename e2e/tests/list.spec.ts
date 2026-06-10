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
    // A live session is `running`, and the list omits the lifecycle badge for
    // that state — nearly every row is running, so the pill would just be
    // repeated noise. Non-running states still show it (see the archived test).
    await expect(cardA.getByTestId('status-badge')).toHaveCount(0);

    const cardB = page.locator(`[data-session-id="${b.id}"]`);
    await expect(cardB).toContainText('beta-task');
    await expect(cardB).toContainText('Fix the login bug');
  });

  test('attention is its own badge, separate from the lifecycle axis', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Refactor auth', name: 'auth' });
    await weaver.setStatus(s, 'attention', 'ready for review');

    await page.goto(weaver.baseUrl);
    const card = page.locator(`[data-session-id="${s.id}"]`);
    // The agent's single signal (attention) is its own badge — never merged into
    // the lifecycle cell. The session is running, so the lifecycle pill is
    // omitted from the row (declutter), leaving attention to stand alone; the two
    // axes co-rendering as distinct badges is covered by the archived test below.
    await expect(card.getByTestId('attention-badge')).toHaveAttribute('data-level', 'attention');
    await expect(card.getByTestId('status-badge')).toHaveCount(0);
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

  test('an overlooker triage mark surfaces as the one resolved badge, attributed', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Looks stuck', name: 'watched' });
    // The agent itself is calm; an overlooker stamps a triage mark. The single
    // resolved badge raises to its level and attributes it to the overlooker.
    await weaver.mark(s, 'blocked', { note: 'no progress in an hour', by: 'status-check' });

    await page.goto(weaver.baseUrl);
    const card = page.locator(`[data-session-id="${s.id}"]`);
    const badge = card.getByTestId('attention-badge');
    await expect(badge).toHaveAttribute('data-level', 'blocked');
    await expect(badge).toHaveAttribute('data-raised-by', 'triage');
    // It counts toward "needs attention" even though the agent is calm.
    await expect(page.getByTestId('filter-attention')).toContainText('1');
  });

  test('a quiet free-form tag renders as a deletable pill', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Tag me', name: 'tagged' });
    await weaver.setTag(s, 'priority', 'high');

    await page.goto(weaver.baseUrl);
    const card = page.locator(`[data-session-id="${s.id}"]`);
    const pill = card.getByTestId('tag-pill');
    await expect(pill).toContainText('priority');
    await expect(pill).toContainText('high');
    // It's quiet — no loud attention treatment from a free-form key.
    await expect(card.getByTestId('attention-badge')).toHaveAttribute('data-level', 'ok');

    // The × clears it server-side, and the pill goes away.
    await pill.getByTestId('tag-pill-clear').click();
    await expect(card.getByTestId('tag-pill')).toHaveCount(0);
    const updated = await weaver.getSession(s.id);
    expect(updated.branch.tags.find((t) => t.key === 'priority')).toBeUndefined();
  });

  test('clicking a card navigates to the detail view', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Navigate to me', name: 'nav-task' });

    await page.goto(weaver.baseUrl);
    await page.locator(`[data-session-id="${s.id}"]`).click();

    await expect(page).toHaveURL(new RegExp(`/s/${s.id}$`));
    await expect(page.getByRole('heading', { name: 'nav-task' })).toBeVisible();
    // The goal is read-only prose on the Overview tab (agent-authored). Scope to
    // the goal element — the text also appears in the tracking issue's body.
    await page.getByRole('button', { name: 'Overview' }).click();
    await expect(page.getByTestId('session-goal')).toHaveText('Navigate to me');
  });
});
