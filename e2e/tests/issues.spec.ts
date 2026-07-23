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
    const session = await weaver.seedSession({ goal: 'do the thing', name: 'feature' });
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

  test('changes and clears an issue GitHub mapping through the inline editor', async ({ page, weaver }) => {
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

    // The delete path guards behind a confirm() — accept it.
    page.on('dialog', (d) => d.accept());
    await row.getByTestId('issue-delete').click();

    await expect(row).toHaveCount(0);
    const persisted = await weaver.listIssues(true);
    expect(persisted.find((i) => i.id === issue.id)).toBeUndefined();
  });

  test('creates a backlog issue with a tag through the New issue form', async ({ page, weaver }) => {
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
