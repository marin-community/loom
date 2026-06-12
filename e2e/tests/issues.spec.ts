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
});
