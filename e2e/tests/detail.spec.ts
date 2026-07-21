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

    // Identity metadata (id, branch, base) lives behind the ⋯ manage menu, under
    // the lifecycle actions, not cluttering the header. Scope to the popover and
    // match exactly so the id doesn't also match the `weaver-<id>` terminal line.
    await page.getByRole('button', { name: 'manage' }).click();
    const details = page.getByTestId('details-popover');
    await expect(details.getByText(s.id, { exact: true })).toBeVisible();
    await expect(details.getByText(s.branch.branch, { exact: true })).toBeVisible();
    await expect(details.getByText(`base ${s.branch.base_branch}`)).toBeVisible();
  });

  test('sets the browser tab title to the open session', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Name my tab', name: 'tab-task' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

    // The tab title tracks the open session (its title, falling back to the
    // branch name) so several loom tabs are tellable apart, composed centrally
    // as "Weaver - <Section>". It's derived from the shared fleet snapshot, which
    // the deep link fills a beat after landing, so toHaveTitle auto-retries until
    // the row arrives.
    await expect(page).toHaveTitle('Weaver - tab-task');

    // Leaving the session for the fleet list moves to the list's own section.
    await page.goto(`${weaver.baseUrl}/`);
    await expect(page).toHaveTitle('Weaver - Sessions');
  });

  test('edits pull request and issue associations from visible pills', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Map my PR', name: 'pr-map' });
    let requestBody: unknown;
    await page.route(`**/api/sessions/${s.id}/github`, async (route) => {
      if (route.request().method() !== 'PUT') return route.fallback();
      requestBody = route.request().postDataJSON();
      await route.fulfill({
        json: { ...s, branch: { ...s.branch, github_pr: 37 } },
      });
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    const prPill = page.getByTestId('pr-association-pill');
    const issuePill = page.getByTestId('issue-association-pill');
    await expect(prPill).toHaveText('PR —');
    await expect(issuePill).toHaveText('Issue —');

    await prPill.click();
    const form = page.getByTestId('pr-mapping-form');
    await form.getByLabel('PR number').fill('37');
    await form.getByRole('button', { name: 'Pin PR' }).click();

    await expect.poll(() => requestBody).toEqual({ pr_number: 37 });
    await expect(form).toBeHidden();

    await issuePill.click();
    const issueForm = page.getByTestId('issue-mapping-form');
    await issueForm.getByLabel('owner/repo#number').fill('acme/widgets#73');
    await issueForm.getByRole('button', { name: 'Save' }).click();

    await expect(issuePill).toHaveText('Issue #73');
    await expect.poll(async () => (await weaver.getSession(s.id)).github_issue).toEqual({
      repo: 'acme/widgets',
      number: 73,
    });

    await issuePill.click();
    await page.getByTestId('issue-mapping-form').getByRole('button', { name: 'Clear' }).click();
    await expect(issuePill).toHaveText('Issue —');
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

  test('scratch attachments ride the tab row and drop anywhere on the page', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: 'Hold my files', name: 'scratch-task' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    const panel = page.getByTestId('scratch-panel');
    await expect(panel.getByRole('button', { name: 'Attach' })).toBeVisible();

    // The Attach affordance drives a hidden file input.
    await panel.locator('input[type=file]').setInputFiles({
      name: 'notes.txt',
      mimeType: 'text/plain',
      buffer: Buffer.from('hello'),
    });
    await expect(panel.getByText('notes.txt')).toBeVisible();

    // Dragging a file over the window raises the full-page drop cue; dropping
    // uploads it. Synthesized events (Playwright can't drive native OS drag)
    // dispatched on body, bubbling up to the panel's window listeners — the
    // same path a real drag takes (its target is the element under the
    // cursor); the overlay assertions prove the listeners fired.
    const dataTransfer = await page.evaluateHandle(() => {
      const dt = new DataTransfer();
      dt.items.add(new File(['drop'], 'dropped.txt', { type: 'text/plain' }));
      return dt;
    });
    await page.dispatchEvent('body', 'dragenter', { dataTransfer });
    await expect(page.getByTestId('scratch-dropzone')).toBeVisible();
    await page.dispatchEvent('body', 'drop', { dataTransfer });
    await expect(page.getByTestId('scratch-dropzone')).toHaveCount(0);
    await expect(panel.getByText('dropped.txt')).toBeVisible();

    // Both landed server-side in the worktree's scratch/.
    const res = await fetch(`${weaver.baseUrl}/api/sessions/${s.id}/scratch`);
    const listed = ((await res.json()) as { name: string }[]).map((f) => f.name).sort();
    expect(listed).toEqual(['dropped.txt', 'notes.txt']);

    // A chip's ✕ removes that file.
    await panel.getByRole('button', { name: 'Remove notes.txt' }).click();
    await expect(panel.getByText('notes.txt')).toHaveCount(0);
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
