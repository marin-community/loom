import { test, expect } from '../fixtures/weaver';

test.describe('session detail view', () => {
  test('renders goal, status and identity metadata', async ({ page, weaver }) => {
    const goal = Array.from(
      { length: 40 },
      (_, index) => `Private operator context line ${index + 1}`,
    ).join('\n');
    const s = await weaver.seedSession({ goal, name: 'detail-task' });
    await weaver.seedIssue(s, 'Scoped from Details');

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

    await expect(page.getByRole('heading', { name: 'detail-task' })).toBeVisible();
    // Running is the silent lifecycle default — the header shows no lifecycle
    // badge for it (only off-nominal states get one, as on the fleet list).
    await expect(page.getByTestId('status-badge')).toHaveCount(0);

    await expect(page.getByRole('button', { name: 'Overview' })).toHaveCount(0);

    // Identity and goal context live behind a keyboard-operable Details popover,
    // not in permanent header chrome.
    const trigger = page.getByRole('button', { name: /Details/ });
    await trigger.focus();
    await trigger.press('Enter');
    const details = page.getByTestId('details-popover');
    await expect(details.locator(':focus')).toHaveCount(1);
    await expect(details).toHaveAttribute('role', 'region');
    await expect(details).not.toHaveAttribute('aria-modal', 'true');
    await expect(details.getByText(s.id, { exact: true })).toBeVisible();
    await expect(details.getByText(s.branch.branch, { exact: true })).toBeVisible();
    await expect(details.getByText(`base ${s.branch.base_branch}`)).toBeVisible();
    await expect(details.getByTestId('session-goal-context')).toBeHidden();
    await details.getByText('Goal / prompt', { exact: true }).click();
    const goalContext = details.getByTestId('session-goal-context');
    await expect(goalContext).toHaveText(goal);
    await expect(goalContext).toHaveCSS('overflow-y', 'auto');
    const goalBounds = await goalContext.evaluate((element) => ({
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
    }));
    expect(goalBounds.scrollHeight).toBeGreaterThan(goalBounds.clientHeight);
    await expect(details.locator('a[href$="/artifacts/goal"]')).toHaveCount(0);

    // Details preserves a contextual Issues path, scoped by the literal branch
    // name (not the branch row id) for #630's claimed/source branch matching.
    const issuesLink = details.getByRole('link', { name: /\d+ open issues?/ });
    const issuesHref = await issuesLink.getAttribute('href');
    const issuesTarget = new URL(issuesHref!, weaver.baseUrl);
    expect(issuesTarget.pathname).toBe('/issues');
    expect(issuesTarget.searchParams.get('repo_root')).toBe(s.branch.repo_root);
    expect(issuesTarget.searchParams.get('branch')).toBe(s.branch.branch);

    // Escape dismisses the nonmodal popover and returns focus to its trigger.
    await page.keyboard.press('Escape');
    await expect(details).toHaveCount(0);
    await expect(trigger).toBeFocused();
  });

  test('outside dismissal preserves the focus target that was clicked', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: 'Dismiss lightly',
      name: 'outside-focus',
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole('button', { name: /Details/ }).click();
    await expect(page.getByTestId('details-popover')).toBeVisible();

    const agentTab = page.locator('[data-tab="terminal"]');
    await agentTab.click();
    await expect(page.getByTestId('details-popover')).toHaveCount(0);
    await expect(agentTab).toBeFocused();
  });

  test('resource navigation closes Details across cached session routes', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: 'Navigate cleanly',
      name: 'details-navigation',
    });
    await weaver.seedIssue(s, 'Scoped navigation');

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole('button', { name: /Details/ }).click();
    await page.getByTestId('details-popover').getByRole('link', { name: 'Artifacts' }).click();
    await expect(page).toHaveURL(new RegExp(`/s/${s.id}/artifacts`));
    await expect(page.getByTestId('details-popover')).toHaveCount(0);

    await page.locator('[data-tab="terminal"]').click();
    await page.getByRole('button', { name: /Details/ }).click();
    await page
      .getByTestId('details-popover')
      .getByRole('link', { name: /\d+ open issues?/ })
      .click();
    await expect(page).toHaveURL(/\/issues\?/);
    await expect(page.getByTestId('details-popover')).toHaveCount(0);

    await page.goBack();
    await expect(page).toHaveURL(`${weaver.baseUrl}/s/${s.id}`);
    await expect(page.getByTestId('details-popover')).toHaveCount(0);
  });

  test('Details preserves bounded, deduplicated surfaced links', async ({ page, weaver }) => {
    const s = await weaver.seedSession({
      goal: 'Keep operational links',
      name: 'surfaced-links',
    });
    for (let index = 0; index < 13; index += 1) {
      await weaver.setStatus(
        s,
        'attention',
        `Review https://example.test/doc/${index} before continuing`,
      );
    }
    await weaver.setStatus(s, 'attention', 'Latest: https://example.test/doc/12.');

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole('button', { name: /Details/ }).click();
    await page.getByText('Surfaced links (12)', { exact: true }).click();

    const links = page.getByTestId('session-links');
    await expect(links.getByRole('link')).toHaveCount(12);
    await expect(links.getByRole('link', { name: 'example.test/doc/12' })).toHaveCount(1);
    await expect(links.getByRole('link', { name: 'example.test/doc/12' })).toHaveAttribute(
      'href',
      'https://example.test/doc/12',
    );
    await expect(links.getByText('example.test/doc/0', { exact: true })).toHaveCount(0);
  });

  test('sets the browser tab title to the open session', async ({ page, weaver }) => {
    const s = await weaver.seedSession({
      goal: 'Name my tab',
      name: 'tab-task',
    });

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
    await page.route(`**/api/sessions/${s.id}`, async (route) => {
      if (!requestBody) return route.fallback();
      const response = await route.fetch();
      const current = (await response.json()) as typeof s;
      await route.fulfill({
        response,
        json: {
          ...current,
          github_repo: current.github_repo ?? 'acme/widgets',
          branch: {
            ...current.branch,
            github_pr: 37,
            github: {
              pr_number: 37,
              pr_url: 'https://github.com/acme/widgets/pull/37',
              pr_state: 'OPEN',
              checks: 'passing',
            },
          },
        },
      });
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    const prPill = page.getByTestId('pr-association-pill');
    const issuePill = page.getByTestId('issue-association-pill');
    await expect(page.getByTestId('github-associations')).toBeVisible();
    await expect(prPill).toHaveText('PR —');
    await expect(issuePill).toHaveText('Issue —');

    await prPill.click();
    const form = page.getByTestId('pr-mapping-form');
    await form.getByLabel('PR number').fill('37');
    await form.getByRole('button', { name: 'Pin PR' }).click();

    await expect.poll(() => requestBody).toEqual({ pr_number: 37 });
    await expect(form).toBeHidden();
    await expect(prPill).toHaveAttribute('href', 'https://github.com/acme/widgets/pull/37');
    await expect(prPill).toHaveAttribute('target', '_blank');

    // Reassociation is secondary, but its popover follows the same Escape /
    // focus-return contract as Details.
    const prEdit = page.getByTestId('pr-association-edit');
    await prEdit.click();
    await expect(page.getByTestId('pr-mapping-popover')).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('pr-mapping-popover')).toHaveCount(0);
    await expect(prEdit).toBeFocused();

    await issuePill.click();
    const issueForm = page.getByTestId('issue-mapping-form');
    await issueForm.getByLabel('owner/repo#number').fill('acme/widgets#73');
    await issueForm.getByRole('button', { name: 'Save' }).click();

    await expect(issuePill).toHaveText('Issue #73');
    await expect
      .poll(async () => (await weaver.getSession(s.id)).github_issue)
      .toEqual({
        repo: 'acme/widgets',
        number: 73,
      });
    await expect(issuePill).toHaveAttribute('href', 'https://github.com/acme/widgets/issues/73');

    await page.getByTestId('issue-association-edit').click();
    await page.getByTestId('issue-mapping-form').getByRole('button', { name: 'Clear' }).click();
    await expect(issuePill).toHaveText('Issue —');
  });

  test('clearing the attention chip marks the agent’s attention calm', async ({ page, weaver }) => {
    const s = await weaver.seedSession({
      goal: 'Acknowledge me',
      name: 'ack-task',
    });
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

  test('scratch attachments share one bounded browse and drop target', async ({ page, weaver }) => {
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

    // The same bounded component accepts a drop from files.length even when
    // DataTransfer.types does not advertise Files.
    const dataTransfer = await page.evaluateHandle(() => {
      const dt = new DataTransfer();
      dt.items.add(new File(['drop'], 'dropped.txt', { type: 'text/plain' }));
      Object.defineProperty(dt, 'types', { value: ['text/plain'] });
      return dt;
    });
    await page.getByTestId('scratch-dropzone').dispatchEvent('drop', { dataTransfer });
    await expect(panel.getByText('dropped.txt')).toBeVisible();

    // Both landed server-side in the worktree's scratch/.
    const res = await fetch(`${weaver.baseUrl}/api/sessions/${s.id}/scratch`);
    const listed = ((await res.json()) as { name: string }[]).map((f) => f.name).sort();
    expect(listed).toEqual(['dropped.txt', 'notes.txt']);

    // A chip's ✕ removes that file.
    await panel.getByRole('button', { name: 'Remove notes.txt' }).click();
    await expect(panel.getByText('notes.txt')).toHaveCount(0);
  });

  test('renders an interactive terminal that connects to the agent', async ({ page, weaver }) => {
    const s = await weaver.seedSession({
      goal: 'Receive a command',
      name: 'term-task',
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

    // The xterm.js terminal mounts.
    await expect(page.locator('.xterm')).toBeVisible();
    await expect(page.locator('.xterm-screen')).toBeVisible();

    // It connects: the connection-state overlay (connecting/reconnecting/
    // disconnected) clears once the WebSocket reaches the PTY. This is
    // renderer-independent; the keystroke→PTY→output byte round-trip itself is
    // covered deterministically by the Rust integration test (WebGL draws to a
    // canvas, so asserting rendered text here would be renderer-dependent).
    await expect(page.getByTestId('term-status')).toHaveCount(0, {
      timeout: 20_000,
    });
  });
});
