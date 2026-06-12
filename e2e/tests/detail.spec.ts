import { test, expect } from '../fixtures/weaver';

test.describe('session detail view', () => {
  test('renders goal, status and identity metadata', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Render my details', name: 'detail-task' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

    await expect(page.getByRole('heading', { name: 'detail-task' })).toBeVisible();
    // Running is the silent lifecycle default — the header shows no lifecycle
    // badge for it (only off-nominal states get one, as on the fleet list).
    await expect(page.getByTestId('status-badge')).toHaveCount(0);

    // The goal is the agent's launch prompt — read-only prose on the Overview
    // tab, not an editable field.
    await page.getByRole('button', { name: 'Overview' }).click();
    // Scope to the goal element — the goal text also appears in the tracking
    // issue's body in the issues panel, so a bare getByText is ambiguous.
    await expect(page.getByTestId('session-goal')).toHaveText('Render my details');

    // Identity metadata (id, branch, base) lives behind the ⌄ details popover,
    // not cluttering the header. Scope to the popover and match exactly so the
    // id doesn't also match the `weaver-<id>` tmux line.
    await page.getByRole('button', { name: 'details' }).click();
    const details = page.getByTestId('details-popover');
    await expect(details.getByText(s.id, { exact: true })).toBeVisible();
    await expect(details.getByText(s.branch.branch, { exact: true })).toBeVisible();
    await expect(details.getByText(`base ${s.branch.base_branch}`)).toBeVisible();
  });

  test('clearing the attention chip marks the agent’s attention calm', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Acknowledge me', name: 'ack-task' });
    // The agent raises its attention; the human's only write here is to clear it.
    await weaver.setStatus(s, 'attention', 'waiting on review');

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

    // Attention shows as a deletable signal chip — there is no separate "Mark OK"
    // control; the chip's × is the calm gesture.
    const chip = page.locator('[data-testid="signal-chip"][data-signal-key="attention"]');
    await expect(chip).toHaveAttribute('data-level', 'attention');

    await chip.getByTestId('signal-chip-clear').click();

    // The chip goes away…
    await expect(chip).toHaveCount(0);
    // …and it's cleared server-side: the × DELETEs the `attention` tag, so the
    // calm state is its absence (there is no stored `ok`).
    const updated = await weaver.getSession(s.id);
    expect(updated.branch.tags.find((t) => t.key === 'attention')).toBeUndefined();
  });

  test('renders an interactive terminal that connects to the agent', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Receive a command', name: 'term-task' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

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
