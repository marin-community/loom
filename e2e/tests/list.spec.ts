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

  test('attention is its own chip, separate from the lifecycle axis', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Refactor auth', name: 'auth' });
    await weaver.setStatus(s, 'attention', 'ready for review');

    await page.goto(weaver.baseUrl);
    const card = page.locator(`[data-session-id="${s.id}"]`);
    // The agent's signal (attention) renders as its own deletable chip — never
    // merged into the lifecycle cell. The session is running, so the lifecycle
    // pill is omitted from the row (declutter), leaving the signal chip alone.
    await expect(
      card.locator('[data-testid="signal-chip"][data-signal-key="attention"]'),
    ).toHaveAttribute('data-level', 'attention');
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
    // No signal chip (an archived agent is gone); the lifecycle badge shows it.
    await expect(card.getByTestId('signal-chip')).toHaveCount(0);
    await expect(card.getByTestId('status-badge')).toHaveText(/archived/i);
    // The stale "Waiting for input" reason is suppressed…
    await expect(card).not.toContainText('Waiting for input');
    // …and it isn't counted among the sessions that need a human.
    await expect(page.getByTestId('filter-attention')).toContainText('0');
  });

  test('a watch triage mark is its own chip, attributed and clearable', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Looks stuck', name: 'watched' });
    // The agent itself is calm; a watch stamps a triage mark. It renders as
    // its own chip, attributed to the watch (⊙).
    await weaver.mark(s, 'blocked', { note: 'no progress in an hour', by: 'status-check' });

    await page.goto(weaver.baseUrl);
    const card = page.locator(`[data-session-id="${s.id}"]`);
    const chip = card.locator('[data-testid="signal-chip"][data-signal-key="triage"]');
    await expect(chip).toHaveAttribute('data-level', 'blocked');
    await expect(chip).toHaveAttribute('data-raised-by', 'watch');
    // It counts toward "needs attention" even though the agent is calm.
    await expect(page.getByTestId('filter-attention')).toContainText('1');

    // The human can resolve it by clearing the chip — no privileged "Mark OK"
    // path; the × DELETEs the `triage` tag the watch set.
    await chip.getByTestId('signal-chip-clear').click();
    await expect(chip).toHaveCount(0);
    const updated = await weaver.getSession(s.id);
    expect(updated.branch.tags.find((t) => t.key === 'triage')).toBeUndefined();
  });

  test('a resting agent shows a soothing idle mark, not a loud signal', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Resting', name: 'resting' });
    // The idle hook stamps the quiet `idle` mark when the agent goes quiet.
    await weaver.setTag(s, 'idle', 'idle');

    await page.goto(weaver.baseUrl);
    const card = page.locator(`[data-session-id="${s.id}"]`);
    // It renders as a calm, neutral idle chip — never a loud signal chip, and not
    // as a generic quiet pill (it's a lifecycle signal, surfaced soothingly).
    await expect(card.getByTestId('idle-chip')).toContainText(/idle/i);
    await expect(card.getByTestId('signal-chip')).toHaveCount(0);
    await expect(card.getByTestId('tag-pill')).toHaveCount(0);
    // A resting agent does not count toward "needs attention".
    await expect(page.getByTestId('filter-attention')).toContainText('0');

    // A loud signal supersedes the calm mark: once the agent raises attention,
    // the idle chip yields to the loud signal chip.
    await weaver.setStatus(s, 'attention', 'ready for review');
    await page.reload();
    await expect(card.getByTestId('idle-chip')).toHaveCount(0);
    await expect(
      card.locator('[data-testid="signal-chip"][data-signal-key="attention"]'),
    ).toHaveAttribute('data-level', 'attention');
  });

  test('a quiet free-form tag renders as a deletable pill', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Tag me', name: 'tagged' });
    await weaver.setTag(s, 'priority', 'high');

    await page.goto(weaver.baseUrl);
    const card = page.locator(`[data-session-id="${s.id}"]`);
    const pill = card.getByTestId('tag-pill');
    await expect(pill).toContainText('priority');
    await expect(pill).toContainText('high');
    // It's quiet — a free-form key never renders as a loud signal chip.
    await expect(card.getByTestId('signal-chip')).toHaveCount(0);

    // The × clears it server-side, and the pill goes away.
    await pill.getByTestId('tag-pill-clear').click();
    await expect(card.getByTestId('tag-pill')).toHaveCount(0);
    const updated = await weaver.getSession(s.id);
    expect(updated.branch.tags.find((t) => t.key === 'priority')).toBeUndefined();
  });

  test('a session awaiting external review sinks in the live list, not the shelf', async ({
    page,
    weaver,
  }) => {
    // Three sessions, created oldest→newest: one whose PR awaits an external
    // reviewer, a plainly-calm one, and one the agent raised. The review-wait
    // mark sinks the first below the calm rows but must NOT hide it on the shelf —
    // an open PR awaiting review is still yours to glance at.
    const review = await weaver.seedSession({ goal: 'Awaiting review', name: 'review-low' });
    const calm = await weaver.seedSession({ goal: 'Quietly working', name: 'calm-mid' });
    const attn = await weaver.seedSession({ goal: 'Needs a decision', name: 'top-attn' });
    await weaver.setTag(review, 'awaiting', 'review', {
      note: 'PR #7 review required — waiting on an external reviewer',
      by: 'review-wait',
    });
    await weaver.setStatus(attn, 'attention', 'which approach?');

    await page.goto(weaver.baseUrl);

    // All three stay live: the raised row floats to the top, the calm one next,
    // and the review-waiting row sinks to the bottom — sunk, but never hidden.
    const liveIds = await page
      .getByTestId('session-list')
      .getByTestId('session-card')
      .evaluateAll((els) => els.map((e) => e.getAttribute('data-session-id')));
    expect(liveIds).toEqual([attn.id, calm.id, review.id]);

    // The review-waiting row carries no loud signal (its mark is quiet) and does
    // not count toward "needs attention" — the user has no action there.
    const reviewCard = page
      .getByTestId('session-list')
      .locator(`[data-session-id="${review.id}"]`);
    await expect(reviewCard.getByTestId('signal-chip')).toHaveCount(0);
    await expect(page.getByTestId('filter-attention')).toContainText('1');
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
