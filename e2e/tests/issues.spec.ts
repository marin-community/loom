import { test, expect } from '../fixtures/weaver';

test.describe('issues pane', () => {
  test('shows an empty state when there are no issues', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/issues`);
    await expect(page.getByRole('heading', { name: 'Issues' })).toBeVisible();
    await expect(page.getByTestId('issues-empty')).toBeVisible();
    await expect(page.getByTestId('issue-row')).toHaveCount(0);
  });

  test('renders a seeded issue with its tag pill and the session that references it', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: 'do the thing',
      name: 'feature',
    });
    const issue = await weaver.seedIssue(session, 'wire up the routes');
    await weaver.tagIssue(issue.id, 'priority', 'high');

    await page.goto(`${weaver.baseUrl}/issues`);
    const row = page.locator(`[data-issue-id="${issue.id}"]`);
    await expect(row).toBeVisible();
    await expect(row.getByTestId('issue-title')).toContainText('wire up the routes');
    await expect(row.getByTestId('issue-status')).toContainText('open');

    // The tag renders with the expected `key: value` pill.
    await expect(row.getByTestId('tag-pill')).toContainText('priority: high');

    // The claiming session resolves to a link back to its detail page.
    const ref = row.getByTestId('issue-session-ref');
    await expect(ref).toContainText('claimed: feature');
    await expect(ref).toHaveAttribute('href', `/s/${session.id}`);
  });

  test('closes an issue, hiding it until closed are shown', async ({ page, weaver }) => {
    const session = await weaver.seedSession({ goal: 'g', name: 'feature' });
    const issue = await weaver.seedIssue(session, 'closeable');

    await page.goto(`${weaver.baseUrl}/issues`);
    const row = page.locator(`[data-issue-id="${issue.id}"]`);
    await row.getByTestId('issue-close').click();

    // With "show closed" off, the closed issue drops out of the list. (A
    // seeded session also opens a tracking issue, so the global open count is
    // not asserted here — only that this issue left the open view.)
    await expect(row).toHaveCount(0);

    // Toggling closed back in surfaces it with a Reopen control, and the close
    // really persisted server-side.
    await page.getByTestId('issues-show-closed').check();
    await expect(row).toBeVisible();
    await expect(row.getByTestId('issue-status')).toContainText('closed');
    await expect(row.getByTestId('issue-reopen')).toBeVisible();
    const persisted = await weaver.listIssues(true);
    expect(persisted.find((i) => i.id === issue.id)?.status).toBe('closed');
  });

  test('keeps selection across filtering, pagination, and a kept-alive refresh', async ({
    page,
    weaver,
  }) => {
    const issues = [];
    for (let index = 0; index < 27; index++) {
      issues.push(
        await weaver.seedBacklogIssue(
          weaver.repoPath,
          `triage item ${String(index).padStart(2, '0')}`,
        ),
      );
    }

    await page.goto(`${weaver.baseUrl}/issues`);
    await expect(page.getByTestId('issues-pagination')).toBeVisible();
    await page.getByTestId('issues-select-visible').click();
    await expect(page.getByTestId('issues-selected-count')).toHaveText('25 selected');

    await page.getByTestId('issues-page-next').click();
    const secondPageRows = page.getByTestId('issue-row');
    await expect(secondPageRows).toHaveCount(2);
    await secondPageRows.first().getByTestId('issue-select').click();
    await expect(page.getByTestId('issues-selected-count')).toHaveText('26 selected');

    await page.getByTestId('issues-search').fill('triage item 26');
    await expect(page.getByTestId('issue-row')).toHaveCount(1);
    await expect(page.getByTestId('issues-selected-count')).toHaveText('26 selected');
    await page.getByTestId('issues-search').fill('');
    await expect(page.getByTestId('issues-selected-count')).toHaveText('26 selected');

    // The remaining matching item can be added explicitly across the whole
    // filtered result, not only the current page.
    await page.getByTestId('issues-select-matching').click();
    await expect(page.getByTestId('issues-selected-count')).toHaveText('27 selected');

    // Client-side navigation keeps Issues alive; activation performs a fresh
    // API load without discarding the ID-based selection.
    await page.getByRole('link', { name: 'loom home' }).click();
    await page.getByRole('link', { name: 'Issues' }).click();
    await expect(page.getByTestId('issues-selected-count')).toHaveText('27 selected');
    expect(issues).toHaveLength(27);
  });

  test('supports keyboard and shift-range additive selection', async ({ page, weaver }) => {
    const issues = [];
    for (let index = 0; index < 4; index++) {
      issues.push(await weaver.seedBacklogIssue(weaver.repoPath, `range item ${index}`));
    }

    await page.goto(`${weaver.baseUrl}/issues`);
    const first = page.locator(`[data-issue-id="${issues[0].id}"]`).getByTestId('issue-select');
    await first.focus();
    await page.keyboard.press('Space');
    await page
      .locator(`[data-issue-id="${issues[3].id}"]`)
      .getByTestId('issue-select')
      .click({ modifiers: ['Shift'] });
    await expect(page.getByTestId('issues-selected-count')).toHaveText('4 selected');

    await page.locator(`[data-issue-id="${issues[1].id}"]`).getByTestId('issue-select').click();
    await expect(page.getByTestId('issues-selected-count')).toHaveText('3 selected');
  });

  test('bulk closes selected issues atomically and refreshes once', async ({ page, weaver }) => {
    const first = await weaver.seedBacklogIssue(weaver.repoPath, 'bulk close first');
    const second = await weaver.seedBacklogIssue(weaver.repoPath, 'bulk close second');
    let issueRefreshes = 0;
    page.on('request', (request) => {
      if (request.method() === 'GET' && new URL(request.url()).pathname === '/api/issues') {
        issueRefreshes++;
      }
    });

    await page.goto(`${weaver.baseUrl}/issues`);
    await expect(page.getByTestId('issue-row')).toHaveCount(2);
    issueRefreshes = 0;
    await page.locator(`[data-issue-id="${first.id}"]`).getByTestId('issue-select').click();
    await page.locator(`[data-issue-id="${second.id}"]`).getByTestId('issue-select').click();
    await page.getByTestId('issues-bulk-close').click();

    await expect(page.getByTestId('issues-batch-feedback')).toContainText(
      'Close applied to 2 issues.',
    );
    expect(issueRefreshes).toBe(1);
    await expect(page.getByTestId('issue-row')).toHaveCount(0);
    const persisted = await weaver.listIssues(true);
    expect(persisted.find((issue) => issue.id === first.id)?.status).toBe('closed');
    expect(persisted.find((issue) => issue.id === second.id)?.status).toBe('closed');
  });

  test('cancels and then confirms an accessible bulk delete dialog', async ({ page, weaver }) => {
    const first = await weaver.seedBacklogIssue(weaver.repoPath, 'bulk delete first');
    const second = await weaver.seedBacklogIssue(weaver.repoPath, 'bulk delete second');

    await page.goto(`${weaver.baseUrl}/issues`);
    await page.getByTestId('issues-select-visible').click();
    const deleteButton = page.getByTestId('issues-bulk-delete');
    await deleteButton.click();
    const dialog = page.getByTestId('confirm-dialog');
    await expect(dialog).toContainText('Permanently delete 2 selected issues');
    await expect(dialog.getByTestId('confirm-dialog-cancel')).toBeFocused();
    await page.keyboard.press('Shift+Tab');
    await expect(dialog.getByTestId('confirm-dialog-confirm')).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(dialog).toHaveCount(0);
    await expect(deleteButton).toBeFocused();
    await expect(page.getByTestId('issue-row')).toHaveCount(2);

    await deleteButton.click();
    await page.getByTestId('confirm-dialog-confirm').click();
    await expect(page.getByTestId('issues-batch-feedback')).toContainText(
      'Delete applied to 2 issues.',
    );
    await expect(page.getByTestId('issue-row')).toHaveCount(0);
    const persisted = await weaver.listIssues(true);
    expect(persisted.some((issue) => issue.id === first.id || issue.id === second.id)).toBe(false);
  });

  test('shows structured atomic failure details and changes nothing', async ({ page, weaver }) => {
    const open = await weaver.seedBacklogIssue(weaver.repoPath, 'must stay open');
    const closed = await weaver.seedBacklogIssue(weaver.repoPath, 'already closed');
    const response = await page.request.patch(`${weaver.baseUrl}/api/issues/${closed.id}`, {
      data: { status: 'closed' },
    });
    expect(response.ok()).toBe(true);

    await page.goto(`${weaver.baseUrl}/issues`);
    await page.getByTestId('issues-show-closed').check();
    await page.locator(`[data-issue-id="${open.id}"]`).getByTestId('issue-select').click();
    await page.locator(`[data-issue-id="${closed.id}"]`).getByTestId('issue-select').click();
    await page.getByTestId('issues-bulk-close').click();

    const feedback = page.getByTestId('issues-batch-feedback');
    await expect(feedback).toHaveAttribute('role', 'alert');
    await expect(feedback).toContainText('No issues were changed');
    await expect(feedback).toContainText(`#${closed.id} — issue is already closed`);
    await expect(page.getByTestId('issues-selected-count')).toHaveText('2 selected');
    const persisted = await weaver.listIssues(true);
    expect(persisted.find((issue) => issue.id === open.id)?.status).toBe('open');
    expect(persisted.find((issue) => issue.id === closed.id)?.status).toBe('closed');

    await page.getByTestId('issues-remove-invalid').click();
    await expect(page.getByTestId('issues-selected-count')).toHaveText('1 selected');
    await page.getByTestId('issues-batch-retry').click();
    await expect(page.getByTestId('issues-batch-feedback')).toContainText(
      'Close applied to 1 issue.',
    );
    const retried = await weaver.listIssues(true);
    expect(retried.find((issue) => issue.id === open.id)?.status).toBe('closed');
  });

  test('honors and clears URL-backed repository and branch scope', async ({ page, weaver }) => {
    const firstSession = await weaver.seedSession({
      goal: 'one',
      name: 'scope-one',
    });
    const secondSession = await weaver.seedSession({
      goal: 'two',
      name: 'scope-two',
    });
    const first = await weaver.seedIssue(firstSession, 'first scoped issue');
    const second = await weaver.seedIssue(secondSession, 'second scoped issue');
    await weaver.seedBacklogIssue('/a/other-repo', 'outside the scoped repository');
    const query = new URLSearchParams({
      repo_root: firstSession.branch.repo_root,
      branch: firstSession.branch.branch,
    });

    await page.goto(`${weaver.baseUrl}/issues?${query}`);
    await expect(page.getByTestId('issues-active-scope')).toContainText('scope-one');
    await expect(page.getByTestId('issues-open-count')).toHaveText('2 open');
    await expect(page.locator(`[data-issue-id="${first.id}"]`)).toBeVisible();
    await expect(page.locator(`[data-issue-id="${second.id}"]`)).toHaveCount(0);

    await page.getByTestId('issue-create-toggle').click();
    await expect(page.getByTestId('issue-create-repo')).toHaveValue(firstSession.branch.repo_root);
    await page.getByTestId('issue-create-toggle').click();

    await page.locator(`[data-issue-id="${first.id}"]`).getByTestId('issue-select').click();
    await page.getByTestId('issues-clear-scope').click();
    await expect(page.getByTestId('issues-active-scope')).toHaveCount(0);
    await expect(page.getByTestId('issues-bulk-toolbar')).toHaveCount(0);
    await expect(page.locator(`[data-issue-id="${second.id}"]`)).toBeVisible();
  });

  test('edits an issue title through the inline editor', async ({ page, weaver }) => {
    const session = await weaver.seedSession({ goal: 'g', name: 'feature' });
    const issue = await weaver.seedIssue(session, 'old title');

    await page.goto(`${weaver.baseUrl}/issues`);
    const row = page.locator(`[data-issue-id="${issue.id}"]`);
    // Clicking the title opens the editor.
    await row.getByTestId('issue-title').click();
    await expect(row.getByTestId('issue-editor')).toBeVisible();

    await row.getByTestId('issue-edit-title').fill('new shiny title');
    await row.getByTestId('issue-save').click();

    await expect(row.getByTestId('issue-title')).toContainText('new shiny title');
    const persisted = await weaver.listIssues();
    expect(persisted.find((i) => i.id === issue.id)?.title).toBe('new shiny title');
  });

  test('returns a claimed issue to the backlog', async ({ page, weaver }) => {
    const session = await weaver.seedSession({ goal: 'g', name: 'feature' });
    const issue = await weaver.seedIssue(session, 'release me');

    await page.goto(`${weaver.baseUrl}/issues`);
    const row = page.locator(`[data-issue-id="${issue.id}"]`);
    await row.getByTestId('issue-unclaim').click();

    await expect(row.getByTestId('issue-unclaim')).toHaveCount(0);
    await expect(row.getByTestId('issue-launch')).toBeVisible();
    const persisted = (await weaver.listIssues()).find((candidate) => candidate.id === issue.id);
    expect(persisted?.claimed_branch).toBeNull();
  });

  test('changes and clears an issue GitHub mapping through the inline editor', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({ goal: 'g', name: 'feature' });
    const issue = await weaver.seedIssue(session, 'remappable');
    await page.goto(`${weaver.baseUrl}/issues`);
    const row = page.locator(`[data-issue-id="${issue.id}"]`);

    await row.getByTestId('issue-edit').click();
    await row.getByTestId('issue-edit-github').fill('acme/widgets#17');
    await row.getByTestId('issue-save').click();
    await expect(row.getByRole('link', { name: 'gh #17' })).toHaveAttribute(
      'href',
      'https://github.com/acme/widgets/issues/17',
    );

    await row.getByTestId('issue-edit').click();
    await row.getByTestId('issue-edit-github').fill('');
    await row.getByTestId('issue-save').click();
    await expect(row.getByRole('link', { name: 'gh #17' })).toHaveCount(0);

    const persisted = (await weaver.listIssues()).find((candidate) => candidate.id === issue.id)!;
    expect(persisted.github_repo).toBeNull();
    expect(persisted.github_issue).toBeNull();
  });

  test('adds a tag through the editor', async ({ page, weaver }) => {
    const session = await weaver.seedSession({ goal: 'g', name: 'feature' });
    const issue = await weaver.seedIssue(session, 'taggable');

    await page.goto(`${weaver.baseUrl}/issues`);
    const row = page.locator(`[data-issue-id="${issue.id}"]`);
    await row.getByTestId('issue-edit').click();

    await row.getByTestId('issue-tag-input').fill('area: ui');
    await row.getByTestId('issue-tag-add').click();

    // The tag renders both in the row's pill strip and inside the open editor;
    // assert on the editor's copy to stay unambiguous.
    await expect(row.getByTestId('issue-editor').getByTestId('tag-pill')).toContainText('area: ui');
    const persisted = await weaver.listIssues();
    const tags = persisted.find((i) => i.id === issue.id)?.tags ?? [];
    expect(tags.map((t) => `${t.key}=${t.value}`)).toContain('area=ui');
  });

  test('deletes an issue', async ({ page, weaver }) => {
    const session = await weaver.seedSession({ goal: 'g', name: 'feature' });
    const issue = await weaver.seedIssue(session, 'deletable');

    await page.goto(`${weaver.baseUrl}/issues`);
    const row = page.locator(`[data-issue-id="${issue.id}"]`);

    await row.getByTestId('issue-delete').click();
    const dialog = page.getByTestId('confirm-dialog');
    await expect(dialog).toContainText(`Delete issue #${issue.id}?`);
    await dialog.getByTestId('confirm-dialog-confirm').click();

    await expect(row).toHaveCount(0);
    const persisted = await weaver.listIssues(true);
    expect(persisted.find((i) => i.id === issue.id)).toBeUndefined();
  });

  test('creates a backlog issue with a tag through the New issue form', async ({
    page,
    weaver,
  }) => {
    // A seeded session puts exactly one repo on the board, so the form's repo
    // field is the static-label case and needs no selection.
    await weaver.seedSession({ goal: 'g', name: 'feature' });

    await page.goto(`${weaver.baseUrl}/issues`);
    await page.getByTestId('issue-create-toggle').click();
    const form = page.getByTestId('issue-create-form');
    await expect(form).toBeVisible();

    await form.getByTestId('issue-create-title').fill('add a settings page');
    await form.getByTestId('issue-create-body').fill('with a dark-mode toggle');

    // Stage a tag, which renders as a removable pill before the issue exists.
    await form.getByTestId('issue-create-tag-input').fill('priority: high');
    await form.getByTestId('issue-create-tag-add').click();
    await expect(form.getByTestId('tag-pill')).toContainText('priority: high');

    await form.getByTestId('issue-create-submit').click();

    // The form closes and the new row appears at the top with its tag pill.
    await expect(form).toBeHidden();
    const persisted = await weaver.listIssues();
    const created = persisted.find((i) => i.title === 'add a settings page');
    expect(created).toBeTruthy();
    expect(created?.body).toBe('with a dark-mode toggle');
    expect(created?.claimed_branch).toBeNull(); // an unclaimed backlog item
    expect((created?.tags ?? []).map((t) => `${t.key}=${t.value}`)).toContain('priority=high');

    const row = page.locator(`[data-issue-id="${created!.id}"]`);
    await expect(row.getByTestId('issue-title')).toContainText('add a settings page');
    await expect(row.getByTestId('tag-pill')).toContainText('priority: high');
  });

  test('launches a session from an unclaimed backlog issue', async ({ page, weaver }) => {
    // Seed a session to put one repo on the board, then file an *unclaimed*
    // backlog item in that same repo — the Launch button picks it up.
    const session = await weaver.seedSession({ goal: 'g', name: 'feature' });
    const backlog = await weaver.seedBacklogIssue(session.branch.repo_root, 'pick me up');

    await page.goto(`${weaver.baseUrl}/issues`);

    // The session's own tracking issue is already claimed, so it offers no
    // Launch button — only the unclaimed backlog item does.
    const claimed = (await weaver.listIssues()).find((i) => i.claimed_branch);
    await expect(
      page.locator(`[data-issue-id="${claimed!.id}"]`).getByTestId('issue-launch'),
    ).toHaveCount(0);

    const row = page.locator(`[data-issue-id="${backlog.id}"]`);
    await expect(row.getByTestId('issue-launch')).toBeVisible();
    await row.getByTestId('issue-launch').click();

    // Lands on the freshly-launched session's detail page…
    await page.waitForURL(/\/s\/[^/]+$/);

    // …and the backlog issue is now claimed by a branch (its new tracker).
    await expect
      .poll(async () => {
        const persisted = await weaver.listIssues();
        return persisted.find((i) => i.id === backlog.id)?.claimed_branch;
      })
      .not.toBeNull();
  });

  test('rejects an empty title and does not create', async ({ page, weaver }) => {
    await weaver.seedSession({ goal: 'g', name: 'feature' });

    await page.goto(`${weaver.baseUrl}/issues`);
    await page.getByTestId('issue-create-toggle').click();
    const form = page.getByTestId('issue-create-form');
    await form.getByTestId('issue-create-submit').click();

    await expect(form.getByTestId('issue-create-error')).toContainText('title is required');
    // No backlog issue was filed (the seeded session's tracking issue carries a
    // claimed branch, so a backlog item would be the only unclaimed one).
    const issues = await weaver.listIssues(true);
    expect(issues.some((i) => i.claimed_branch === null)).toBe(false);
  });

  test('files the first issue via the free-text repo field on an empty board', async ({
    page,
    weaver,
  }) => {
    // With no sessions or issues, the board knows of no repo, so the form offers
    // a free-text path instead of a picker.
    await page.goto(`${weaver.baseUrl}/issues`);
    await expect(page.getByTestId('issues-empty')).toBeVisible();

    await page.getByTestId('issue-create-toggle').click();
    const form = page.getByTestId('issue-create-form');
    const repo = form.getByTestId('issue-create-repo');
    await expect(repo).toBeVisible();
    await repo.fill(weaver.repoPath);
    await form.getByTestId('issue-create-title').fill('bootstrap the backlog');
    await form.getByTestId('issue-create-submit').click();

    await expect(form).toBeHidden();
    const persisted = await weaver.listIssues();
    const created = persisted.find((i) => i.title === 'bootstrap the backlog');
    expect(created).toBeTruthy();
    const row = page.locator(`[data-issue-id="${created!.id}"]`);
    await expect(row.getByTestId('issue-title')).toContainText('bootstrap the backlog');
  });
});
